# VM story: the root post-build hook drops to the queue owner, never fails a
# build, and pending records keep their identity across a reboot.
args:
(import ./lib/vm.nix args).story "durability" {
  passthru.bloomery = [
    "CLIENT-INGEST-HOOK-009"
    "CLIENT-INGEST-RETRY-008"
    "CLIENT-INTEGRATION-VM-005"
    "CLIENT-INTEGRATION-VM-006"
  ];
  script = ''
    with subtest("VM-005 HOOK-009 an unwritable queue does not fail builds"):
        # Root could write a 0500 directory; the record is missing only
        # because the hook switched to the queue owner first.
        machine.succeed(f"chmod 0500 {QUEUE}/pending")
        blocked = build("blocked")
        machine.fail(f"test -e {QUEUE}/pending/{store_hash(blocked)}.json")
        machine.succeed(f"chmod 0755 {QUEUE}/pending")

    with subtest("VM-006 RETRY-008 pending uploads survive reboot with their identity"):
        wait_drained()
        machine.systemctl("stop stockhouse-uploader")
        survivor = build("survivor")
        record = json.loads(machine.succeed(f"cat {QUEUE}/pending/{store_hash(survivor)}.json"))
        machine.shutdown()
        machine.start()
        machine.wait_for_unit("stockhouse-uploader.service")
        wait_uploaded(survivor)
        done = json.loads(machine.succeed(f"cat {QUEUE}/done/{store_hash(survivor)}.json"))
        assert done["id"] == record["id"], (done, record)
  '';
}
