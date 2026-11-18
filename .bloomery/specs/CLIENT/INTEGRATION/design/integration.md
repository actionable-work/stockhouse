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
| `status --config <auth.toml>` | print the [session status](../../CREDS/design/oauth.md#status) as one JSON object | 0 when signed in, 1 otherwise, 2 on configuration error |
| `login --config <auth.toml> [--device]` | [sign in](../../CREDS/design/oauth.md#login) with the loopback flow, or the device flow with `--device`; prompts go to stdout | 0 when signed in, 1 on failure, 2 on configuration error |
| `logout --config <auth.toml>` | [revoke and delete](../../CREDS/design/oauth.md#logout) the stored tokens | 0 when revoked and deleted or not signed in, 1 when revocation or deletion failed (the store is still deleted after a failed revocation), 2 on configuration error |
| `refresh --config <auth.toml>` | [force one refresh](../../CREDS/design/oauth.md#explicit-refresh) | 0 when refreshed, 1 when signed out or failed, 2 on configuration error |
| `lease --config <file>` | renew this machine's leases (slots 1, 2 and extras) | 0, 75 (some slot not renewed), 1 (machine identity), 2 (configuration) |
| `--version` | print `stockhouse <version> (protocol <n>)` | 0 |

`status` takes exactly one of `--queue` and `--config`. Unknown commands,
unknown flags and missing values print usage to stderr and exit 2. No flag
accepts a credential value; credentials reach the client only through the
configured credential sources and the token store. The sign-in commands print
the authorization URL, the verification URI and the user code, and messages
naming outcomes; they never print a token, code, verifier, state, device code
or key. The groundwork's deferred `login`/`logout`/`refresh` stubs
(`CLIENT-INTEGRATION-CLI-004`) are retired.

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
  `uploader.rescanInterval`, `uploader.backend` (`deferred`, `nix-copy` or
  `server`), `uploader.nixCopyDestination`, `uploader.scanner`,
  `uploader.configureHook` (default true), `uploader.backoffBase` (default
  5) and `uploader.backoffMax` (default 3600).
- `lease.enable` (default true when the uploader is enabled with
  `uploader.backend = "server"`),
  `lease.randomizedDelaySec` (default 600) and `lease.extraSlots` (a list
  of `{ slot, path }` with slots 3–10).

Behavior:

- Without server sign-in (`auth.enable = false`), the proxy runs as
  `stockhouse-proxy.service` with `DynamicUser`,
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
- `uploader.enable` defaults to false. With the default `deferred` backend,
  records stay pending until a backend (normally `server`) is configured.
- With `configureHook`, `nix.settings.post-build-hook` is a store script that
  runs `stockhouse post-build-hook --queue <queueDir> || true`; the capacity
  comes from the queue's `LIMITS`.
- `backoffBase` and `backoffMax` become `backoff_base_secs` and
  `backoff_max_secs`; the module asserts `backoffBase <= backoffMax`.
- With `configureNix`, `nix.settings.fallback` defaults (`mkDefault`) to
  `true`: a proxy 503 or 502 then fails only the substitution, and Nix builds
  the path locally, so sign-out and server outages never block builds.
- With `configureNix`, `nix.settings.narinfo-cache-negative-ttl` defaults
  (`mkDefault`) to `proxy.narinfoCacheNegativeTtl`, 60 seconds instead of
  Nix's 3600. The server signs uploaded paths after publication, so a path
  can read as a miss briefly; a short TTL keeps that miss from being cached
  for an hour.

### Server upload and leases

With `uploader.backend = "server"` the server is the private tier: the
generated backend is `kind = "server"` with `base_url` equal to
`proxy.private.url`, the module's `nix`, and the private tier's credential.
That credential is the module's single `privateCredential` definition,
shared by the uploader and lease units:

- with `services.stockhouse.auth.enable`, the internal value
  `services.stockhouse.auth.unitCredential` defined by server sign-in:
  its `toml` (`credential = { server = "<auth configuration file>" }`) goes
  into the generated configuration and its `serviceConfig`
  (`LoadCredentialEncrypted`, `ReadWritePaths` for the token store) is merged
  into each unit;
- otherwise `credential = { systemd = "private" }` in the generated
  configuration plus `LoadCredential = private:<proxy.private.credentialFile>`
  on each unit.

Either way the credential value never enters an option, derivation, store
file or argument. Assertions: `proxy.private` is set; its URL starts with
`https://`, or with `http://` and a loopback IP literal host, and ends
in `/`; and
`auth.enable` is true or `proxy.private.credentialFile` is set. Sign-in
derives its private tier from `proxy.private.url`, so the audience check of
the uploader and lease client passes by construction.

The uploader and lease units share one hardening set: `ProtectSystem =
strict`, `ProtectHome`, `PrivateTmp`, `NoNewPrivileges` and
`RestrictAddressFamilies = AF_INET AF_INET6 AF_UNIX`, with writes limited to
`StateDirectory = stockhouse` and, for the uploader, `ReadWritePaths` of the
queue directory and any `nix-copy` destination (`-`-prefixed, so a path that
does not exist yet does not stop the unit; plus the token store paths sign-in
adds), so a non-default queue or destination stays writable.

With `lease.enable`:

- `stockhouse-lease.service` is `Type = oneshot` under the `stockhouse` user
  with `StateDirectory = stockhouse`, the hardening set, the private
  credential, `ExecStart = stockhouse lease --config <generated TOML>` and
  `SuccessExitStatus = 75`. Its configuration has `base_url` and
  `credential` as above, `machine_id_file = /var/lib/stockhouse/machine-id`,
  the default `current_system` and `system_profile`, and one `[[extra]]` per
  `lease.extraSlots` entry. The service has no `wantedBy` and nothing
  requires or orders itself after it, so it is never part of the boot or
  activation transaction.
- `stockhouse-lease.timer` (`wantedBy = timers.target`) is the backstop: it
  starts the service with `OnBootSec = 1min`, `OnUnitActiveSec = 1h`,
  `OnCalendar = daily`, `Persistent = true` and `RandomizedDelaySec =
  lease.randomizedDelaySec` (default 600, so a fleet does not renew in the
  same second).
- After activation: an activation snippet runs
  `systemctl restart --no-block stockhouse-lease-switch.timer || true`.
  `stockhouse-lease-switch.timer` has no `wantedBy`, `OnActiveSec = 30s` and
  `AccuracySec = 1s`, and starts `stockhouse-lease.service`. The delay lets
  activation finish replacing `/run/current-system` (its last step) before
  slot 1 is read, and `--no-block` and `|| true` keep the snippet from
  waiting on or failing activation, including during boot, when systemd is
  not running yet.
- The module asserts that `lease.enable` implies the uploader is enabled with
  `uploader.backend = "server"`, and that `extraSlots` slots are distinct and
  within 3–10 with absolute paths.

No VM story uses the sign-in credential for the uploader or lease client;
they use the `LoadCredential` path. Sign-in itself is covered by its own
stories, and end to end the private server's drill covers the combination.

## Server sign-in

Options under `services.stockhouse.auth`:

| Option | Type, default | Meaning |
| --- | --- | --- |
| `enable` | bool, false | sign the private tier in to the server |
| `server` | str | server origin, for example `https://example.invalid/` |
| `endpoints.authorization`, `endpoints.token`, `endpoints.deviceAuthorization`, `endpoints.revocation` | str | absolute endpoint URLs on that origin |
| `clientId` | str | the public client ID |
| `scopes` | list of str | requested scopes |
| `requestTimeout` | int, 30 | endpoint timeout in seconds |
| `store.mode` | `"sealed"` or `"file"`, `"sealed"` | token store protection |
| `store.directory` | str, `/var/lib/stockhouse/auth` | token store directory |
| `store.keyCredentialFile` | str, `/etc/stockhouse/token-key.cred` | runtime path of the `systemd-creds encrypt` output holding the sealing key (sealed mode; ignored in file mode) |
| `store.createKey` | bool, true | create the key file at boot if it is absent (sealed mode) |
| `configFile` | read-only path | the generated sign-in configuration |
| `unitCredential` | read-only, internal | what a unit needs to use the token (below) |

The private tier the tokens are for is `proxy.private.url`; there is no
separate option, so the two cannot disagree.

Behavior:

- The module generates the [sign-in configuration](../../CREDS/design/oauth.md#configuration)
  as a store file (it holds no secret) with `private_tier = proxy.private.url`
  and, in sealed mode, `key_credential = "stockhouse-token-key"`.
- Assertions: `proxy.private` is set and has no `credentialFile`; `server`,
  the endpoints and `proxy.private.url` start with `https://` or are
  `http://` to an IP literal matched by `^http://(127\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}|\[::1\])(:[0-9]+)?/`;
  in sealed mode `store.keyCredentialFile` is absolute and outside
  `/nix/store`; `store.directory` is absolute and outside `/nix/store`. Full validation is repeated by `AuthConfig::parse`
  when a unit starts.
- `unitCredential` is the one value through which any unit gets the token,
  so every consumer is wired the same way:

  ```nix
  unitCredential = {
    toml = { server = configFile; };           # the credential spec to write into the unit's configuration
    serviceConfig = {
      LoadCredentialEncrypted = [ "stockhouse-token-key:<keyCredentialFile>" ];  # sealed mode only
      ReadWritePaths = [ store.directory ];
    };
    unitConfig = { after = [ "stockhouse-token-key.service" ]; requires = [ "stockhouse-token-key.service" ]; };  # with createKey
  };
  ```

  Such a unit must also run as `User = Group = "stockhouse"`. The module
  applies it to `stockhouse-proxy.service` (the private tier's
  `credential = { server = … }`), to `stockhouse-uploader.service` with the
  `server` backend and to `stockhouse-lease.service`, and to any later unit
  that needs the token. A consumer's credential assertion is
  `credentialFile != null || auth.enable`.
- User model: with `auth.enable`, the proxy no longer uses `DynamicUser`. It
  runs as the static `stockhouse` system user and group, the same user as the
  uploader, which the module creates whenever `auth.enable` or
  `uploader.enable` is set. The proxy keeps every hardening setting
  (`ProtectSystem = strict`, `PrivateTmp`, `NoNewPrivileges`,
  `RestrictAddressFamilies`) and sets explicitly what `DynamicUser` used to
  imply: `ProtectHome = true`, `RemoveIPC = true` and
  `RestrictSUIDSGID = true`; it adds `UMask = 0077` and the
  `unitCredential.serviceConfig`. A `tmpfiles` rule creates `store.directory`
  as `0700 stockhouse:stockhouse`.

  Justification: the proxy and the uploader must both read and rotate one
  refresh token under one lock, because a refresh token is single use and a
  second use can revoke the grant family. Files owned by a `DynamicUser` have
  no stable owner across restarts and cannot be shared with another unit, and
  group-readable token files would defeat the owner-only checks. Both
  services hold the full access and refresh tokens either way, so separate
  UIDs would not isolate the token; a shared UID with an owner-only `0700`
  directory keeps it from every other local user. The cost is that a
  compromised proxy could also write the upload queue; the server verifies
  every upload, and this is recorded as a non-claim.
- Sealing key: in sealed mode with `store.createKey`, a root oneshot
  `stockhouse-token-key.service` (ordered before every unit that applies
  `unitCredential`) creates `store.keyCredentialFile` if it is absent:

  ```sh
  set -euo pipefail
  umask 077
  [ -e "$key" ] && exit 0
  rm -f "$key.tmp"
  install -d -m 0700 "$(dirname "$key")"
  head -c 32 /dev/urandom | od -An -v -tx1 | tr -d ' \n' \
    | systemd-creds encrypt --name=stockhouse-token-key - "$key.tmp"
  mv -n "$key.tmp" "$key"
  rm -f "$key.tmp"
  ```

  Any failing step fails the unit (and so the units that require it); a
  leftover temporary file is discarded, and an existing key is never
  replaced.

  `systemd-creds` chooses the TPM2 and/or host key (`--with-key=auto`). An
  existing file is never replaced. With `createKey = false` the operator runs
  the same commands once as root before enabling sign-in (the units fail to
  start without the file); the option description documents both. The
  encrypted file is useless without the machine's TPM2 or host key; its
  plaintext exists only in the services' `$CREDENTIALS_DIRECTORY`. A new key
  makes an existing sealed store unreadable (`StoreCorrupt`), which login
  overwrites. File mode creates and loads no key.
- Sign-in commands need the key and the store owner, so the module installs a
  `stockhouse-auth` wrapper in `environment.systemPackages`. It must be run as
  root, accepts only `login`, `login --device`, `logout`, `refresh` and
  `status` (anything else prints usage and exits 2), and runs
  `systemd-run --quiet --wait --collect --pipe` with `User=stockhouse`,
  `Group=stockhouse`, `UMask=0077`, the proxy's hardening and the
  `unitCredential` service settings, executing
  `stockhouse <command> --config <configFile> [--device]` and exiting with its
  status. The transient unit sets no `PrivateNetwork` and no
  `IPAddressDeny`, since it must reach the server and serve the loopback
  callback. `--pipe` connects it to the caller's terminal, so prompts go to
  the operator and not to the journal. The loopback listener runs in that
  unit on `127.0.0.1`, reachable from a browser on the same machine; remote
  shells use `--device`.

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

The VM tests are nine stories
(`tests/vm-{proxy,substitution,durability,lifecycle,signin,refresh,upload,upload-recovery,lease}.test.nix`),
each run as one NixOS VM test per pinned Nix version on the shared node in
`tests/lib/vm.nix`. Each story traces at most five requirements. A
fixture signing key is written in the VM at runtime from a constant public
seed and set as the daemon's `secret-key-files`, so locally built paths are
signed. It is synthetic and trusted only inside the test VM. The VM has no
network, a `file://` private tier that is also the `nix-copy` destination, and
a loopback HTTP shared-tier fixture. The fixture records request headers,
answers 403 and 500 for two chosen hashes and 404 otherwise, so denied and
unavailable tiers are exercised through real Nix and the real proxy. The node
sets `narinfo-cache-negative-ttl = 0`, because every build first queries the
proxy and a cached miss would hide the later upload, and it keeps the
writable store on disk so built paths survive the reboot story.

`tests/module-{proxy,ingest,auth,upload,lease}.test.nix` evaluate the module without a VM
to check generated units, configuration and assertions.

### Sign-in stories

`tests/vm-{signin,refresh}.test.nix` use the shared node with an extra module
(`tests/lib/auth.nix`) that enables `services.stockhouse.auth` in sealed mode
(with the module's key oneshot, which uses the host key since the VM has no
TPM2) against a fake server, `tests/lib/fake_server.py`, on
`http://127.0.0.1:8486/`, with the proxy's private tier at
`http://127.0.0.1:8486/t/1/`. The extra module restores Nix's default
`narinfo-cache-negative-ttl` (3600 s) so that the no-negative-cache check
below is meaningful; its fixture builds use `--option substituters ''`. The
fake server is test code in this repository, implements only the public RFCs and the Nix cache protocol, and
is not a model of any real server:

- `/oauth/authorize` answers a redirect to the given `redirect_uri` with a
  code and the given `state` (the test drives it with `curl -L` as the
  browser) after checking `code_challenge_method=S256`; `/oauth/token`
  checks the PKCE verifier, device codes, `slow_down` and refresh rotation;
  `/oauth/device` issues device and user codes; `/oauth/revoke` follows
  RFC 7009 and revoking a refresh token ends its grant family.
- Refresh tokens rotate on every use. Reusing a rotated refresh token
  revokes the whole family and is recorded as a reuse event, which every
  story asserts never happens.
- Access token lifetime is set through a control route (`/__fake/*`, which
  also approves device codes, revokes a family and reports counters); the
  refresh story uses a lifetime that expires during the test.
- `/t/1/` is a private cache requiring a current access token
  (`Authorization: Bearer`) and answering 401 otherwise, serving narinfo and
  NARs copied in by the test from a daemon-signed local build.

The stories check, besides the sign-in flows, that the token store is
`0700`/`0600` and owned by `stockhouse`, that `tokens.sealed` does not contain
the access or refresh token, and that neither token appears in the journal,
in the arguments of any process, in the units or in the generated
configuration. The revocation case shows that a signed-out machine answers
503 (seen by `curl` and in Nix's own error output), and that after signing in
again the same path substitutes at once, so no negative cache entry was
left behind.

### Server upload stories

The `upload`, `upload-recovery` and `lease` stories use the same node with
`uploader.backend = "server"` and lease renewal enabled
(`randomizedDelaySec = 0`, `backoffMax = 10`). The private tier is a local
fake server (`tests/lib/upload_server.py`, loopback `http://127.0.0.1:8486/t/1/`)
that implements protocol v1 from this repository's spec: it requires the
bearer from a runtime credential file (401 otherwise), keeps uploads,
published paths and leases on disk under `/var/lib/stockhouse-fake` so they
survive a reboot, refuses a declaration whose references (other than itself)
it does not hold with `missing_references`, verifies each NAR's size and
SHA-256 against its declaration, and logs one line per request with the
method, path, upload identity and outcome. Control files make it answer
`present` for chosen paths, `quota_exceeded` to every declaration,
`verification_failed` for a chosen path, stall a chosen NAR body until
released, or close the connection without replying after publishing a
chosen NAR. It serves `nix-cache-info` and answers 404 to narinfo, so the
proxy's private tier stays a valid upstream. It is a test fixture, not a
server implementation, and it claims nothing about a real server.
