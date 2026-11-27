# VM story: the server backend uploads closures references first, skips
# paths the server already holds or the ledger records, and rejects a NAR
# the server fails to verify, all against the local fake server.
args:
(import ./lib/vm.nix args).story "upload" {
  backend = "server";
  fakeProtocol = "v1";
  passthru.bloomery = [
    "CLIENT-INTEGRATION-VM-029"
    "CLIENT-INTEGRATION-VM-019"
    "CLIENT-INTEGRATION-VM-020"
    "CLIENT-INTEGRATION-VM-021"
  ];
  script = ''
    def references(path):
        return machine.succeed(f"nix-store -q --references {path}").split()


    def index(lines, needle):
        return next(i for i, line in enumerate(lines) if needle in line)


    with subtest("VM-019 a closure uploads references first"):
        machine.systemctl("stop stockhouse-uploader")
        b = build_expr(f"let a = {drv('ra')}; in {drv('rb', ['a'])}")
        [a] = references(b)
        # Only the referrer is queued, so the reference is reached through its closure.
        machine.succeed(f"rm {QUEUE}/pending/{store_hash(a)}.json")
        machine.systemctl("start stockhouse-uploader")
        wait_uploaded(b)
        log = fake_log()
        assert index(log, f" {store_hash(a)} open") < index(log, f" {store_hash(b)} open"), log
        assert not any("missing_references" in line for line in log), log
        state = json.loads(machine.succeed(f"cat {UPLOAD_STATE}/state.json"))
        decl = next(u["decl"] for u in state["uploads"].values() if u["decl"]["store_path"] == b)
        assert a.split("/")[-1] in decl["references"], decl
        assert any(line.endswith(f"{store_hash(a)} published") for line in log), log

    with subtest("VM-020 held and ledgered paths are not sent again"):
        machine.systemctl("stop stockhouse-uploader")
        c = build_expr(drv("pc"))
        control(f"present-{store_hash(c)}")
        machine.succeed(f"rm {QUEUE}/pending/{store_hash(c)}.json")
        before_a = len(fake_lines(store_hash(a)))
        d = build_expr(f"let a = {drv('ra')}; c = {drv('pc')}; in {drv('pd', ['a', 'c'])}")
        machine.systemctl("start stockhouse-uploader")
        wait_uploaded(d)
        c_lines = fake_lines(store_hash(c))
        assert c_lines and all(line.startswith("POST") and line.endswith("present") for line in c_lines), c_lines
        assert len(fake_lines(store_hash(a))) == before_a, fake_lines(store_hash(a))
        assert fake_lines(store_hash(d))[-1].endswith("published")

    with subtest("VM-021 a verification failure rejects the record"):
        machine.systemctl("stop stockhouse-uploader")
        e = build_expr(drv("re"))
        control(f"reject-{store_hash(e)}")
        machine.systemctl("start stockhouse-uploader")
        machine.wait_until_succeeds(f"test -f {QUEUE}/failed/{store_hash(e)}.json", timeout=120)
        failed = json.loads(machine.succeed(f"cat {QUEUE}/failed/{store_hash(e)}.json"))
        assert "verification_failed" in failed["reason"], failed
    assert_v1_fallback()
  '';
}
