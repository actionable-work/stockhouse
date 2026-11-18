# Shared helpers for the module evaluation stories: evaluate the client module
# without a VM and expose the generated units, configuration and assertions.
{
  pkgs,
  nixpkgs,
  modules,
}: let
  inherit (pkgs) lib;
  key = "stockhouse-vm-fixture-1:UpT9w0BGYiXPBfpdNiWwBf1b3LAwvtjoUHLlbFaOM3E=";
  evaluate = module: settings:
    (nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        module
        {
          boot.isContainer = true;
          system.stateVersion = "25.11";
          services.stockhouse = settings;
        }
      ];
    })
    .config;
  base = {
    proxy = {
      enable = true;
      private = {
        url = "https://cache.example.invalid/org/";
        trustedKeys = [key];
        credentialFile = "/run/keys/stockhouse-private";
      };
      public = {
        url = "https://cache.nixos.org/";
        trustedKeys = ["cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY="];
      };
    };
    uploader.enable = true;
  };
  config = evaluate modules.default base;
  failed = cfg: map (a: a.message) (lib.filter (a: !a.assertion) cfg.assertions);
  proxy = config.systemd.services.stockhouse-proxy.serviceConfig;
in {
  inherit lib key evaluate base config failed proxy;
  client = evaluate modules.client base;
  default = modules.default;
  uploader = config.systemd.services.stockhouse-uploader.serviceConfig;
  configFile = lib.last (lib.splitString " " proxy.ExecStart);
  withTier = tier: lib.recursiveUpdate base {proxy.private = tier;};
  hasFailure = cfg: fragment: lib.any (lib.hasInfix fragment) (failed cfg);
  # A check derivation that exists only once the evaluation asserts held.
  check = name: {
    passthru,
    script ? "",
  }:
    pkgs.runCommand "stockhouse-${name}" {inherit passthru;} ''
      ${script}
      mkdir "$out"
    '';
}
