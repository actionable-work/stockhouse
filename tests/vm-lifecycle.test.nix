# VM story: queue overflow is visible and reconciled by rescan, and a
# configuration upgrade and rollback apply to the running services.
args:
(import ./lib/vm.nix args).story "lifecycle" {
  passthru.bloomery = [
    "CLIENT-INTEGRATION-VM-007"
    "CLIENT-INTEGRATION-VM-009"
    "CLIENT-INTEGRATION-VM-012"
  ];
  script = ''
    with subtest("VM-007 overflow is visible in status"):
        wait_drained()
        machine.systemctl("stop stockhouse-uploader")
        burst = [build(f"burst{i}") for i in range(5)]
        snapshot = status()
        assert snapshot["pending"] == 3, snapshot
        assert snapshot["overflow"]["active"] and snapshot["overflow"]["dropped"] >= 2, snapshot

    with subtest("VM-012 overflow is reconciled by rescan"):
        machine.systemctl("start stockhouse-uploader")
        for path in burst:
            wait_uploaded(path)
        machine.wait_until_succeeds(
            f"stockhouse status --queue {QUEUE} | grep -q '\"active\":false'", timeout=120
        )

    with subtest("VM-009 configuration upgrade and rollback apply"):
        base = machine.succeed("readlink -f /run/current-system").strip()
        machine.succeed(f"{base}/specialisation/upgraded/bin/switch-to-configuration test")
        machine.wait_until_succeeds(f"curl -s {PROXY}/nix-cache-info | grep -q 'Priority: 45'", timeout=60)
        status()
        machine.succeed(f"{base}/bin/switch-to-configuration test")
        machine.wait_until_succeeds(f"curl -s {PROXY}/nix-cache-info | grep -q 'Priority: 30'", timeout=60)
        status()
  '';
}
