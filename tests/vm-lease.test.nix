# VM story: leases renew on boot with a stable owner-only machine identity
# and after activation, and an outage blocks neither boot nor activation.
args:
(import ./lib/vm.nix args).story "lease" {
  backend = "server";
  passthru.bloomery = [
    "CLIENT-INTEGRATION-VM-025"
    "CLIENT-INTEGRATION-VM-026"
    "CLIENT-INTEGRATION-VM-027"
  ];
  script = ''
    def leases():
        text = machine.succeed(f"cat {UPLOAD_STATE}/leases.log 2>/dev/null || true")
        return [line.split() for line in text.splitlines()]


    def wait_lease(slot, path, after=0):
        machine.wait_until_succeeds(
            f"tail -n +{after + 1} {UPLOAD_STATE}/leases.log | grep -q ' {slot} {path}$'", timeout=240
        )


    def current():
        return machine.succeed("readlink -f /run/current-system").strip()


    def lease_status():
        out = machine.succeed("systemctl show -p ExecMainStatus -p Result stockhouse-lease.service")
        return dict(line.split("=", 1) for line in out.split())


    with subtest("VM-025 leases renew on boot with a stable machine identity"):
        base = current()
        wait_lease(1, base)
        assert machine.succeed("stat -c '%U %a' /var/lib/stockhouse/machine-id").strip() == "stockhouse 600"
        identity = machine.succeed("cat /var/lib/stockhouse/machine-id").strip()
        assert leases()[0][0] == identity, leases()
        seen = len(leases())
        machine.shutdown()
        machine.start()
        machine.wait_for_unit("multi-user.target")
        wait_lease(1, base, after=seen)
        assert all(entry[0] == identity for entry in leases()), leases()
        assert machine.succeed("cat /var/lib/stockhouse/machine-id").strip() == identity

    with subtest("VM-026 leases renew after activation"):
        upgraded = machine.succeed("readlink -f /run/current-system/specialisation/upgraded").strip()
        machine.succeed(f"nix-env -p /nix/var/nix/profiles/system --set {base}")
        machine.succeed(f"nix-env -p /nix/var/nix/profiles/system --set {upgraded}")
        seen = len(leases())
        machine.succeed(f"{upgraded}/bin/switch-to-configuration test")
        assert current() == upgraded
        wait_lease(1, upgraded, after=seen)
        wait_lease(2, base, after=seen)
        # The renewal came from the switch timer the activation snippet restarted.
        machine.succeed("systemctl show -p LastTriggerUSec stockhouse-lease-switch.timer | grep -v '=n/a'")

    with subtest("VM-027 outages block neither boot nor activation"):
        control("down")
        machine.succeed("systemctl stop stockhouse-uploader")
        pending = build_expr(drv("outage"))
        machine.succeed("systemctl start stockhouse-uploader")
        machine.succeed(f"{base}/bin/switch-to-configuration test")
        assert current() == base
        machine.wait_until_succeeds(
            "systemctl show -p ExecMainStatus stockhouse-lease.service | grep -qx ExecMainStatus=75", timeout=120
        )
        assert lease_status() == {"ExecMainStatus": "75", "Result": "success"}, lease_status()
        machine.fail("systemctl is-failed stockhouse-lease.service")
        machine.shutdown()
        machine.start()
        machine.wait_for_unit("multi-user.target")
        machine.wait_until_succeeds(
            "systemctl show -p ExecMainStatus stockhouse-lease.service | grep -qx ExecMainStatus=75", timeout=240
        )
        assert lease_status()["Result"] == "success", lease_status()
        machine.succeed(f"test -f {QUEUE}/pending/{store_hash(pending)}.json")
  '';
}
