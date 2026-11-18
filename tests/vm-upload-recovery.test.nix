# VM story: interrupted, uncertain and quota-limited uploads keep their
# identity and finish once the server allows, also across a reboot.
args:
(import ./lib/vm.nix args).story "upload-recovery" {
  backend = "server";
  passthru.bloomery = [
    "CLIENT-INTEGRATION-VM-022"
    "CLIENT-INTEGRATION-VM-023"
    "CLIENT-INTEGRATION-VM-024"
    "CLIENT-INTEGRATION-VM-028"
  ];
  script = ''
    def upload_ids(h):
        return {line.split()[1].split("/")[1] for line in fake_lines(h) if line.split()[1].startswith("uploads/")}


    def queued(tag):
        machine.systemctl("stop stockhouse-uploader")
        path = build_expr(drv(tag))
        return path, store_hash(path), pending_record(path)["id"]


    with subtest("VM-022 an upload interrupted by a restart keeps its identity"):
        f, h, record_id = queued("stall")
        control(f"stall-{h}")
        machine.systemctl("start stockhouse-uploader")
        machine.wait_until_succeeds(f"grep -q ' {h} stalled' {UPLOAD_STATE}/requests.log", timeout=120)
        machine.systemctl("restart stockhouse-uploader")
        control(f"stall-{h}", on=False)
        wait_uploaded(f)
        lines = fake_lines(h)
        assert upload_ids(h) == {record_id}, lines
        assert any(line.startswith("GET") for line in lines), lines
        assert lines[-1].endswith("published"), lines
        assert json.loads(machine.succeed(f"cat {QUEUE}/done/{h}.json"))["id"] == record_id

    with subtest("VM-023 a lost reply is reconciled without resending"):
        g, h, record_id = queued("drop")
        control(f"drop-{h}")
        machine.systemctl("start stockhouse-uploader")
        wait_uploaded(g)
        lines = fake_lines(h)
        puts = [line for line in lines if line.startswith("PUT")]
        assert len(puts) == 1 and puts[0].endswith("dropped"), lines
        assert lines[-1].startswith("GET") and lines[-1].endswith("published"), lines
        assert upload_ids(h) == {record_id}, lines

    with subtest("VM-024 quota exhaustion recovers"):
        control("quota")
        q, h, record_id = queued("quota")
        machine.systemctl("start stockhouse-uploader")
        machine.wait_until_succeeds(f"grep -q 'quota_exceeded' {QUEUE}/pending/{h}.json", timeout=120)
        assert pending_record(q)["id"] == record_id
        control("quota", on=False)
        wait_uploaded(q)
        assert json.loads(machine.succeed(f"cat {QUEUE}/done/{h}.json"))["id"] == record_id
        assert upload_ids(h) == {record_id}

    with subtest("VM-028 uploads pending at reboot reach the server"):
        r, h, record_id = queued("reboot")
        machine.shutdown()
        machine.start()
        machine.wait_for_unit("stockhouse-uploader.service")
        wait_uploaded(r)
        assert json.loads(machine.succeed(f"cat {QUEUE}/done/{h}.json"))["id"] == record_id
        assert upload_ids(h) == {record_id}
        assert fake_lines(h)[-1].endswith("published")
  '';
}
