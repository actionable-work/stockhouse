# VM story: loopback and device sign-in through the stockhouse-auth wrapper,
# logout, and the sealed owner-only token store keeping tokens out of every
# visible place.
args:
(import ./lib/vm.nix args).story "signin" {
  signIn = true;
  passthru.bloomery = [
    "CLIENT-INTEGRATION-VM-013"
    "CLIENT-INTEGRATION-VM-014"
    "CLIENT-INTEGRATION-VM-017"
    "CLIENT-INTEGRATION-VM-018"
  ];
  script = ''
    STORE = "/var/lib/stockhouse/auth"
    # The nix-copy uploader would re-substitute deleted fixture paths.
    machine.systemctl("stop stockhouse-uploader")
    machine.wait_for_unit("stockhouse-vm-fake.service")
    machine.wait_for_open_port(8486)
    assert not signed_in()

    with subtest("VM-014 loopback login signs the machine in"):
        machine.succeed("setsid stockhouse-auth login < /dev/null > /tmp/loopback.out 2>&1 &")
        machine.wait_until_succeeds("grep -q '^http://127.0.0.1:8486/oauth/authorize' /tmp/loopback.out", timeout=60)
        url = machine.succeed("grep '^http' /tmp/loopback.out").strip()
        assert "code_challenge_method=S256" in url and "redirect_uri=http%3A%2F%2F127.0.0.1%3A" in url, url
        machine.succeed(f"curl -sfL -o /dev/null '{url}'")
        machine.wait_until_succeeds("grep -q 'Signed in.' /tmp/loopback.out", timeout=60)
        assert signed_in()

    with subtest("VM-017 logout revokes and deletes"):
        refresh = fake()["refresh"]
        assert machine.succeed("stockhouse-auth logout").strip() == "Signed out."
        state = fake()
        assert refresh in state["revoked"] and state["access"] is None, state
        machine.succeed(f"test ! -e {STORE}/tokens.sealed")
        assert not signed_in()
        path = fixture("after-logout")
        assert http_code(f"/{store_hash(path)}.narinfo") == "503"

    with subtest("VM-013 device login signs the machine in and substitutes"):
        device_login()
        assert signed_in()
        machine.succeed(f"nix-store --max-jobs 0 --realise {path}")
        machine.succeed(f"test -e {path}")

    with subtest("VM-018 tokens stay in the owner-only sealed store"):
        assert machine.succeed(f"stat -c '%a %U' {STORE}").strip() == "700 stockhouse"
        assert machine.succeed(f"stat -c '%a %U' {STORE}/tokens.sealed").strip() == "600 stockhouse"
        state = fake()
        tokens = state["issued"]
        assert state["access"] in tokens and len(tokens) >= 4
        sealed = machine.succeed(f"base64 -w0 {STORE}/tokens.sealed")
        raw = base64.b64decode(sealed)
        pid = machine.succeed("systemctl show -p MainPID --value stockhouse-proxy").strip()
        config = machine.succeed(f"tr '\\0' ' ' < /proc/{pid}/cmdline").split()[-1]
        visible = [
            machine.succeed("journalctl -b --no-pager"),
            machine.succeed("cat /proc/[0-9]*/cmdline 2>/dev/null | tr '\\0' ' ' || true"),
            machine.succeed("cat /proc/[0-9]*/environ 2>/dev/null | tr '\\0' ' ' || true"),
            machine.succeed("systemctl cat stockhouse-proxy stockhouse-uploader"),
            machine.succeed(f"cat {config}"),
            machine.succeed(f"cat $(grep -o '/nix/store/[^\"]*stockhouse-auth.toml' {config})"),
            machine.succeed("cat /tmp/loopback.out /tmp/login.out"),
        ]
        for value in tokens:
            assert value.encode() not in raw, "token in sealed store"
            for text in visible:
                assert value not in text, "token leaked"
  '';
}
