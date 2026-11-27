# VM story: the installed proxy listens on loopback only, keeps its credential
# out of every visible place, and never falls through on denial or outage.
args:
(import ./lib/vm.nix args).story "proxy" {
  passthru.bloomery = [
    "CLIENT-INTEGRATION-VM-001"
    "CLIENT-INTEGRATION-VM-008"
    "CLIENT-INTEGRATION-VM-010"
    "CLIENT-INTEGRATION-VM-011"
  ];
  script = ''
    with subtest("VM-001 the proxy listens only on loopback"):
        listeners = [line for line in machine.succeed("ss -Htln").splitlines() if ":8484" in line]
        assert len(listeners) == 1 and "127.0.0.1:8484" in listeners[0], listeners
        assert http_code("/nix-cache-info") == "200"

    with subtest("VM-008 credentials stay out of arguments, units and configuration"):
        token = machine.succeed("cat /var/lib/stockhouse-vm/shared-token").strip()
        assert len(token) >= 16
        assert http_code("/0000000000000000000000000000000a.narinfo") == "404"
        requests = machine.succeed("cat /var/lib/stockhouse-vm/shared-requests.log")
        assert f"Bearer {token}" in requests, requests
        pid = machine.succeed("systemctl show -p MainPID --value stockhouse-proxy").strip()
        cmdline = machine.succeed(f"tr '\\0' ' ' < /proc/{pid}/cmdline")
        unit = machine.succeed("systemctl cat stockhouse-proxy")
        config = machine.succeed("cat " + cmdline.split()[-1])
        for text in (cmdline, unit, config, machine.succeed("journalctl -u stockhouse-proxy")):
            assert token not in text
        assert 'systemd = "shared"' in config, config
        substituters = machine.succeed("nix --extra-experimental-features nix-command config show substituters").strip()
        assert substituters == PROXY, substituters

    with subtest("VM-010 the client and daemon use the default Nix package"):
        nix_bin = "${args.pkgs.nix.out}/bin"
        daemon_pid = machine.succeed("systemctl show -p MainPID --value nix-daemon.service").strip()
        client = machine.succeed('readlink -f "$(command -v nix)"').strip()
        daemon = machine.succeed(f"readlink -f /proc/{daemon_pid}/exe").strip()
        assert client == machine.succeed(f"readlink -f {nix_bin}/nix").strip(), client
        assert daemon == machine.succeed(f"readlink -f {nix_bin}/nix-daemon").strip(), daemon

    with subtest("VM-011 denied and unavailable tiers do not fall through"):
        assert http_code(f"/{DENIED_HASH}.narinfo") == "503"
        assert http_code(f"/{FAILING_HASH}.narinfo") == "503"
        machine.fail(f"nix-store --realise /nix/store/{DENIED_HASH}-denied")
        machine.fail(f"nix-store --realise /nix/store/{FAILING_HASH}-failing")
        journal = machine.succeed("journalctl -u stockhouse-proxy")
        assert "shared denied" in journal and "shared unavailable" in journal, journal
  '';
}
