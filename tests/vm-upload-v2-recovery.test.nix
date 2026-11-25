# VM story for recovery after a lost publish, restart and Retry-After.
args:
(import ./lib/vm.nix args).story "upload-v2-recovery" {
  backend = "server";
  passthru.bloomery = [
    "CLIENT-INTEGRATION-VM-033"
    "CLIENT-INTEGRATION-VM-034"
    "CLIENT-INTEGRATION-VM-035"
  ];
  script = ''
    machine.systemctl("stop stockhouse-lease.timer stockhouse-lease-daily.timer stockhouse-lease-switch.timer")
    machine.wait_for_open_port(8486)


    def done_id(path):
        return json.loads(machine.succeed(f"cat {QUEUE}/done/{store_hash(path)}.json"))["id"]


    def queued(tag):
        machine.systemctl("stop stockhouse-uploader")
        path = build_expr(drv(tag))
        return path, store_hash(path), pending_record(path)["id"]


    with subtest("VM-033 lost publish replies reconcile without resending any NAR"):
        machine.systemctl("stop stockhouse-uploader")
        root = build_expr(f"let a = {drv('lost-publish-dep')}; in {drv('lost-publish-root', ['a'])}")
        [dep] = machine.succeed(f"nix-store -q --references {root}").split()
        identities = {path: pending_record(path)["id"] for path in [root, dep]}
        control("drop-publish")
        start = len(fake_log())
        machine.systemctl("start stockhouse-uploader")
        machine.wait_until_succeeds(f"grep -q 'POST v2 publish: request failed' {QUEUE}/pending/{store_hash(root)}.json", timeout=120)
        assert pending_record(root)["attempts"] == 1
        control("drop-publish", on=False)
        wait_uploaded(root)
        wait_drained()
        lines = fake_log()[start:]
        publishes = [line for line in lines if line.startswith("POST v2/publish ")]
        assert publishes == ["POST v2/publish - entries=2"] * 3, lines
        for path, upload_id in identities.items():
            assert done_id(path) == upload_id
            assert [line for line in lines if line.startswith(f"PUT v2/nar/{upload_id} ")] == [
                f"PUT v2/nar/{upload_id} {store_hash(path)} verified"
            ], lines
            assert f"ENTRY v2/status {upload_id} {store_hash(path)} published" in lines


    with subtest("VM-034 the restarted uploader reconciles the stalled identity first"):
        root, h, upload_id = queued("stall-v2")
        control("trace-nar-start")
        control(f"stall-{h}")
        machine.systemctl("start stockhouse-uploader")
        machine.wait_until_succeeds(f"grep -q 'PUT v2/nar/{upload_id} {h} stalled' {UPLOAD_STATE}/requests.log", timeout=120)
        machine.systemctl("stop stockhouse-uploader")
        control(f"stall-{h}", on=False)
        machine.wait_until_succeeds(
            f"grep -Eq '^PUT v2/nar/{upload_id} {h} (aborted|verified)$' {UPLOAD_STATE}/requests.log",
            timeout=120,
        )
        machine.systemctl("start stockhouse-uploader")
        wait_uploaded(root)
        assert done_id(root) == upload_id
        lines = fake_lines(h)
        reads = [i for i, line in enumerate(lines) if line.startswith(f"ENTRY v2/status {upload_id} ")]
        transfers = [i for i, line in enumerate(lines) if line == f"START v2/nar/{upload_id} {h} reading"]
        assert reads and transfers, lines
        assert transfers[0] < reads[0] and all(reads[0] < index for index in transfers[1:]), lines
        state = lines[reads[0]].split()[-1]
        assert state in ("open", "verified"), lines
        assert len(transfers) == (2 if state == "open" else 1), lines
        assert all(upload_id in line for line in lines if line.startswith(("ENTRY", "PUT", "START"))), lines
        control("trace-nar-start", on=False)


    with subtest("VM-035 the batch resends after Retry-After without consuming an attempt"):
        root, h, upload_id = queued("busy-v2")
        control("trace-busy")
        control("busy-once")
        control(f"stall-{h}")
        machine.systemctl("start stockhouse-uploader")
        machine.wait_until_succeeds(f"grep -q 'PUT v2/nar/{upload_id} {h} stalled' {UPLOAD_STATE}/requests.log", timeout=120)
        assert pending_record(root)["attempts"] == 0
        attempts = machine.succeed(f"grep '^v2/declare ' {UPLOAD_STATE}/busy.log").splitlines()
        assert len(attempts) == 2, attempts
        first, second = [line.split() for line in attempts]
        assert first[2] == second[2], attempts
        assert 0.95 <= float(second[1]) - float(first[1]) <= 5, attempts
        control(f"stall-{h}", on=False)
        control("trace-busy", on=False)
        wait_uploaded(root)
        assert done_id(root) == upload_id
  '';
}
