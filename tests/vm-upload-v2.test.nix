# VM story for batched uploads, lease rebinding and rollback to v1.
args:
(import ./lib/vm.nix args).story "upload-v2" {
  backend = "server";
  passthru.bloomery = [
    "CLIENT-INTEGRATION-VM-030"
    "CLIENT-INTEGRATION-VM-031"
    "CLIENT-INTEGRATION-VM-032"
    "CLIENT-PROTO-BATCH-022"
    "CLIENT-PROTO-LEASE-010"
  ];
  script = ''
    machine.systemctl("stop stockhouse-lease.timer stockhouse-lease-daily.timer stockhouse-lease-switch.timer")
    machine.wait_for_open_port(8486)


    def fake_state():
        return json.loads(machine.succeed(f"cat {UPLOAD_STATE}/state.json"))


    def sections(lines, method, route):
        return [line for line in lines if line.startswith(f"{method} {route} ")]


    def lease_run():
        machine.systemctl("start stockhouse-lease.service")
        assert machine.succeed("systemctl show -p ExecMainStatus --value stockhouse-lease.service").strip() == "0"
        return fake_state()["leases"]


    with subtest("VM-030 one declaration and publish cover twenty store paths"):
        machine.systemctl("stop stockhouse-uploader")
        paths = {f"d{i}": drv(f"batch-{i}", [f"d{i - 1}"] if i % 3 == 1 else []) for i in range(19)}
        definitions = " ".join(f"{name} = {definition};" for name, definition in paths.items())
        root = build_expr(f"let {definitions} in {drv('batch-root', paths)}")
        closure = machine.succeed(f"nix-store -qR {root}").split()
        assert len(closure) == 20, closure
        held = next(path for path in closure if path.endswith("-vm-batch-0"))
        control(f"present-{store_hash(held)}")
        control("delay-nars")
        machine.succeed(f"find {QUEUE}/pending -type f ! -name {store_hash(root)}.json -delete")
        start = len(fake_log())
        machine.systemctl("start stockhouse-uploader")
        wait_uploaded(root)
        wait_drained()
        control("delay-nars", on=False)
        lines = fake_log()[start:]
        assert sections(lines, "GET", "protocol") == ["GET protocol - 200"], lines
        assert sections(lines, "POST", "v2/declare") == ["POST v2/declare - entries=20"], lines
        assert sections(lines, "POST", "v2/publish") == ["POST v2/publish - entries=19"], lines
        nars = [line for line in lines if line.startswith("PUT v2/nar/")]
        assert len(nars) == 19 and all(line.endswith(" verified") for line in nars), lines
        assert {line.split()[2] for line in nars} == {store_hash(path) for path in closure if path != held}, lines
        assert 1 < int(machine.succeed(f"cat {UPLOAD_STATE}/peak.log")) <= 4
        assert not any(line.split()[1].startswith("uploads/") for line in lines), lines


    with subtest("VM-031 and LEASE-010 renew two slots and restore a prior binding"):
        base = machine.succeed("readlink -f /run/current-system").strip()
        alternate = machine.succeed("readlink -f /run/current-system/specialisation/upgraded").strip()
        machine.succeed(f"nix-env -p /nix/var/nix/profiles/system --set {alternate}")
        machine.succeed(f"nix-env -p /nix/var/nix/profiles/system --set {base}")
        start = len(fake_log())
        try:
            initial = lease_run()
            machine.succeed(f"ln -sfn {alternate} /run/current-system")
            changed = lease_run()
            machine.succeed(f"ln -sfn {base} /run/current-system")
            rolled_back = lease_run()
            renewed_again = lease_run()
        finally:
            machine.succeed(f"ln -sfn {base} /run/current-system")
        machine_id = machine.succeed("cat /var/lib/stockhouse/machine-id").strip()
        first, second = f"{machine_id}/1", f"{machine_id}/2"
        assert set(rolled_back) == {first, second}, rolled_back
        assert initial[first]["store_path"] == rolled_back[first]["store_path"] == base
        assert changed[first]["store_path"] == rolled_back[second]["store_path"] == alternate
        assert changed[second]["store_path"] == base
        assert renewed_again[first]["store_path"] == base
        assert set(renewed_again) == {first, second}, renewed_again
        assert initial[first]["expires_at"] < changed[first]["expires_at"] < rolled_back[first]["expires_at"] < renewed_again[first]["expires_at"]
        assert sections(fake_log()[start:], "POST", "v2/leases") == ["POST v2/leases - entries=2"] * 4


    with subtest("BATCH-022 retained verification cannot block a dependency"):
        machine.systemctl("stop stockhouse-uploader")
        extra = build("unrelated-staged")
        extra_id = pending_record(extra)["id"]
        control("hold-publish")
        machine.systemctl("start stockhouse-uploader")
        machine.wait_until_succeeds(f"grep -q 'PUT v2/nar/{extra_id} .* verified' {UPLOAD_STATE}/requests.log", timeout=120)
        machine.systemctl("stop stockhouse-uploader")
        control("hold-publish", on=False)
        machine.succeed(f"rm {QUEUE}/pending/{store_hash(extra)}.json")
        large_drv = drv("large-dependency").replace(
            "echo large-dependency  > $out",
            "i=0; while test $i -lt 65536; do printf %0192d 0; i=$((i+1)); done > $out",
        )
        referrer = build_expr(f"let a = {large_drv}; in {drv('small-referrer', ['a'])}")
        [dep] = machine.succeed(f"nix-store -q --references {referrer}").split()
        dep_hash, ref_hash = store_hash(dep), store_hash(referrer)
        machine.succeed(f"rm {QUEUE}/pending/{dep_hash}.json")
        state = fake_state()
        already_published = sum(decl["nar_size"] for decl in state["held"].values())
        sizes = [int(machine.succeed(f"nix-store -q --size {path}")) for path in (dep, referrer)]
        assert sizes[0] > 12 * 1024 * 1024 and sizes[0] > 100 * sizes[1], sizes
        quota = already_published + sum(sizes)
        machine.succeed(f"echo {quota} > {UPLOAD_STATE}/control/quota-bytes")
        control(f"stall-{dep_hash}")
        machine.systemctl("start stockhouse-uploader")
        machine.wait_until_succeeds(f"grep -q 'PUT v2/nar/.* {ref_hash} verified' {UPLOAD_STATE}/requests.log", timeout=120)
        staged = fake_state()
        assert staged["uploads"][extra_id]["state"] == "verified"
        assert sum(upload["decl"]["nar_size"] for upload in staged["uploads"].values() if upload["state"] == "verified") > sizes[1]
        control(f"stall-{dep_hash}", on=False)
        wait_uploaded(referrer)
        lines = fake_log()
        dep_verified = next(i for i, line in enumerate(lines) if line.endswith(f"{dep_hash} verified"))
        ref_verified = next(i for i, line in enumerate(lines) if line.endswith(f"{ref_hash} verified"))
        assert ref_verified < dep_verified, lines
        published = [line.split()[3] for line in sections(lines, "ENTRY", "v2/publish") if line.endswith(" published")]
        assert published[-2:] == [dep_hash, ref_hash], lines
        control("quota-bytes", on=False)


    with subtest("VM-032 rollback causes a new probe and v1 upload"):
        start = len(fake_log())
        machine.succeed("systemctl kill --signal=STOP --kill-whom=main stockhouse-uploader.service")
        try:
            control("v1-only")
            root = build("after-protocol-rollback")
            old = pending_record(root)["id"]
        finally:
            machine.succeed("systemctl kill --signal=CONT --kill-whom=main stockhouse-uploader.service")
        h = store_hash(root)
        wait_uploaded(root)
        lines = fake_log()[start:]
        assert lines.index("POST v2/declare - 404") < lines.index("GET protocol - 404"), lines
        assert f"PUT uploads/{old}/nar {h} published" in lines, lines
        assert status()["failed"] == 0
  '';
}
