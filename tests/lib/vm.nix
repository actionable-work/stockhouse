# Shared NixOS VM fixture for the client qualification stories. Each story
# runs once per pinned Nix version, used as both client and daemon. The VM
# has no network. Its signing key is a synthetic fixture: the secret is
# assembled at runtime from a public constant seed and is trusted only inside
# this VM.
{
  pkgs,
  module,
}: let
  inherit (pkgs) lib;
  keyName = "stockhouse-vm-fixture-1";
  publicKey = "UpT9w0BGYiXPBfpdNiWwBf1b3LAwvtjoUHLlbFaOM3E=";
  # sha256("stockhouse-vm-fixture-key-v1"); public, synthetic, test-only.
  seed = "aa6ee2469379313f381ccfc5e7767b0911677391d44edcecbead9d28e51094ac";
  deniedHash = "00000000000000000000000000000403";
  failingHash = "00000000000000000000000000000500";

  sharedFixture = pkgs.writeText "stockhouse-shared-fixture.py" ''
    import http.server

    LOG = "/var/lib/stockhouse-vm/shared-requests.log"
    REPLIES = {"/nix-cache-info": 200, "/${deniedHash}.narinfo": 403, "/${failingHash}.narinfo": 500}


    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            with open(LOG, "a") as log:
                log.write(f"{self.path} {self.headers.get('Authorization', '-')}\n")
            code = REPLIES.get(self.path, 404)
            body = b"StoreDir: /nix/store\n" if code == 200 else b""
            self.send_response(code)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *args):
            pass


    http.server.HTTPServer(("127.0.0.1", 8485), Handler).serve_forever()
  '';

  # The server-backend variant: the private tier is the local fake protocol
  # v1 server, which the uploader and lease client talk to.
  serverNode = {
    systemd.services.stockhouse-vm-fake = {
      wantedBy = ["multi-user.target"];
      before = ["stockhouse-proxy.service" "stockhouse-uploader.service"];
      after = ["stockhouse-vm-token.service"];
      requires = ["stockhouse-vm-token.service"];
      serviceConfig = {
        ExecStart = "${pkgs.python3}/bin/python3 ${./upload_server.py} /var/lib/stockhouse-vm/private-token";
        StateDirectory = "stockhouse-fake";
      };
    };
    systemd.services.stockhouse-uploader = {
      after = ["stockhouse-vm-token.service"];
      requires = ["stockhouse-vm-token.service"];
    };
    systemd.services.stockhouse-lease.after = ["stockhouse-vm-token.service"];
    services.stockhouse = {
      proxy.private = lib.mkForce {
        url = "http://127.0.0.1:8486/t/1/";
        trustedKeys = ["${keyName}:${publicKey}"];
        credentialFile = "/var/lib/stockhouse-vm/private-token";
      };
      uploader = {
        backend = lib.mkForce "server";
        nixCopyDestination = lib.mkForce null;
        backoffMax = 10;
        maxPending = lib.mkForce 32;
        # No rescans: queued paths reach the server only as records or
        # through a closure, so the stories see exactly those uploads.
        scanner = lib.mkForce "none";
      };
      lease.randomizedDelaySec = 0;
    };
  };

  node = nixPackage: extraModule: {config, ...}: {
    imports = [module extraModule];
    nix.package = nixPackage;
    nix.settings.secret-key-files = ["/etc/stockhouse-vm/key.sec"];
    # Builds query the proxy before building; do not cache those misses.
    nix.settings.narinfo-cache-negative-ttl = 0;
    # Keep built paths across reboots.
    virtualisation.writableStoreUseTmpfs = false;
    virtualisation.memorySize = 2048;
    environment.systemPackages = [config.services.stockhouse.package pkgs.curl];

    systemd.services.stockhouse-vm-token = {
      requiredBy = ["stockhouse-proxy.service"];
      before = ["stockhouse-proxy.service"];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
      };
      script = ''
        install -d -m 0700 /var/lib/stockhouse-vm
        for name in shared-token private-token; do
          if [ ! -f /var/lib/stockhouse-vm/$name ]; then
            (umask 077; head -c 24 /dev/urandom | ${pkgs.coreutils}/bin/base64 | tr -d '/+=\n' > /var/lib/stockhouse-vm/$name)
          fi
        done
      '';
    };
    systemd.services.stockhouse-vm-shared = {
      wantedBy = ["multi-user.target"];
      after = ["stockhouse-vm-token.service"];
      requires = ["stockhouse-vm-token.service"];
      serviceConfig.ExecStart = "${pkgs.python3}/bin/python3 ${sharedFixture}";
    };

    services.stockhouse = {
      proxy = {
        enable = true;
        private = {
          url = "file:///var/lib/stockhouse/private-cache/";
          trustedKeys = ["${keyName}:${publicKey}"];
        };
        shared = {
          url = "http://127.0.0.1:8485/";
          trustedKeys = ["${keyName}:${publicKey}"];
          credentialFile = "/var/lib/stockhouse-vm/shared-token";
        };
      };
      uploader = {
        enable = true;
        backend = "nix-copy";
        nixCopyDestination = "file:///var/lib/stockhouse/private-cache";
        maxPending = 3;
        pollInterval = 1;
      };
    };
    specialisation.upgraded.configuration.services.stockhouse.proxy.priority = lib.mkForce 45;
  };

  # Helpers and boot steps shared by every story.
  prelude = ''
    import base64
    import json

    QUEUE = "/var/lib/stockhouse/queue"
    CACHE = "/var/lib/stockhouse/private-cache"
    PROXY = "http://127.0.0.1:8484"
    KEY_NAME = "${keyName}"
    DENIED_HASH = "${deniedHash}"
    FAILING_HASH = "${failingHash}"
    secret = base64.b64encode(bytes.fromhex("${seed}") + base64.b64decode("${publicKey}")).decode()


    def http_code(path):
        return machine.succeed(f"curl -s -o /dev/null -w '%{{http_code}}' {PROXY}{path}").strip()


    def build(tag):
        expr = f'derivation {{ name = "vm-{tag}"; system = builtins.currentSystem; builder = "/bin/sh"; args = [ "-c" "echo {tag} > $out" ]; }}'
        return machine.succeed(f"nix-build --no-out-link -E '{expr}'").strip()


    def store_hash(path):
        return path.split("/")[-1][:32]


    def status():
        return json.loads(machine.succeed(f"stockhouse status --queue {QUEUE}"))


    def wait_uploaded(path):
        machine.wait_until_succeeds(f"test -f {QUEUE}/done/{store_hash(path)}.json", timeout=120)


    FAKE = "http://127.0.0.1:8486"


    def fake(path="/__fake/state"):
        return json.loads(machine.succeed(f"curl -sf '{FAKE}{path}'"))


    def fixture(tag):
        """A daemon-signed path copied into the fake's private cache and deleted locally."""
        expr = f'derivation {{ name = "vm-{tag}"; system = builtins.currentSystem; builder = "/bin/sh"; args = [ "-c" "echo {tag} > $out" ]; }}'
        path = machine.succeed(f"nix-build --option substituters ''' --no-out-link -E '{expr}'").strip()
        machine.succeed(f"nix --extra-experimental-features nix-command copy --to file:///var/lib/stockhouse-vm/server-cache {path}")
        machine.succeed(f"nix-store --delete {path}")
        return path


    def device_login():
        machine.succeed("rm -f /tmp/login.out")
        machine.succeed("setsid stockhouse-auth login --device < /dev/null > /tmp/login.out 2>&1 &")
        machine.wait_until_succeeds("grep -q 'enter the code' /tmp/login.out", timeout=60)
        code = machine.succeed("sed -n 's/.*enter the code: //p' /tmp/login.out").strip()
        fake(f"/__fake/approve?user_code={code}")
        machine.wait_until_succeeds("grep -q 'Signed in.' /tmp/login.out", timeout=60)


    def signed_in():
        code, out = machine.execute("stockhouse-auth status")
        return code == 0 and json.loads(out)["signed_in"]


    UPLOAD_STATE = "/var/lib/stockhouse-fake"


    def fake_log():
        return machine.succeed(f"cat {UPLOAD_STATE}/requests.log 2>/dev/null || true").splitlines()


    def fake_lines(h):
        return [line for line in fake_log() if f" {h} " in f" {line} "]


    def control(name, on=True):
        if on:
            machine.succeed(f"touch {UPLOAD_STATE}/control/{name}")
        else:
            machine.succeed(f"rm -f {UPLOAD_STATE}/control/{name}")


    def pending_record(path):
        return json.loads(machine.succeed(f"cat {QUEUE}/pending/{store_hash(path)}.json"))


    def build_expr(expr):
        return machine.succeed(f"nix-build --no-out-link -E '{expr}'").strip()


    def drv(tag, refs=()):
        body = " ".join("''${" + r + "}" for r in refs)
        return f'derivation {{ name = "vm-{tag}"; system = builtins.currentSystem; builder = "/bin/sh"; args = [ "-c" "echo {tag} {body} > $out" ]; }}'


    def wait_drained():
        machine.wait_until_succeeds(f"test $(ls {QUEUE}/pending | wc -l) -eq 0", timeout=120)


    machine.start()
    machine.wait_for_unit("multi-user.target")
    machine.succeed("install -d -m 0755 /etc/stockhouse-vm")
    machine.succeed(f"(umask 077; printf '%s' '{KEY_NAME}:{secret}' > /etc/stockhouse-vm/key.sec)")
    machine.succeed(f"install -d -o stockhouse -g stockhouse -m 0755 {CACHE}")
    machine.succeed(f"printf 'StoreDir: /nix/store\\n' > {CACHE}/nix-cache-info && chown stockhouse:stockhouse {CACHE}/nix-cache-info")
    machine.systemctl("restart nix-daemon")
    machine.wait_for_unit("stockhouse-proxy.service")
    machine.wait_for_unit("stockhouse-uploader.service")
    machine.wait_for_open_port(8484)
  '';

  # The sign-in node module: fake OAuth server and sealed server sign-in.
  authModule = import ./auth.nix {inherit pkgs lib keyName publicKey;};

  nixVersions = {
    "nix-2_34" = pkgs.nixVersions.nix_2_34;
    "nix-2_31" = pkgs.nixVersions.nix_2_31;
  };
in {
  # One check per pinned Nix version: `vm-<story>-<nix version>`.
  story = story: {
    script,
    passthru,
    signIn ? false,
    backend ? "nix-copy",
  }:
    lib.mapAttrs' (version: nixPackage:
      lib.nameValuePair "vm-${story}-${version}" (pkgs.testers.runNixOSTest {
        name = "stockhouse-vm-${story}-${version}";
        nodes.machine = {
          imports =
            [
              (node nixPackage (
                if signIn
                then authModule
                else {}
              ))
            ]
            ++ lib.optional (backend == "server") serverNode;
        };
        testScript = prelude + script;
        inherit passthru;
      }))
    nixVersions;
}
