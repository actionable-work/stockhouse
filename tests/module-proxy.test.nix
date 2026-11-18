# Module story: the exported module runs a hardened, loopback-only proxy and
# makes it Nix's only substituter.
args: let
  m = import ./lib/module.nix args;
  inherit (m) lib config proxy evaluate base hasFailure;
in
  # MODULE-001: both exported module names produce the same proxy unit.
  assert m.client.systemd.services.stockhouse-proxy.serviceConfig.ExecStart == proxy.ExecStart;
  # MODULE-002: a dynamic-user service started from the generated configuration.
  assert proxy.DynamicUser && proxy.Restart == "on-failure";
  assert lib.hasInfix "/bin/stockhouse proxy --config /nix/store/" proxy.ExecStart;
  # MODULE-009: the unit is hardened.
  assert proxy.ProtectSystem == "strict" && proxy.NoNewPrivileges && proxy.PrivateTmp;
  assert proxy.RestrictAddressFamilies == ["AF_INET" "AF_INET6" "AF_UNIX"];
  # MODULE-006: the proxy is the only substituter and tier keys are trusted.
  assert config.nix.settings.substituters == ["http://127.0.0.1:8484"];
  assert lib.elem m.key config.nix.settings.trusted-public-keys;
  assert (evaluate m.default (lib.recursiveUpdate base {proxy.address = "::1";})).nix.settings.substituters == ["http://[::1]:8484"];
  # MODULE-005: a non-loopback address fails evaluation.
  assert m.failed config == [];
  assert hasFailure (evaluate m.default (lib.recursiveUpdate base {proxy.address = "0.0.0.0";})) "loopback"; {
    module-proxy = m.check "module-proxy" {
      passthru.bloomery = [
        "CLIENT-INTEGRATION-MODULE-001"
        "CLIENT-INTEGRATION-MODULE-002"
        "CLIENT-INTEGRATION-MODULE-005"
        "CLIENT-INTEGRATION-MODULE-006"
        "CLIENT-INTEGRATION-MODULE-009"
      ];
    };
  }
