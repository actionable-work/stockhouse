# The Stockhouse client NixOS module: the loopback substitution proxy, the
# post-build ingestion uploader and the Nix settings that point at them.
# Credential options are string paths read by systemd at runtime; no
# credential value ever enters an option, a derivation or the store.
{defaultPackage}: {
  config,
  lib,
  pkgs,
  ...
}: let
  inherit (lib) mkEnableOption mkIf mkOption types;
  cfg = config.services.stockhouse;
  toml = pkgs.formats.toml {};

  tierType = types.submodule {
    options = {
      url = mkOption {
        type = types.str;
        description = "Base URL ending in `/`: `file://`, `http://` or `https://`.";
      };
      trustedKeys = mkOption {
        type = types.listOf types.str;
        description = "Keys (`name:base64`) whose narinfo signatures this tier may serve.";
      };
      credentialFile = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Absolute runtime path of this tier's bearer credential, loaded with
          systemd `LoadCredential=`. systemd copies it as root, so protect the
          source (for example `root:root 0400`). Never a store path.
        '';
      };
    };
  };

  tierOption = name:
    mkOption {
      type = types.nullOr tierType;
      default = null;
      description = "The ${name} tier, or null to skip it.";
    };

  tiers = lib.filterAttrs (_: tier: tier != null) {inherit (cfg.proxy) private shared public;};
  credentialTiers = lib.filterAttrs (_: tier: tier.credentialFile != null) tiers;

  address = cfg.proxy.address;
  isIPv6 = lib.hasInfix ":" address;
  isLoopback = address == "::1" || builtins.match "127\\.[0-9]+\\.[0-9]+\\.[0-9]+" address != null;
  listen =
    if isIPv6
    then "[${address}]:${toString cfg.proxy.port}"
    else "${address}:${toString cfg.proxy.port}";
  proxyUrl = "http://${listen}";

  tierConfig = name: tier:
    {
      url = tier.url;
      trusted_keys = tier.trustedKeys;
    }
    // lib.optionalAttrs (tier.credentialFile != null) {credential.systemd = name;};

  proxyConfig = toml.generate "stockhouse-proxy.toml" ({
      version = 1;
      inherit listen;
      priority = cfg.proxy.priority;
    }
    // lib.mapAttrs tierConfig tiers);

  nix = "${config.nix.package}/bin/nix";
  uploaderConfig = toml.generate "stockhouse-uploader.toml" {
    version = 1;
    queue_dir = cfg.uploader.queueDir;
    max_pending = cfg.uploader.maxPending;
    max_attempts = cfg.uploader.maxAttempts;
    batch_size = 64;
    backoff_base_secs = 5;
    backoff_max_secs = 3600;
    poll_interval_secs = cfg.uploader.pollInterval;
    rescan_interval_secs = cfg.uploader.rescanInterval;
    backend =
      if cfg.uploader.backend == "nix-copy"
      then {
        kind = "nix-copy";
        destination = cfg.uploader.nixCopyDestination;
        inherit nix;
      }
      else {kind = "deferred";};
    scanner =
      if cfg.uploader.scanner == "nix-path-info"
      then {
        kind = "nix-path-info";
        inherit nix;
      }
      else {kind = "none";};
  };

  hookScript = pkgs.writeShellScript "stockhouse-post-build-hook" ''
    ${cfg.package}/bin/stockhouse post-build-hook --queue ${lib.escapeShellArg cfg.uploader.queueDir} || true
  '';

  credentialAssertions =
    lib.mapAttrsToList (name: tier: {
      assertion = lib.hasPrefix "/" tier.credentialFile && !(lib.hasPrefix "/nix/store" tier.credentialFile);
      message = "services.stockhouse.proxy.${name}.credentialFile must be an absolute runtime path outside /nix/store.";
    })
    credentialTiers;
in {
  options.services.stockhouse = {
    package = mkOption {
      type = types.package;
      default = defaultPackage pkgs;
      defaultText = lib.literalExpression "stockhouse.packages.\${system}.stockhouse";
      description = "The stockhouse package.";
    };

    proxy = {
      enable = mkEnableOption "the loopback Stockhouse substitution proxy";
      address = mkOption {
        type = types.str;
        default = "127.0.0.1";
        description = "Loopback address to listen on (127.0.0.0/8 or ::1).";
      };
      port = mkOption {
        type = types.port;
        default = 8484;
        description = "Port to listen on. An occupied port is an error, never silently changed.";
      };
      priority = mkOption {
        type = types.ints.unsigned;
        default = 30;
        description = "Priority advertised in the proxy's nix-cache-info.";
      };
      configureNix = mkOption {
        type = types.bool;
        default = true;
        description = "Make the proxy Nix's only substituter and trust every tier's keys.";
      };
      private = tierOption "private";
      shared = tierOption "shared";
      public = tierOption "public";
    };

    uploader = {
      enable = mkEnableOption "the Stockhouse post-build ingestion uploader";
      queueDir = mkOption {
        type = types.str;
        default = "/var/lib/stockhouse/queue";
        description = "Durable queue directory, owned by the stockhouse user.";
      };
      maxPending = mkOption {
        type = types.ints.positive;
        default = 10000;
        description = "Queue capacity; further builds are counted as overflow.";
      };
      maxAttempts = mkOption {
        type = types.ints.positive;
        default = 20;
        description = "Retry outcomes before a record fails.";
      };
      pollInterval = mkOption {
        type = types.ints.positive;
        default = 5;
        description = "Seconds between uploader passes.";
      };
      rescanInterval = mkOption {
        type = types.ints.positive;
        default = 3600;
        description = "Seconds between periodic store rescans.";
      };
      backend = mkOption {
        type = types.enum ["deferred" "nix-copy"];
        default = "deferred";
        description = "`deferred` keeps records pending until server upload exists (P2.08); `nix-copy` copies to a local file:// cache for development and tests.";
      };
      nixCopyDestination = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Absolute `file://` destination for the `nix-copy` backend.";
      };
      scanner = mkOption {
        type = types.enum ["nix-path-info" "none"];
        default = "nix-path-info";
        description = "How rescans find locally built paths.";
      };
      configureHook = mkOption {
        type = types.bool;
        default = true;
        description = "Install the post-build hook that records built paths.";
      };
    };
  };

  config = lib.mkMerge [
    (mkIf cfg.proxy.enable {
      assertions =
        [
          {
            assertion = isLoopback;
            message = "services.stockhouse.proxy.address must be a loopback address.";
          }
          {
            assertion = tiers != {};
            message = "services.stockhouse.proxy needs at least one tier.";
          }
          {
            assertion = cfg.proxy.public == null || cfg.proxy.public.credentialFile == null;
            message = "services.stockhouse.proxy.public cannot have a credentialFile.";
          }
        ]
        ++ credentialAssertions;

      systemd.services.stockhouse-proxy = {
        description = "Stockhouse loopback substitution proxy";
        wantedBy = ["multi-user.target"];
        after = ["network.target"];
        serviceConfig = {
          ExecStart = "${cfg.package}/bin/stockhouse proxy --config ${proxyConfig}";
          DynamicUser = true;
          Restart = "on-failure";
          RestartSec = 2;
          LoadCredential = lib.mapAttrsToList (name: tier: "${name}:${tier.credentialFile}") credentialTiers;
          ProtectSystem = "strict";
          ProtectHome = true;
          PrivateTmp = true;
          NoNewPrivileges = true;
          RestrictAddressFamilies = ["AF_INET" "AF_INET6" "AF_UNIX"];
        };
      };

      nix.settings = mkIf cfg.proxy.configureNix {
        substituters = lib.mkForce [proxyUrl];
        trusted-public-keys = lib.concatMap (tier: tier.trustedKeys) (lib.attrValues tiers);
      };
    })

    (mkIf cfg.uploader.enable {
      assertions = [
        {
          assertion = cfg.uploader.backend != "nix-copy" || (cfg.uploader.nixCopyDestination != null && lib.hasPrefix "file:///" cfg.uploader.nixCopyDestination);
          message = "services.stockhouse.uploader.nixCopyDestination must be an absolute file:// URL.";
        }
      ];

      users.users.stockhouse = {
        isSystemUser = true;
        group = "stockhouse";
        description = "Stockhouse uploader";
      };
      users.groups.stockhouse = {};

      systemd.services.stockhouse-uploader = {
        description = "Stockhouse post-build ingestion uploader";
        wantedBy = ["multi-user.target"];
        after = ["nix-daemon.socket"];
        serviceConfig = {
          ExecStart = "${cfg.package}/bin/stockhouse uploader --config ${uploaderConfig}";
          User = "stockhouse";
          Group = "stockhouse";
          StateDirectory = "stockhouse";
          StateDirectoryMode = "0755";
          Restart = "always";
          RestartSec = 5;
        };
      };

      nix.settings.post-build-hook = mkIf cfg.uploader.configureHook hookScript;
    })
  ];
}
