# Module story: the stockhouse-auth wrapper runs only the sign-in commands as
# the service user, the sealing key is created when absent, and units get the
# token through one shared value.
args: let
  m = import ./lib/module.nix args;
  inherit (m) lib evaluate;
  inherit (args) pkgs;
  authBase = lib.recursiveUpdate m.base {
    proxy.private = {
      url = "https://cache.example.invalid/t/1/";
      credentialFile = null;
    };
    auth = {
      enable = true;
      server = "https://example.invalid/";
      endpoints = lib.genAttrs ["authorization" "token" "deviceAuthorization" "revocation"] (n: "https://example.invalid/oauth/${n}");
      clientId = "stockhouse-cli";
      scopes = ["cache:read"];
    };
  };
  cfg = evaluate m.default authBase;
  auth = cfg.services.stockhouse.auth;
  noKey = evaluate m.default (lib.recursiveUpdate authBase {auth.store.createKey = false;});
  keyUnit = cfg.systemd.services.stockhouse-token-key;
  proxyUnit = cfg.systemd.services.stockhouse-proxy;
  systemdRun = "${cfg.systemd.package}/bin/systemd-run";
in
  # MODULE-016: the key oneshot exists only with createKey, before the proxy.
  assert keyUnit.serviceConfig.Type == "oneshot";
  assert keyUnit.unitConfig.ConditionPathExists == "!/etc/stockhouse/token-key.cred";
  assert lib.elem "stockhouse-token-key.service" proxyUnit.after;
  assert lib.elem "stockhouse-token-key.service" proxyUnit.requires;
  assert !(noKey.systemd.services ? stockhouse-token-key);
  # MODULE-017: one value wires a unit to the token, applied to the proxy.
  assert auth.unitCredential.toml == {server = "${auth.configFile}";};
  assert auth.unitCredential.serviceConfig.ReadWritePaths == ["/var/lib/stockhouse/auth"];
  assert auth.unitCredential.serviceConfig.LoadCredentialEncrypted == ["stockhouse-token-key:/etc/stockhouse/token-key.cred"];
  assert auth.unitCredential.unitConfig.requires == ["stockhouse-token-key.service"];
  assert proxyUnit.serviceConfig.LoadCredentialEncrypted == auth.unitCredential.serviceConfig.LoadCredentialEncrypted;
  # MODULE-015: the wrapper is installed.
  assert lib.elem auth.wrapper cfg.environment.systemPackages; {
    module-auth-wrapper = m.check "module-auth-wrapper" {
      passthru.bloomery = [
        "CLIENT-INTEGRATION-MODULE-015"
        "CLIENT-INTEGRATION-MODULE-016"
        "CLIENT-INTEGRATION-MODULE-017"
      ];
      script = ''
        set -eu
        wrapper=${auth.wrapper}/bin/stockhouse-auth
        # MODULE-016: the key script encrypts 64 hex characters only when absent.
        grep -q 'if \[ -e "$key" \]' ${keyUnit.serviceConfig.ExecStart}
        grep -q 'systemd-creds encrypt --name=stockhouse-token-key' ${keyUnit.serviceConfig.ExecStart}
        grep -qx 'set -euo pipefail' ${keyUnit.serviceConfig.ExecStart}
        grep -q 'rm -f "$key.tmp"' ${keyUnit.serviceConfig.ExecStart}
        grep -q 'mv -n "$key.tmp" "$key"' ${keyUnit.serviceConfig.ExecStart}
        # MODULE-015: run the wrapper against a stub systemd-run.
        stub=$PWD/systemd-run
        printf '#!${pkgs.runtimeShell}\nprintf "%%s\\n" "$@" > %s/argv\n' "$PWD" > "$stub"
        chmod +x "$stub"
        sed -e "s|${systemdRun}|$stub|" -e 's|"$(.*/bin/id -u)"|0|' "$wrapper" > ./wrapper
        chmod +x ./wrapper
        ! grep -q 'PrivateNetwork\|IPAddressDeny' "$wrapper"
        for bad in "" "foo" "login --token x" "logout extra" "login --device --device"; do
          # shellcheck disable=SC2086
          if ./wrapper $bad 2>/dev/null; then echo "accepted: $bad"; exit 1; fi
          # shellcheck disable=SC2086
          set +e; ./wrapper $bad 2>/dev/null; code=$?; set -e
          [ "$code" = 2 ] || { echo "exit $code for: $bad"; exit 1; }
        done
        for good in "login" "login --device" "logout" "refresh" "status"; do
          # shellcheck disable=SC2086
          ./wrapper $good
          grep -qx -- '--pipe' argv && grep -qx -- '--wait' argv
          grep -qx 'User=stockhouse' argv
          grep -qx 'LoadCredentialEncrypted=stockhouse-token-key:/etc/stockhouse/token-key.cred' argv
          grep -qx 'ReadWritePaths=/var/lib/stockhouse/auth' argv
          grep -qx -- '--config' argv
          tail -n 1 argv | grep -qx '${auth.configFile}'
          # shellcheck disable=SC2086
          set -- $good
          grep -qx -- "$1" argv
        done
      '';
    };
  }
