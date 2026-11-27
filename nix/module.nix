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
    // lib.optionalAttrs (tier.credentialFile != null) {credential.systemd = name;}
    // lib.optionalAttrs (name == "private" && auth.enable) {credential = auth.unitCredential.toml;};

  # Server sign-in (see the integration design, "Server sign-in").
  auth = cfg.auth;
  sealed = auth.store.mode == "sealed";
  keyCredential = "stockhouse-token-key";
  keyUnit = "stockhouse-token-key.service";
  createKey = sealed && auth.store.createKey;
  secureUrl = url:
    lib.hasPrefix "https://" url
    || builtins.match "http://(127[.][0-9]{1,3}[.][0-9]{1,3}[.][0-9]{1,3}|[[]::1[]])(:[0-9]+)?/.*" url != null;
  runtimePath = path: lib.hasPrefix "/" path && !(lib.hasPrefix "/nix/store" path);
  privateUrl =
    if cfg.proxy.private == null
    then ""
    else cfg.proxy.private.url;
  authConfig = toml.generate "stockhouse-auth.toml" {
    version = 1;
    server = auth.server;
    client_id = auth.clientId;
    scopes = auth.scopes;
    private_tier = privateUrl;
    request_timeout_secs = auth.requestTimeout;
    endpoints = {
      authorization = auth.endpoints.authorization;
      token = auth.endpoints.token;
      device_authorization = auth.endpoints.deviceAuthorization;
      revocation = auth.endpoints.revocation;
    };
    store =
      {
        mode = auth.store.mode;
        directory = auth.store.directory;
      }
      // lib.optionalAttrs sealed {key_credential = keyCredential;};
  };
  unitCredential = {
    toml.server = "${authConfig}";
    serviceConfig =
      {ReadWritePaths = [auth.store.directory];}
      // lib.optionalAttrs sealed {
        LoadCredentialEncrypted = ["${keyCredential}:${auth.store.keyCredentialFile}"];
      };
    unitConfig = lib.optionalAttrs createKey {
      after = [keyUnit];
      requires = [keyUnit];
    };
  };
  systemdRun = "${config.systemd.package}/bin/systemd-run";
  wrapperProperties =
    [
      "User=stockhouse"
      "Group=stockhouse"
      "UMask=0077"
      "ProtectSystem=strict"
      "ProtectHome=yes"
      "PrivateTmp=yes"
      "NoNewPrivileges=yes"
      "RemoveIPC=yes"
      "RestrictSUIDSGID=yes"
      "ReadWritePaths=${auth.store.directory}"
    ]
    ++ lib.optional sealed "LoadCredentialEncrypted=${keyCredential}:${auth.store.keyCredentialFile}";
  wrapper = pkgs.writeShellScriptBin "stockhouse-auth" ''
    if [ "$(${pkgs.coreutils}/bin/id -u)" != 0 ]; then
      echo "stockhouse-auth: must be run as root" >&2
      exit 1
    fi
    case "$#:$*" in
      "1:login" | "2:login --device" | "1:logout" | "1:refresh" | "1:status") ;;
      *)
        echo "usage: stockhouse-auth login [--device] | logout | refresh | status" >&2
        exit 2
        ;;
    esac
    exec ${systemdRun} --quiet --wait --collect --pipe --service-type=exec \
      ${lib.concatMapStringsSep " " (p: "-p ${lib.escapeShellArg p}") wrapperProperties} \
      -- ${cfg.package}/bin/stockhouse "$@" --config ${authConfig}
  '';
  keyScript = pkgs.writeShellScript "stockhouse-token-key" ''
    set -euo pipefail
    umask 077
    key=${lib.escapeShellArg auth.store.keyCredentialFile}
    if [ -e "$key" ]; then
      exit 0
    fi
    # A partial file from an interrupted run is never used or kept.
    ${pkgs.coreutils}/bin/rm -f "$key.tmp"
    ${pkgs.coreutils}/bin/install -d -m 0700 "$(${pkgs.coreutils}/bin/dirname "$key")"
    ${pkgs.coreutils}/bin/head -c 32 /dev/urandom | ${pkgs.coreutils}/bin/od -An -v -tx1 | ${pkgs.coreutils}/bin/tr -d ' \n' \
      | ${config.systemd.package}/bin/systemd-creds encrypt --name=${keyCredential} - "$key.tmp"
    # Never replace a key that appeared meanwhile.
    ${pkgs.coreutils}/bin/mv -n "$key.tmp" "$key"
    ${pkgs.coreutils}/bin/rm -f "$key.tmp"
  '';
  authAssertions = [
    {
      assertion = cfg.proxy.private != null && cfg.proxy.private.credentialFile == null;
      message = "services.stockhouse.auth needs services.stockhouse.proxy.private without a credentialFile.";
    }
    {
      assertion = lib.all secureUrl ([auth.server privateUrl] ++ lib.attrValues auth.endpoints);
      message = "services.stockhouse.auth URLs (server, endpoints, proxy.private.url) must be https:// or http:// to a loopback IP literal.";
    }
    {
      assertion = runtimePath auth.store.directory && (!sealed || runtimePath auth.store.keyCredentialFile);
      message = "services.stockhouse.auth.store.directory and keyCredentialFile must be absolute runtime paths outside /nix/store.";
    }
  ];

  proxyConfig = toml.generate "stockhouse-proxy.toml" ({
      version = 1;
      inherit listen;
      priority = cfg.proxy.priority;
    }
    // lib.mapAttrs tierConfig tiers);

  nix = "${config.nix.package}/bin/nix";

  # Server upload and leases use the private tier as the server. Its
  # credential wiring is defined once and shared by the uploader and lease
  # units: with server sign-in, the signed-in token (`auth.unitCredential`);
  # otherwise the systemd credential loaded from the private tier's runtime
  # credential file.
  isServer = cfg.uploader.backend == "server";
  privateTier = cfg.proxy.private;
  privateCredential =
    if auth.enable
    then unitCredential
    else {
      toml = {systemd = "private";};
      serviceConfig.LoadCredential = ["private:${privateTier.credentialFile}"];
      unitConfig = {};
    };
  privateUrlSafe = privateTier != null && secureUrl privateTier.url && lib.hasSuffix "/" privateTier.url;
  # Hardening makes the file system read-only, so the uploader may write only
  # its queue and, for `nix-copy`, the local destination (plus sign-in paths).
  # The `-` prefix lets the unit start before a path exists: the default
  # queue is created by the uploader inside its state directory.
  uploaderWritable = map (path: "-${path}") ([cfg.uploader.queueDir]
    ++ lib.optional (cfg.uploader.backend == "nix-copy" && cfg.uploader.nixCopyDestination != null)
    (lib.removePrefix "file://" cfg.uploader.nixCopyDestination));
  # Shared by the uploader and lease units.
  hardening = {
    ProtectSystem = "strict";
    ProtectHome = true;
    PrivateTmp = true;
    NoNewPrivileges = true;
    RestrictAddressFamilies = ["AF_INET" "AF_INET6" "AF_UNIX"];
  };
  machineIdFile = "/var/lib/stockhouse/machine-id";

  serverBackend = {
    kind = "server";
    base_url = privateTier.url;
    protocol = cfg.uploader.protocol;
    max_inflight_nars = cfg.uploader.maxInflightNars;
    inherit nix;
    credential = privateCredential.toml;
  };

  leaseConfig = toml.generate "stockhouse-lease.toml" {
    version = 1;
    base_url = privateTier.url;
    machine_id_file = machineIdFile;
    protocol = cfg.uploader.protocol;
    credential = privateCredential.toml;
    extra = map (e: {inherit (e) slot path;}) cfg.lease.extraSlots;
  };
  leaseSlots = map (e: e.slot) cfg.lease.extraSlots;

  uploaderConfig = toml.generate "stockhouse-uploader.toml" {
    version = 1;
    queue_dir = cfg.uploader.queueDir;
    max_pending = cfg.uploader.maxPending;
    max_attempts = cfg.uploader.maxAttempts;
    batch_size = 64;
    backoff_base_secs = cfg.uploader.backoffBase;
    backoff_max_secs = cfg.uploader.backoffMax;
    poll_interval_secs = cfg.uploader.pollInterval;
    rescan_interval_secs = cfg.uploader.rescanInterval;
    backend =
      if cfg.uploader.backend == "nix-copy"
      then {
        kind = "nix-copy";
        destination = cfg.uploader.nixCopyDestination;
        inherit nix;
      }
      else if isServer
      then serverBackend
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
  options.services.stockhouse.auth = {
    enable = mkEnableOption "server sign-in for the private tier (OAuth 2.0, sealed token store)";
    server = mkOption {
      type = types.str;
      description = "Server origin, for example `https://example.invalid/`.";
    };
    endpoints = lib.genAttrs ["authorization" "token" "deviceAuthorization" "revocation"] (name:
      mkOption {
        type = types.str;
        description = "Absolute ${name} endpoint URL on the server origin.";
      });
    clientId = mkOption {
      type = types.str;
      description = "The public OAuth client ID.";
    };
    scopes = mkOption {
      type = types.listOf types.str;
      description = "Requested scopes.";
    };
    requestTimeout = mkOption {
      type = types.ints.between 1 300;
      default = 30;
      description = "Endpoint connect and read timeout in seconds.";
    };
    store = {
      mode = mkOption {
        type = types.enum ["sealed" "file"];
        default = "sealed";
        description = ''
          `sealed` encrypts the token store with XChaCha20-Poly1305 under a key
          loaded as an encrypted systemd credential; `file` keeps it in an
          owner-only plaintext file and is used only when chosen here.
        '';
      };
      directory = mkOption {
        type = types.str;
        default = "/var/lib/stockhouse/auth";
        description = "Token store directory, created `0700 stockhouse:stockhouse`.";
      };
      keyCredentialFile = mkOption {
        type = types.str;
        default = "/etc/stockhouse/token-key.cred";
        description = ''
          Runtime path of the sealing key encrypted with `systemd-creds`
          (sealed mode; ignored in file mode). With `createKey` the module
          creates it at boot if absent. To create it by hand instead, as root:

              install -d -m 0700 /etc/stockhouse
              head -c 32 /dev/urandom | od -An -v -tx1 | tr -d ' \n' \
                | systemd-creds encrypt --name=stockhouse-token-key - /etc/stockhouse/token-key.cred

          The file is useless without this machine's TPM2 or host key. Never a
          store path.
        '';
      };
      createKey = mkOption {
        type = types.bool;
        default = true;
        description = "Create the sealing key file at boot if it does not exist (sealed mode).";
      };
    };
    configFile = mkOption {
      type = types.path;
      readOnly = true;
      default = authConfig;
      defaultText = lib.literalMD "the generated sign-in configuration";
      description = "The generated sign-in configuration (no secrets).";
    };
    unitCredential = mkOption {
      type = types.attrs;
      readOnly = true;
      internal = true;
      default = unitCredential;
      defaultText = lib.literalMD "derived from the sign-in settings";
      description = ''
        What a unit needs to use the signed-in token: `toml` (the credential
        spec), `serviceConfig` and `unitConfig`. Such a unit runs as
        `stockhouse`.
      '';
    };
    wrapper = mkOption {
      type = types.package;
      readOnly = true;
      internal = true;
      default = wrapper;
      defaultText = lib.literalMD "the generated `stockhouse-auth` wrapper";
      description = "The `stockhouse-auth` command.";
    };
  };

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
      narinfoCacheNegativeTtl = mkOption {
        type = types.ints.unsigned;
        default = 60;
        description = ''
          Seconds Nix caches a narinfo miss when `configureNix` is set (Nix's
          own default is 3600). A path the uploader has published but the
          server has not yet signed reads as a miss; a short TTL lets it
          substitute soon after instead of an hour later.
        '';
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
        type = types.enum ["deferred" "nix-copy" "server"];
        default = "deferred";
        description = "`deferred` keeps records pending; `nix-copy` copies to a local file:// cache for development and tests; `server` uploads each built closure to the private tier (`proxy.private`) with protocol v1 or v2.";
      };
      protocol = mkOption {
        type = types.enum ["auto" "v1" "v2"];
        default = "auto";
        description = "Wire protocol for server uploads and lease renewal. Auto selects the highest version offered by the server.";
      };
      maxInflightNars = mkOption {
        type = types.int;
        default = 4;
        description = "Maximum concurrent NAR uploads with protocol v2 (1–16).";
      };
      backoffBase = mkOption {
        type = types.ints.positive;
        default = 5;
        description = "First retry backoff, seconds.";
      };
      backoffMax = mkOption {
        type = types.ints.positive;
        default = 3600;
        description = "Retry backoff cap, seconds; quota exhaustion waits this long.";
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

    lease = {
      enable = mkOption {
        type = types.bool;
        default = cfg.uploader.enable && cfg.uploader.backend == "server";
        defaultText = lib.literalExpression ''config.services.stockhouse.uploader.enable && config.services.stockhouse.uploader.backend == "server"'';
        description = "Renew this machine's leases on the server's system roots on boot, hourly, daily and after activation (requires the uploader with the `server` backend).";
      };
      randomizedDelaySec = mkOption {
        type = types.ints.unsigned;
        default = 600;
        description = "Random delay added to the boot, hourly and daily lease renewals, so a fleet does not renew at once.";
      };
      extraSlots = mkOption {
        type = types.listOf (types.submodule {
          options = {
            slot = mkOption {
              type = types.ints.between 3 10;
              description = "Lease slot, 3 to 10.";
            };
            path = mkOption {
              type = types.str;
              description = "Absolute path that resolves to the store path to keep.";
            };
          };
        });
        default = [];
        description = "Operator-defined lease slots 3 to 10.";
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
        after = ["network.target"] ++ lib.optionals auth.enable (unitCredential.unitConfig.after or []);
        requires = lib.optionals auth.enable (unitCredential.unitConfig.requires or []);
        serviceConfig =
          {
            ExecStart = "${cfg.package}/bin/stockhouse proxy --config ${proxyConfig}";
            DynamicUser = !auth.enable;
            Restart = "on-failure";
            RestartSec = 2;
            LoadCredential = lib.mapAttrsToList (name: tier: "${name}:${tier.credentialFile}") credentialTiers;
            ProtectSystem = "strict";
            ProtectHome = true;
            PrivateTmp = true;
            NoNewPrivileges = true;
            RestrictAddressFamilies = ["AF_INET" "AF_INET6" "AF_UNIX"];
          }
          // lib.optionalAttrs auth.enable ({
              User = "stockhouse";
              Group = "stockhouse";
              UMask = "0077";
              RemoveIPC = true;
              RestrictSUIDSGID = true;
            }
            // unitCredential.serviceConfig);
      };

      nix.settings = mkIf cfg.proxy.configureNix {
        narinfo-cache-negative-ttl = lib.mkDefault cfg.proxy.narinfoCacheNegativeTtl;
        # The proxy answers 503 when signed out or the server is down; without
        # fallback Nix fails the build instead of building locally.
        fallback = lib.mkDefault true;
        substituters = lib.mkForce [proxyUrl];
        trusted-public-keys = lib.concatMap (tier: tier.trustedKeys) (lib.attrValues tiers);
      };
    })

    (mkIf (cfg.auth.enable || cfg.uploader.enable) {
      users.users.stockhouse = {
        isSystemUser = true;
        group = "stockhouse";
        description = "Stockhouse client services";
      };
      users.groups.stockhouse = {};
    })

    (mkIf cfg.auth.enable {
      assertions = authAssertions;
      systemd.tmpfiles.rules = ["d ${auth.store.directory} 0700 stockhouse stockhouse -"];
      environment.systemPackages = [wrapper];
      systemd.services.stockhouse-token-key = mkIf createKey {
        description = "Create the Stockhouse token sealing key";
        wantedBy = ["multi-user.target"];
        unitConfig.ConditionPathExists = "!${auth.store.keyCredentialFile}";
        serviceConfig = {
          Type = "oneshot";
          RemainAfterExit = true;
          ExecStart = "${keyScript}";
        };
      };
    })

    (mkIf cfg.lease.enable {
      assertions = [
        {
          assertion = cfg.uploader.enable && isServer;
          message = "services.stockhouse.lease.enable needs services.stockhouse.uploader.enable with backend = \"server\".";
        }
        {
          assertion = lib.length (lib.unique leaseSlots) == lib.length leaseSlots;
          message = "services.stockhouse.lease.extraSlots must use distinct slots.";
        }
        {
          assertion = lib.all (e: lib.hasPrefix "/" e.path) cfg.lease.extraSlots;
          message = "services.stockhouse.lease.extraSlots paths must be absolute.";
        }
      ];
    })

    (mkIf cfg.uploader.enable {
      assertions = [
        {
          assertion = cfg.uploader.backend != "nix-copy" || (cfg.uploader.nixCopyDestination != null && lib.hasPrefix "file:///" cfg.uploader.nixCopyDestination);
          message = "services.stockhouse.uploader.nixCopyDestination must be an absolute file:// URL.";
        }
        {
          assertion = !isServer || (privateUrlSafe && (auth.enable || privateTier.credentialFile != null));
          message = "services.stockhouse.uploader.backend = \"server\" needs services.stockhouse.proxy.private with an https:// (or loopback-IP http://) URL ending in /, and services.stockhouse.auth.enable or a private credentialFile.";
        }
        {
          assertion = !isServer || (cfg.uploader.maxInflightNars >= 1 && cfg.uploader.maxInflightNars <= 16);
          message = "services.stockhouse.uploader.maxInflightNars must be between 1 and 16.";
        }
        {
          assertion = cfg.uploader.backoffBase <= cfg.uploader.backoffMax;
          message = "services.stockhouse.uploader.backoffBase must not exceed backoffMax.";
        }
      ];

      systemd.services.stockhouse-uploader = {
        description = "Stockhouse post-build ingestion uploader";
        wantedBy = ["multi-user.target"];
        after = ["nix-daemon.socket"] ++ lib.optionals isServer (privateCredential.unitConfig.after or []);
        requires = lib.optionals isServer (privateCredential.unitConfig.requires or []);
        serviceConfig =
          hardening
          // lib.optionalAttrs isServer privateCredential.serviceConfig
          // {
            ReadWritePaths = uploaderWritable ++ lib.optionals isServer (privateCredential.serviceConfig.ReadWritePaths or []);
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

    (mkIf (cfg.uploader.enable && cfg.lease.enable && isServer) {
      # Never wanted by a target, so never part of boot or activation; the
      # timers below and the activation snippet only queue it.
      systemd.services.stockhouse-lease = {
        description = "Stockhouse lease renewal";
        after = ["network-online.target"] ++ (privateCredential.unitConfig.after or []);
        wants = ["network-online.target"];
        requires = privateCredential.unitConfig.requires or [];
        serviceConfig =
          hardening
          // privateCredential.serviceConfig
          // {
            Type = "oneshot";
            ExecStart = "${cfg.package}/bin/stockhouse lease --config ${leaseConfig}";
            User = "stockhouse";
            Group = "stockhouse";
            StateDirectory = "stockhouse";
            StateDirectoryMode = "0755";
            SuccessExitStatus = "75";
          };
      };

      systemd.timers.stockhouse-lease = {
        description = "Stockhouse lease renewal on boot and hourly";
        wantedBy = ["timers.target"];
        timerConfig = {
          OnBootSec = "1min";
          OnUnitActiveSec = "1h";
          RandomizedDelaySec = toString cfg.lease.randomizedDelaySec;
        };
      };

      # An existing calendar stamp can suppress an overdue OnBootSec trigger
      # when the schedules share a persistent timer.
      systemd.timers.stockhouse-lease-daily = {
        description = "Stockhouse daily lease renewal";
        wantedBy = ["timers.target"];
        timerConfig = {
          OnCalendar = "daily";
          Persistent = true;
          RandomizedDelaySec = toString cfg.lease.randomizedDelaySec;
          Unit = "stockhouse-lease.service";
        };
      };

      # Restarted by activation; fires once activation has replaced
      # /run/current-system (its last step).
      systemd.timers.stockhouse-lease-switch = {
        description = "Stockhouse lease renewal after activation";
        timerConfig = {
          OnActiveSec = "30s";
          AccuracySec = "1s";
          Unit = "stockhouse-lease.service";
        };
      };

      system.activationScripts.stockhouse-lease = {
        deps = [];
        text = ''
          ${config.systemd.package}/bin/systemctl restart --no-block stockhouse-lease-switch.timer || true
        '';
      };
    })
  ];
}
