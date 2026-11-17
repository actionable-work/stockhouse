# Integration design

## Thin binary

`packages/rust/bins/stockhouse` holds only `Cargo.toml` and `src/main.rs`.
`main.rs` is exactly
`fn main() -> std::process::ExitCode { stockhouse_cli::main(std::env::args().collect()) }`
and the crate depends only on `stockhouse-cli`. Behavior lives in library
crates with their own component tests.

## Commands

`stockhouse_cli::main(Vec<String>) -> ExitCode` dispatches:

| Command | Behavior | Exit |
| --- | --- | --- |
| `proxy --config <file>` | run the substitution proxy | 2 on configuration error, 1 on serve error |
| `uploader --config <file> [--once]` | create the queue and run uploader passes (one with `--once`) | 2 on configuration error, 1 on queue error |
| `post-build-hook --queue <dir>` | run the post-build hook | always 0 |
| `status --queue <dir>` | print queue status as one JSON object | 1 if the queue cannot be opened |
| `login`, `logout`, `refresh` | the deferred grant flow | 1, naming P2.06 |
| `--version` | print `stockhouse <version> (protocol <n>)` | 0 |

Unknown commands, unknown flags and missing values print usage to stderr and
exit 2. No flag accepts a credential value; credentials reach the client only
through the configured credential sources.

## NixOS module

The flake exports `nixosModules.default` (also `nixosModules.client`) with
options under `services.stockhouse`:

- `package`: the `stockhouse` package from this flake.
- `proxy.enable`, `proxy.address` (default `127.0.0.1`), `proxy.port`
  (default 8484), `proxy.priority` (default 30), `proxy.configureNix`
  (default true), and `proxy.private`, `proxy.shared`, `proxy.public`, each
  `null` or `{ url, trustedKeys, credentialFile ? null }`.
- `uploader.enable`, `uploader.queueDir` (default `/var/lib/stockhouse/queue`),
  `uploader.maxPending`, `uploader.maxAttempts`, `uploader.pollInterval`,
  `uploader.rescanInterval`, `uploader.backend` (`deferred` or `nix-copy`),
  `uploader.nixCopyDestination`, `uploader.scanner` and
  `uploader.configureHook` (default true).

Behavior:

- The proxy runs as `stockhouse-proxy.service` with `DynamicUser`,
  `ExecStart = stockhouse proxy --config <generated TOML>`,
  `Restart = on-failure` and sandboxing (`ProtectSystem = strict`,
  `NoNewPrivileges`, `PrivateTmp`, `RestrictAddressFamilies` limited to
  `AF_INET AF_INET6 AF_UNIX`).
- A tier `credentialFile` is a string path loaded with
  `LoadCredential = <tier>:<path>`; the generated configuration contains only
  `credential = { systemd = "<tier>" }`. The module asserts the path is
  absolute and not under `/nix/store`, so the credential value never enters
  an option value, derivation, store file or process argument.
- The module asserts that `proxy.address` is a loopback address.
- With `configureNix`, `nix.settings.substituters` is forced (`mkForce`) to
  the proxy URL alone, and every tier's trusted keys are added to
  `trusted-public-keys`. Nix silently skips a substituter it has disabled
  after errors, so any second substituter (such as the default
  `cache.nixos.org`) would let a proxy 502 fall through; the public tier lives
  inside the proxy instead. An IPv6 `address` is written as `http://[::1]:<port>`.
- The source of a `credentialFile` is copied by systemd as root, so the
  operator must protect it (for example `root:root 0400`); the client checks
  only the copy in `$CREDENTIALS_DIRECTORY`.
- The uploader runs as `stockhouse-uploader.service` under the static
  `stockhouse` user with `StateDirectory = stockhouse`, `Restart = always`,
  `ExecStart = stockhouse uploader --config <generated TOML>`.
- `uploader.enable` defaults to false. With the default `deferred` backend, records
  stay pending until P2.08 provides server upload.
- With `configureHook`, `nix.settings.post-build-hook` is a store script that
  runs `stockhouse post-build-hook --queue <queueDir> || true`; the capacity
  comes from the queue's `LIMITS`.

## Supported systems

| System | Status |
| --- | --- |
| `x86_64-linux`, NixOS with systemd, from this flake's pinned nixpkgs | supported; VM-qualified |
| `aarch64-linux` | not claimed; not built or tested |
| other Linux distributions, non-systemd init | not claimed |
| macOS/Darwin and other operating systems | not supported (DR13) |

The flake exposes outputs for `x86_64-linux` only.

## Nix version matrix

The VM tests run the whole client against two repo-pinned Nix versions, each
used as both the client and the daemon (`nix.package`):

- Nix 2.34.8 (`nixVersions.nix_2_34`, nixpkgs' current stable);
- Nix 2.31.5 (`nixVersions.nix_2_31`, the oldest pinned release still in
  nixpkgs).

## VM qualification

`tests/vm.test.nix` defines one NixOS VM test per pinned Nix version. A
fixture signing key is written in the VM at runtime from a constant public
seed and set as the daemon's `secret-key-files`, so locally built paths are
signed. It is synthetic and trusted only inside the test VM. The VM has no
network, a `file://` private tier that is also the `nix-copy` destination, and
a loopback HTTP shared-tier fixture. The fixture records request headers,
answers 403 and 500 for two chosen hashes and 404 otherwise, so denied and
unavailable tiers are exercised through real Nix and the real proxy.

`tests/module.test.nix` evaluates the module without a VM to check generated
units, configuration and assertions.
