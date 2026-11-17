# VM story: a build is ingested, uploaded signed, and substituted back through
# the proxy, while tampered cache content is refused.
args:
(import ./lib/vm.nix args).story "substitution" {
  passthru.bloomery = [
    "CLIENT-INTEGRATION-VM-002"
    "CLIENT-INTEGRATION-VM-003"
    "CLIENT-INTEGRATION-VM-004"
  ];
  script = ''
    with subtest("VM-002 a build is ingested and uploaded"):
        first = build("first")
        wait_uploaded(first)
        machine.succeed(f"test -f {CACHE}/{store_hash(first)}.narinfo")
        machine.succeed(f"grep -q '^Sig: {KEY_NAME}:' {CACHE}/{store_hash(first)}.narinfo")

    with subtest("VM-003 a signed uploaded closure substitutes through the proxy"):
        machine.succeed(f"nix-store --delete {first}")
        machine.fail(f"test -e {first}")
        machine.succeed(f"nix-store --realise {first}")
        machine.succeed(f"grep -q first {first}")

    with subtest("VM-004 tampered cache content is not substituted"):
        target = build("tampered")
        wait_uploaded(target)
        narinfo = f"{CACHE}/{store_hash(target)}.narinfo"
        machine.succeed(f"cp {narinfo} /tmp/original.narinfo")
        machine.succeed(f"sed -i 's/^NarSize: .*/NarSize: 999999/' {narinfo}")
        assert http_code(f"/{store_hash(target)}.narinfo") == "502"
        machine.succeed(f"cp /tmp/original.narinfo {narinfo}")
        nar = machine.succeed(f"sed -n 's/^URL: //p' {narinfo}").strip()
        machine.succeed(f"printf 'x' | dd of={CACHE}/{nar} bs=1 seek=40 conv=notrunc")
        machine.succeed(f"nix-store --delete {target}")
        machine.fail(f"nix-store --realise {target}")
        machine.fail(f"test -e {target}")
  '';
}
