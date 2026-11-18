# Extra node module for the sign-in VM stories: the fake OAuth server and
# private cache, and server sign-in in sealed mode. The sealing key is
# created by the module's own oneshot with the host key (no TPM2 in the VM).
{
  pkgs,
  lib,
  keyName,
  publicKey,
}: {
  systemd.services.stockhouse-vm-fake = {
    wantedBy = ["multi-user.target"];
    before = ["stockhouse-proxy.service"];
    after = ["stockhouse-vm-token.service"];
    requires = ["stockhouse-vm-token.service"];
    serviceConfig.ExecStart = "${pkgs.python3}/bin/python3 ${./fake_server.py}";
  };
  # Restore Nix's default so the no-negative-cache check means something.
  nix.settings.narinfo-cache-negative-ttl = lib.mkForce 3600;
  services.stockhouse = {
    proxy.private = lib.mkForce {
      url = "http://127.0.0.1:8486/t/1/";
      trustedKeys = ["${keyName}:${publicKey}"];
    };
    auth = {
      enable = true;
      server = "http://127.0.0.1:8486/";
      endpoints = {
        authorization = "http://127.0.0.1:8486/oauth/authorize";
        token = "http://127.0.0.1:8486/oauth/token";
        deviceAuthorization = "http://127.0.0.1:8486/oauth/device";
        revocation = "http://127.0.0.1:8486/oauth/revoke";
      };
      clientId = "stockhouse-vm";
      scopes = ["cache:read"];
    };
  };
}
