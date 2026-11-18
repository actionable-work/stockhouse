# Module story: server sign-in generates a secret-free configuration, wires
# the sealed key and the token store into the proxy, and moves the proxy to
# the shared stockhouse user without losing hardening.
args: let
  m = import ./lib/module.nix args;
  inherit (m) lib evaluate hasFailure failed;
  inherit (args) pkgs;
  authBase = lib.recursiveUpdate m.base {
    proxy.private = {
      url = "https://cache.example.invalid/t/1/";
      credentialFile = null;
    };
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
  };
  sealed = evaluate m.default authBase;
  file = evaluate m.default (lib.recursiveUpdate authBase {auth.store.mode = "file";});
  proxyOf = cfg: cfg.systemd.services.stockhouse-proxy.serviceConfig;
  proxy = proxyOf sealed;
  withAuth = extra: evaluate m.default (lib.recursiveUpdate authBase extra);
  configFile = sealed.services.stockhouse.auth.configFile;
  proxyConfig = lib.last (lib.splitString " " proxy.ExecStart);
  keyLoad = ["stockhouse-token-key:/etc/stockhouse/token-key.cred"];
in
  assert failed sealed == [];
  # MODULE-010: the private tier uses the generated sign-in configuration.
  assert sealed.services.stockhouse.auth.unitCredential.toml.server == "${configFile}";
  # MODULE-011: the sealed key loads as an encrypted credential on the proxy.
  assert proxy.LoadCredentialEncrypted == keyLoad;
  # MODULE-012: file mode loads no key and defines no key oneshot.
  assert !((proxyOf file) ? LoadCredentialEncrypted);
  assert !(file.systemd.services ? stockhouse-token-key);
  # MODULE-013: the shared user, with hardening kept and made explicit.
  assert !proxy.DynamicUser && proxy.User == "stockhouse" && proxy.Group == "stockhouse";
  assert proxy.ProtectSystem == "strict" && proxy.NoNewPrivileges && proxy.PrivateTmp;
  assert proxy.ProtectHome && proxy.RemoveIPC && proxy.RestrictSUIDSGID && proxy.UMask == "0077";
  assert proxy.RestrictAddressFamilies == ["AF_INET" "AF_INET6" "AF_UNIX"];
  assert proxy.ReadWritePaths == ["/var/lib/stockhouse/auth"];
  assert lib.elem "d /var/lib/stockhouse/auth 0700 stockhouse stockhouse -" sealed.systemd.tmpfiles.rules;
  assert sealed.users.users.stockhouse.isSystemUser;
  # MODULE-014: inconsistent settings fail evaluation.
  assert hasFailure (withAuth {proxy.private.credentialFile = "/run/keys/token";}) "proxy.private without a credentialFile";
  assert hasFailure (evaluate m.default (lib.recursiveUpdate (removeAttrs authBase ["proxy"]) {proxy = removeAttrs authBase.proxy ["private"];})) "proxy.private without";
  assert hasFailure (withAuth {auth.server = "http://example.invalid/";}) "loopback IP literal";
  assert hasFailure (withAuth {auth.endpoints.token = "http://localhost/oauth/token";}) "loopback IP literal";
  assert hasFailure (withAuth {auth.endpoints.token = "http://127.0.0.1.example.invalid/t";}) "loopback IP literal";
  assert failed (withAuth {
    auth.server = "http://[::1]:8486/";
    auth.endpoints = lib.genAttrs ["authorization" "token" "deviceAuthorization" "revocation"] (n: "http://[::1]:8486/${n}");
    proxy.private.url = "http://127.0.0.1:8486/t/1/";
  })
  == [];
  assert hasFailure (withAuth {auth.store.keyCredentialFile = "/nix/store/abc-key";}) "keyCredentialFile";
  assert hasFailure (withAuth {auth.store.directory = "auth";}) "store.directory"; {
    module-auth = m.check "module-auth" {
      passthru.bloomery = [
        "CLIENT-INTEGRATION-MODULE-010"
        "CLIENT-INTEGRATION-MODULE-011"
        "CLIENT-INTEGRATION-MODULE-012"
        "CLIENT-INTEGRATION-MODULE-013"
        "CLIENT-INTEGRATION-MODULE-014"
      ];
      script = ''
        # MODULE-010: the generated files parse and hold no secret-looking field.
        ${pkgs.python3}/bin/python3 - ${configFile} ${proxyConfig} ${file.services.stockhouse.auth.configFile} <<'PY'
        import sys, tomllib
        auth, proxy, plain = (tomllib.load(open(p, "rb")) for p in sys.argv[1:])
        assert auth["private_tier"] == "https://cache.example.invalid/t/1/", auth
        assert auth["endpoints"]["device_authorization"] == "https://example.invalid/oauth/device"
        assert auth["store"] == {"mode": "sealed", "directory": "/var/lib/stockhouse/auth", "key_credential": "stockhouse-token-key"}
        assert proxy["private"]["credential"] == {"server": sys.argv[1]}, proxy
        # MODULE-011/012: the key name only in sealed mode.
        assert plain["store"] == {"mode": "file", "directory": "/var/lib/stockhouse/auth"}, plain
        text = open(sys.argv[1]).read() + open(sys.argv[2]).read()
        for word in ("access_token", "refresh_token", "secret", "password"):
            assert word not in text, word
        PY
      '';
    };
  }
