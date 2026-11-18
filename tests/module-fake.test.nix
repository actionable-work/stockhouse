# Module story: the VM fixture server is a test-only package that the
# module and the released package neither contain, install nor run.
{
  fakeServer,
  package,
  ...
} @ args: let
  m = import ./lib/module.nix {inherit (args) pkgs nixpkgs modules;};
  inherit (m) lib evaluate base;
  fakeName = "stockhouse-fake-server";
  configs = [
    (evaluate m.default (lib.recursiveUpdate base {uploader.backend = "server";}))
    (evaluate m.default (lib.recursiveUpdate base {
      proxy.private = {
        url = "https://cache.example.invalid/t/1/";
        credentialFile = null;
      };
      uploader.backend = "server";
      auth = {
        enable = true;
        server = "https://example.invalid/";
        endpoints = {
          authorization = "https://example.invalid/oauth/authorize";
          token = "https://example.invalid/oauth/token";
          deviceAuthorization = "https://example.invalid/oauth/device";
          revocation = "https://example.invalid/oauth/revoke";
        };
        clientId = "stockhouse-cli";
        scopes = ["cache:read"];
      };
    }))
  ];
  units = cfg: lib.filterAttrs (name: _: lib.hasPrefix "stockhouse" name) cfg.systemd.services;
  runs = unit:
    lib.hasInfix fakeName (toString [
      (unit.serviceConfig.ExecStart or "")
      (unit.serviceConfig.ExecStartPre or "")
      (unit.script or "")
      (unit.preStart or "")
    ]);
  clean = cfg:
    toString cfg.services.stockhouse.package
    == toString package
    && !(lib.elem (toString fakeServer) (map toString cfg.environment.systemPackages))
    && !(lib.any runs (lib.attrValues (units cfg)))
    && units cfg != {};
in
  # FAKE-001: no evaluated configuration installs or runs the fake server.
  assert lib.all clean configs; {
    module-fake = m.check "module-fake" {
      passthru.bloomery = ["CLIENT-INTEGRATION-FAKE-001"];
      script = ''
        # FAKE-001: the fake is its own package; the client package holds only `stockhouse`.
        test -x ${fakeServer}/bin/${fakeName}
        test "$(ls ${package}/bin)" = stockhouse
        ! grep -rqa ${fakeName} ${package}
      '';
    };
  }
