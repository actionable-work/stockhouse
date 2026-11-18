# VM story: refresh across expiry while the proxy serves and several sign-in
# processes refresh at once, with no refresh token ever reused; a revoked
# grant is a 503, never a miss, and signing in again substitutes at once.
args:
(import ./lib/vm.nix args).story "refresh" {
  signIn = true;
  passthru.bloomery = [
    "CLIENT-INTEGRATION-VM-010"
    "CLIENT-INTEGRATION-VM-015"
    "CLIENT-INTEGRATION-VM-016"
    "CLIENT-INTEGRATION-MODULE-026"
  ];
  script = ''
    # The nix-copy uploader would re-substitute deleted fixture paths.
    machine.systemctl("stop stockhouse-uploader")
    machine.wait_for_unit("stockhouse-vm-fake.service")
    machine.wait_for_open_port(8486)
    fake("/__fake/config?lifetime=70")
    served = fixture("served")
    hidden = fixture("hidden")
    device_login()
    narinfo = f"/{store_hash(served)}.narinfo"
    assert http_code(narinfo) == "200"

    def lookups(seconds, refreshers):
        """Proxy lookups every 0.5 s (and optional `stockhouse-auth refresh` loops)."""
        machine.succeed(
            f"end=$((SECONDS + {seconds})); rm -f /tmp/bad /tmp/refresh-fail; "
            f"( while [ $SECONDS -lt $end ]; do c=$(curl -s -o /dev/null -w '%{{http_code}}' {PROXY}{narinfo}); "
            "[ \"$c\" = 200 ] || echo \"$c\" >> /tmp/bad; sleep 0.5; done ) & "
            f"for i in $(seq 1 {refreshers}); do ( sleep $((i * 5)); while [ $SECONDS -lt $end ]; do "
            "stockhouse-auth refresh > /dev/null 2>> /tmp/refresh-fail || echo failed >> /tmp/refresh-fail; "
            "sleep 3; done ) & done; wait",
            timeout=300,
        )
        problems = machine.succeed("cat /tmp/bad /tmp/refresh-fail 2>/dev/null || true")
        assert problems == "", problems + machine.succeed("journalctl -u stockhouse-proxy --no-pager | tail -20")

    with subtest("VM-015 refresh across expiry is serialized"):
        # Proxy alone, past the 70 s lifetime: the fake rejects expired
        # access tokens, so every 200 shows the proxy refreshed in time.
        before = fake()["refreshes"]
        lookups(80, 0)
        assert fake()["refreshes"] > before, fake()
        # Then with four staggered `stockhouse-auth refresh` processes.
        first = fake()["refresh"]
        lookups(90, 4)
        state = fake()
        assert state["reuse"] == [] and state["overlaps"] == [], state
        assert state["refresh"] != first and state["refreshes"] >= 3, state
        assert signed_in()

    with subtest("VM-016 a revoked grant is a 503, not a miss"):
        signed_out = "journalctl -u stockhouse-proxy | grep -c 'private credential signed-out' || true"
        before = int(machine.succeed(signed_out).strip())
        fake("/__fake/revoke")
        assert http_code(narinfo) == "503"
        # The proxy notices at its next refresh: at once, or once the young
        # token nears expiry (lifetime 70 s).
        machine.wait_until_succeeds(
            f"curl -s -o /dev/null {PROXY}{narinfo}; test $({signed_out}) -gt {before}",
            timeout=180,
        )
        assert not signed_in()
        assert http_code(f"/{store_hash(hidden)}.narinfo") == "503"
        machine.fail(f"nix-store --check-validity {hidden}")
        code, out = machine.execute(f"nix-store -v --max-jobs 0 --realise {hidden} 2>&1")
        assert code != 0 and "503" in out, out
        # MODULE-026: while the proxy answers 503, a local build still succeeds.
        expr = 'derivation { name = "vm-fallback"; system = builtins.currentSystem; builder = "/bin/sh"; args = [ "-c" "echo fallback > $out" ]; }'
        machine.succeed(f"nix-build --no-out-link -E '{expr}'")
        device_login()
        machine.succeed(f"nix-store --max-jobs 0 --realise {hidden}")
  '';
}
