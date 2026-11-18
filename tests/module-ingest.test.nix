# Module story: credentials load through systemd and never as values, and the
# uploader and post-build hook are installed.
args: let
  m = import ./lib/module.nix args;
  inherit (m) lib proxy uploader evaluate withTier hasFailure;
  private = m.base.proxy.private;
in
  # MODULE-003: the credential file loads through LoadCredential.
  assert proxy.LoadCredential == ["private:/run/keys/stockhouse-private"];
  # MODULE-004: store-path and relative credential files fail evaluation.
  assert hasFailure (evaluate m.default (withTier (private // {credentialFile = "keys/token";}))) "private.credentialFile";
  assert hasFailure (evaluate m.default (withTier (private // {credentialFile = "/nix/store/abc-token";}))) "private.credentialFile";
  # MODULE-007: the uploader runs as its own system user.
  assert uploader.User == "stockhouse" && uploader.StateDirectory == "stockhouse" && uploader.Restart == "always";
  assert lib.hasInfix "/bin/stockhouse uploader --config /nix/store/" uploader.ExecStart;
  assert m.config.users.users.stockhouse.isSystemUser; {
    module-ingest = m.check "module-ingest" {
      passthru.bloomery = [
        "CLIENT-INTEGRATION-MODULE-003"
        "CLIENT-INTEGRATION-MODULE-004"
        "CLIENT-INTEGRATION-MODULE-007"
        "CLIENT-INTEGRATION-MODULE-008"
      ];
      script = ''
        # MODULE-003: the generated configuration names the systemd credential only.
        grep -q 'systemd = "private"' ${m.configFile}
        ! grep -q 'stockhouse-private' ${m.configFile}
        # MODULE-008: the hook script runs the hook on the queue and ignores failure.
        grep -q 'post-build-hook --queue /var/lib/stockhouse/queue || true' ${m.config.nix.settings.post-build-hook}
      '';
    };
  }
