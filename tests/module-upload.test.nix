# Module story: the server backend uploads to the private tier with its
# credential loaded by systemd.
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
  uploaderFile = lib.last (lib.splitString " " uploader.ExecStart);
  withPrivate = private: lib.recursiveUpdate server {proxy.private = private;};
  private = base.proxy.private;
  # With server sign-in the uploader uses the signed-in token instead.
  signedIn = evaluate m.default (lib.recursiveUpdate server {
    proxy.private.credentialFile = null;
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
      scopes = ["cache:read" "cache:write"];
    };
  });
  signedInUnit = signedIn.systemd.services.stockhouse-uploader;
  signedInLease = signedIn.systemd.services.stockhouse-lease;
  unitCredential = signedIn.services.stockhouse.auth.unitCredential;
  signedInFile = lib.last (lib.splitString " " signedInUnit.serviceConfig.ExecStart);
in
  # MODULE-018: the uploader loads the private tier's credential through systemd.
  assert m.failed config == [];
  assert uploader.LoadCredential == ["private:/run/keys/stockhouse-private"];
  # MODULE-018: with sign-in, the uploader and lease units use the signed-in token.
  assert m.failed signedIn == [];
  assert signedInUnit.serviceConfig.User == "stockhouse" && !(signedInUnit.serviceConfig ? LoadCredential);
  assert signedInUnit.serviceConfig.LoadCredentialEncrypted == unitCredential.serviceConfig.LoadCredentialEncrypted;
  assert signedInLease.serviceConfig.LoadCredentialEncrypted == unitCredential.serviceConfig.LoadCredentialEncrypted;
  assert lib.all (p: lib.elem p signedInUnit.serviceConfig.ReadWritePaths) unitCredential.serviceConfig.ReadWritePaths;
  assert lib.all (u: lib.elem u signedInUnit.requires && lib.elem u signedInLease.requires) (unitCredential.unitConfig.requires or []);
  # MODULE-019: the server backend needs a protected, credentialed private tier.
  assert hasFailure (evaluate m.default (withPrivate (private // {url = "https://cache.example.invalid/t/1";}))) "uploader.backend";
  assert hasFailure (evaluate m.default (lib.recursiveUpdate base {
    uploader.backend = "server";
    proxy.private = null;
    proxy.public = base.proxy.public;
  })) "uploader.backend";
  assert hasFailure (evaluate m.default (withPrivate (private // {credentialFile = null;}))) "uploader.backend";
  assert hasFailure (evaluate m.default (withPrivate (private // {url = "http://cache.example.invalid/t/1/";}))) "uploader.backend";
  assert m.failed (evaluate m.default (withPrivate (private // {url = "http://127.0.0.1:8486/t/1/";}))) == [];
  # MODULE-024: backoff options reach the uploader, and base must not exceed max.
  assert hasFailure (evaluate m.default (lib.recursiveUpdate base {
    uploader = {
      backoffBase = 10;
      backoffMax = 5;
    };
  })) "backoffBase";
  # MODULE-026: substitution errors fall back to local builds.
  assert config.nix.settings.fallback;
  # MODULE-025: narinfo misses are cached briefly, and a configuration can override it.
  assert config.nix.settings.narinfo-cache-negative-ttl == 60;
  assert (evaluate m.default (lib.recursiveUpdate server {proxy.narinfoCacheNegativeTtl = 5;})).nix.settings.narinfo-cache-negative-ttl == 5; {
    module-upload = m.check "module-upload" {
      passthru.bloomery = [
        "CLIENT-INTEGRATION-MODULE-018"
        "CLIENT-INTEGRATION-MODULE-019"
        "CLIENT-INTEGRATION-MODULE-024"
        "CLIENT-INTEGRATION-MODULE-025"
        "CLIENT-INTEGRATION-MODULE-026"
      ];
      script = ''
        # MODULE-018: the backend is the private tier, naming only the systemd credential.
        grep -q 'kind = "server"' ${uploaderFile}
        grep -q 'base_url = "https://cache.example.invalid/org/"' ${uploaderFile}
        grep -q 'systemd = "private"' ${uploaderFile}
        ! grep -q 'stockhouse-private' ${uploaderFile}
        # MODULE-018: with sign-in the backend names the sign-in configuration.
        grep -q 'server = "${unitCredential.toml.server}"' ${signedInFile}
        # MODULE-024: backoff values are written.
        grep -q 'backoff_base_secs = 2' ${uploaderFile}
        grep -q 'backoff_max_secs = 30' ${uploaderFile}
      '';
    };
  }
