# Module story: lease renewal is a hardened oneshot started by timers and an
# activation snippet, never part of boot or activation.
args: let
  m = import ./lib/module.nix args;
  inherit (m) lib evaluate base hasFailure;
  server = lib.recursiveUpdate base {
    uploader = {
      backend = "server";
      backoffBase = 2;
      backoffMax = 30;
    };
    lease.extraSlots = [
      {
        slot = 3;
        path = "/nix/var/nix/profiles/per-user/root/profile";
      }
    ];
  };
  config = evaluate m.default server;
  uploader = config.systemd.services.stockhouse-uploader.serviceConfig;
  leaseUnit = config.systemd.services.stockhouse-lease;
  lease = leaseUnit.serviceConfig;
  timer = config.systemd.timers.stockhouse-lease;
  dailyTimer = config.systemd.timers.stockhouse-lease-daily;
  switchTimer = config.systemd.timers.stockhouse-lease-switch;
  leaseFile = lib.last (lib.splitString " " lease.ExecStart);
  leaseFor = protocol: let
    withProtocol = evaluate m.default (lib.recursiveUpdate server {uploader.protocol = protocol;});
  in
    lib.last (lib.splitString " " withProtocol.systemd.services.stockhouse-lease.serviceConfig.ExecStart);
  hardened = unit:
    unit.ProtectSystem
    == "strict"
    && unit.ProtectHome
    && unit.PrivateTmp
    && unit.NoNewPrivileges
    && unit.RestrictAddressFamilies == ["AF_INET" "AF_INET6" "AF_UNIX"];
in
  # MODULE-020: the lease service is a hardened, credentialed oneshot.
  assert lease.Type == "oneshot" && lease.User == "stockhouse" && lease.SuccessExitStatus == "75";
  assert lease.LoadCredential == ["private:/run/keys/stockhouse-private"];
  assert hardened lease && hardened uploader;
  # Hardening leaves only the queue (and a nix-copy destination) writable.
  assert uploader.ReadWritePaths == ["-/var/lib/stockhouse/queue"];
  assert (evaluate m.default (lib.recursiveUpdate base {
    uploader = {
      queueDir = "/srv/stockhouse/queue";
      backend = "nix-copy";
      nixCopyDestination = "file:///srv/cache";
    };
  })).systemd.services.stockhouse-uploader.serviceConfig.ReadWritePaths
  == ["-/srv/stockhouse/queue" "-/srv/cache"];
  assert lib.hasInfix "/bin/stockhouse lease --config /nix/store/" lease.ExecStart;
  # MODULE-021: boot, hourly and daily renewal, and a switch timer restarted by activation.
  assert timer.timerConfig.OnBootSec == "1min" && timer.timerConfig.OnUnitActiveSec == "1h";
  assert !(timer.timerConfig ? OnCalendar) && !(timer.timerConfig.Persistent or false);
  assert dailyTimer.timerConfig.OnCalendar == "daily" && dailyTimer.timerConfig.Persistent;
  assert !(dailyTimer.timerConfig ? OnBootSec) && !(dailyTimer.timerConfig ? OnUnitActiveSec);
  assert dailyTimer.timerConfig.Unit == "stockhouse-lease.service";
  assert timer.timerConfig.RandomizedDelaySec == "600" && dailyTimer.timerConfig.RandomizedDelaySec == "600";
  assert switchTimer.timerConfig.OnActiveSec == "30s" && switchTimer.timerConfig.AccuracySec == "1s";
  assert switchTimer.timerConfig.Unit == "stockhouse-lease.service";
  assert lib.hasInfix "systemctl restart --no-block stockhouse-lease-switch.timer || true" config.system.activationScripts.stockhouse-lease.text;
  # MODULE-022: only the boot/hourly and daily timers are installed into a target.
  assert leaseUnit.wantedBy == [] && leaseUnit.requiredBy == [] && leaseUnit.before == [];
  assert switchTimer.wantedBy == [] && switchTimer.requiredBy == [] && switchTimer.before == [];
  assert timer.wantedBy == ["timers.target"] && dailyTimer.wantedBy == ["timers.target"];
  # MODULE-023: lease options are validated.
  assert hasFailure (evaluate m.default (lib.recursiveUpdate base {lease.enable = true;})) "lease.enable";
  # The default follows the uploader, and an explicit lease without it fails.
  assert !(evaluate m.default (lib.recursiveUpdate server {uploader.enable = false;})).services.stockhouse.lease.enable;
  assert hasFailure (evaluate m.default (lib.recursiveUpdate server {
    uploader.enable = false;
    lease.enable = true;
  })) "lease.enable";
  assert hasFailure (evaluate m.default (lib.recursiveUpdate server {
    lease.extraSlots = [
      {
        slot = 4;
        path = "relative/profile";
      }
    ];
  })) "extraSlots";
  assert hasFailure (evaluate m.default (lib.recursiveUpdate server {
    lease.extraSlots = [
      {
        slot = 4;
        path = "/a";
      }
      {
        slot = 4;
        path = "/b";
      }
    ];
  })) "extraSlots";
  assert m.failed config == []; {
    module-lease = m.check "module-lease" {
      passthru.bloomery = [
        "CLIENT-INTEGRATION-MODULE-020"
        "CLIENT-INTEGRATION-MODULE-021"
        "CLIENT-INTEGRATION-MODULE-022"
        "CLIENT-INTEGRATION-MODULE-023"
        "CLIENT-INTEGRATION-MODULE-027"
      ];
      script = ''
        # MODULE-020: the lease configuration names the URL, identity file and extra slot.
        grep -q 'base_url = "https://cache.example.invalid/org/"' ${leaseFile}
        grep -q 'machine_id_file = "/var/lib/stockhouse/machine-id"' ${leaseFile}
        grep -qx 'protocol = "auto"' ${leaseFile}
        grep -qx 'protocol = "v1"' ${leaseFor "v1"}
        grep -qx 'protocol = "v2"' ${leaseFor "v2"}
        grep -q 'slot = 3' ${leaseFile}
        ! grep -q 'stockhouse-private' ${leaseFile}
      '';
    };
  }
