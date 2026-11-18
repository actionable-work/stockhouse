# P2.09 W1 `auth`: detailed plan

Workstream W1 of the P2.09 client-server slice (root `PLAN.md`): server
sign-in, the sealed token store, refreshing credentials, the proxy's 401
handling, the CLI commands, the module options and the VM stories. Commit
subject: `feat: add server sign-in, sealed token storage and refreshing credentials`.

Specs (design first, already written):

| Spec | Files |
| --- | --- |
| CREDS | `design/credentials.md` (updated), `design/oauth.md`, `design/store.md` (new); `requirements/CONFIG.toml`, `STORE.toml`, `GRANT.toml`, `REFRESH.toml`, `SESSION.toml` (new); `SOURCE.toml`, `SECRET.toml` (extended; `SOURCE-004` retired) |
| PROXY | `design/proxy.md` (updated); `requirements/CREDENTIAL.toml` (005–011), `CONFIG.toml` (011–012) |
| INTEGRATION | `design/integration.md` (commands, "Server sign-in", sign-in VM stories); `requirements/CLI.toml` (007–012; `CLI-004` retired), `MODULE.toml` (002 amended, 010–015), `VM.toml` (013–018) |
| CLIENT | `README.md` deferral table |

## Crates

Production SLOC estimates keep every crate well under the 1,500 budget.

| Crate (`packages/rust/libs/…`) | New? | Owns | Est. SLOC |
| --- | --- | --- | --- |
| `credentials` (`stockhouse-credentials`) | changed | `CredentialSource::invalidate` (provided, `false`), `CredentialSpec::Server`, new `CredentialError` variants; `grant.rs` (`GrantFlow`, `DeferredGrantFlow`, `GrantError`) deleted | ~400 |
| `token_store` (`stockhouse-token-store`) | new | `TokenSet`, `Binding`, `StoreMode`, `Key`, `Store` (sealed/file read, atomic write, delete), filesystem checks, `Lock`, `StoreError` | ~650 |
| `auth` (`stockhouse-auth`) | new | `AuthConfig`, `Endpoints` (reqwest client, form bodies, response classification), `ServerCredential` (refresh, invalidate), `logout`, `status`, `source` factory, `AuthConfigError`, `Clock` | ~1,000 |
| `signin` (`stockhouse-signin`) | new | PKCE and `state`, the loopback listener (hyper `http1` server), the device flow, `login` (store under lock, best-effort revoke of the replaced grant), `SigninError` | ~650 |
| `proxy` | changed | `RawCredential::Server`, `ConfigError::ServerCredential`, sources from `stockhouse_auth::source` with the audience check, `spawn_blocking` resolution, `UpstreamError::Unauthorized` and the one-retry rule, credential kind in the log line | +~150 |
| `cli` | changed | `login`/`logout`/`refresh`/`status --config` parsing and dispatch | +~150 |

Dependency direction: `credentials` ← `token_store` ← `auth` ← `signin`;
`proxy` → `auth`; `cli` → `auth`, `signin`, `proxy`. W2's ingest backend
depends on `auth` for `source`. The login flow (hyper server) stays out of the
proxy and uploader dependency graph.

## New dependencies

All pinned exactly with `default-features = false`, added to
`[workspace.dependencies]`; run `bloomery sync` afterwards.

| Crate | Pin and features | Why |
| --- | --- | --- |
| `chacha20poly1305` | `=0.11.0`, `["alloc", "zeroize"]` | XChaCha20-Poly1305 (RustCrypto, released 2026-06-28). Reuses the locked `chacha20 0.10.2` and `crypto-common 0.2.2`; new transitive crates are `aead 0.6`, `cipher 0.5`, `inout 0.2`, `poly1305 0.9`, `universal-hash 0.6` (all RustCrypto, `MIT OR Apache-2.0`). No `getrandom` feature: nonces come from `getrandom` directly. At implementation, verify with `bloomery sync` that `=0.11.0` resolves against the workspace's RustCrypto pins; if it does not, fall back to `=0.10.1` (the previous stable line, same `XChaCha20Poly1305` API apart from trait paths) and record the choice in the implementation notes |
| `getrandom` | `=0.4.3`, no features | OS randomness for nonces, PKCE verifier, `state` and temp names. Already locked (via `uuid`) |
| `form_urlencoded` | `=1.2.2`, `["alloc"]` | `application/x-www-form-urlencoded` bodies and the authorization query. Already locked (via `url`) |

Reused workspace pins: `reqwest` (HTTP client; no `json`/`form` features
needed, bodies are built and parsed by hand with `form_urlencoded` and
`serde_json`), `tokio`, `hyper`/`hyper-util`/`http`/`http-body-util`/`bytes`
(loopback listener), `serde`, `serde_json`, `toml`, `sha2` (PKCE, binding,
rejected-token digest), `base64` (base64url), `libc` (`geteuid`, open flags),
`zeroize`. File locking uses `std::fs::File::{lock, try_lock}` (stable since
Rust 1.89; the toolchain is 1.98), so no locking crate. `state` is compared
with plain equality: the listener is one shot, so there is no timing oracle,
and no `subtle` pin is needed.

`reqwest` stays async-only in W1's crates. W2 enables `reqwest/blocking` in
its own crate; feature unification is harmless, but W1 code never calls a
blocking `reqwest` API, and nothing calls one from the proxy's runtime or its
blocking pool.

Rejected alternatives: `ureq` or `reqwest/blocking` for a synchronous refresh
(adds a second TLS stack or panics inside a runtime; a scoped thread with its
own current-thread runtime over the existing `reqwest` is enough);
`oauth2`/`openidconnect` (large, pull in default features and their own HTTP
abstractions, while we need four small requests); `fs2`/`fs4` (std has
`flock`).

## Ordered outcomes and tests

Every Rust test traces its ID with `#[cfg_attr(any(), bloomery("<ID>"))]` and
asserts exact error variants. Test servers are in-crate test code (hyper on
`127.0.0.1:0`) that records requests; no test contacts the network.

### 1. Credentials interface (`credentials`)

- Add `fn invalidate(&self, _rejected: &Secret) -> bool { false }` to
  `CredentialSource`; add `CredentialSpec::Server(PathBuf)` with the one
  shared serde form (`file | systemd | server`, `deny_unknown_fields`) used by
  proxy tiers, the uploader backend and lease configuration; `source(Server)`
  returns a source failing with `CredentialError::Unsupported`; add the
  `CredentialError` variants of the credential design with `Display` text
  naming the check only; delete `grant.rs`, its `GrantFlow`/`GrantError`/
  `DeferredGrantFlow` re-exports from `lib.rs`, the `SOURCE-004` test, and the
  CLI's `CLI-004` test and `DeferredGrantFlow` use; update the AGENTS.md
  layout row for `libs/credentials` (no deferred grant flow) and add rows for
  the three new crates.
- Tests (`tests/credentials.rs`): `SOURCE-006` (each static source returns
  `false` and yields the same value afterwards), `SOURCE-007`.

### 2. Token store (`token_store`)

- `TokenSet` (serde with `deny_unknown_fields`, tokens as `Secret`, manual
  `Debug`), `Binding::new(token_endpoint, client_id, private_tier)`,
  `Key::load(&SystemdCredential)`, `Store::{open(dir, mode, binding), read,
  write, delete, lock(wait)}`, `Lock` guard.
- Tests (`tests/store.rs`, temp dirs `0700`; key via
  `SystemdCredential::with_directory`):

| ID | Test |
| --- | --- |
| STORE-001 | write/read round trip; grep file bytes for both tokens |
| STORE-002 | header layout; two writes of one set differ in nonce and ciphertext; decrypt with AAD manually in the test using the crate's public format constants (not the production reader) |
| STORE-003 | truncate, flip a ciphertext byte, flip the magic, other key → `Corrupt`; file unchanged |
| STORE-004 | other binding → `Mismatch` |
| STORE-005 | no `CREDENTIALS_DIRECTORY`/missing → `KeyUnavailable`; 63/65 chars, uppercase, non-hex → `KeyMalformed` |
| STORE-006 | file mode writes `tokens.json` `0600`; sealed mode never creates it |
| STORE-007 | failure injection: a read-only directory at rename time and a pre-existing temp name collision → previous store intact, no temp left; success path: store file inode changes, other mode's file gone, and the directory `fsync` happens after the removal (checked with a test hook recording the step order) |
| STORE-008 | write sealed with stale `tokens.json` present → removed, and vice versa |
| STORE-009 | symlink, FIFO, chmod `0640`, oversized → exact variants |
| STORE-010 | missing dir, symlinked dir, `0750` dir → exact variants, nothing created |
| STORE-011 | empty dir → `Ok(None)` |
| STORE-012 | hand-built plaintexts: bad JSON, unknown field, `version: 2`, header/JSON binding differ, empty or 4097-byte or non-ASCII token → `Corrupt` |
| STORE-013 | delete with both, one or no files |
| STORE-014 | two independent `Store` handles: second `lock` waits until the first guard drops; with a short wait → `LockTimeout` |
| STORE-015 | a child process (the test binary re-executed with an env var selecting an `#[ignore]` helper) takes the lock and calls `std::process::abort`; the parent then locks immediately |
| STORE-016 | stale `.tokens.1.x.tmp` removed by the next write |
| STORE-017 | lock file pre-created as a symlink, a FIFO, owned by another UID (root-run skip otherwise; owner check unit-tested with an injected euid as for `OwnerOnlyFile`), and `0640` → exact variants; store untouched |
| SECRET-003 | `format!("{:?}", set)` contains neither token |

### 3. Auth configuration, endpoint client, refresh, logout, status (`auth`)

- `AuthConfig::parse` and `load(path)`; `Endpoints` client built like the
  proxy's (redirects off, platform verifier, timeouts, 64 KiB body cap);
  `classify(status, body) -> TokenOutcome`; `ServerCredential` with an
  injectable `Clock` (`fn now(&self) -> u64`) and the in-process mutex;
  `logout(&AuthConfig) -> LogoutReport`; `status(&AuthConfig) -> Status`
  (serde `Serialize`); `source(spec)`.
- A test fake (`tests/common/fake.rs`): token endpoint with scripted
  responses and per-refresh-token use counts; revocation endpoint recording
  `token_type_hint`s; records whether any request had `Authorization`.

| ID | Test |
| --- | --- |
| CONFIG-001…010 | one parse test per row of the configuration table, each asserting the exact `AuthConfigError` and that the error text excludes the offending value |
| GRANT-015 | 200 variants: lowercase `bearer` accepted; missing/zero/86 401 `expires_in`, `token_type: mac`, 4097-byte token → `InvalidResponse`; `expires_at` = clock at send + `expires_in` |
| GRANT-016 | 400 `invalid_grant`, 400 unknown code → `Other`, 429, 503, 302, 418, 70 KiB body, closed connection, timeout → exact classifications; the fake asserts the redirect target was never requested |
| GRANT-017 | fake asserts `client_id` in every form body and no `Authorization` header |
| REFRESH-001 | fresh token: zero fake requests, lock file not even created |
| REFRESH-002 | rewrite and delete the store between calls |
| REFRESH-003 | clock at `expires_at - 60` and after expiry → one refresh request with the stored refresh token |
| REFRESH-004 | store rewritten (as by another process) while the lock is held by the test; the credential then returns the new token with zero requests |
| REFRESH-005 | test holds the lock; short wait → `RefreshUnavailable`, zero requests |
| REFRESH-006 | rotated and non-rotated responses; the store holds the new set before `credential()` returns (checked from the fake's handler on the next request) |
| REFRESH-007 | `invalid_grant` → `SignedOut`, store gone, next call zero requests |
| REFRESH-008 | 503 before expiry → old token, store unchanged; 503 after expiry → `RefreshUnavailable`; `temporarily_unavailable`, timeout, 302 likewise |
| REFRESH-009 | `invalid_client`, malformed 200 → `RefreshRejected`, store unchanged |
| REFRESH-010 | expired, no refresh token → `SignedOut`; fresh but rejected (invalidated by the test bypassing the 60 s guard) with no refresh token → `SignedOut`; zero requests |
| REFRESH-011 | set obtained 61 s ago: `invalidate(current)` → `true`; next call refreshes; with the store replaced by a fresh token meanwhile → no request |
| REFRESH-012 | 16 threads, each with its own `ServerCredential` over one store (separate lock descriptions, as separate processes have), plus 2 child processes; the fake records overlapping requests per refresh token and every token answered 200: none overlaps, none is resent after its 200 |
| REFRESH-013 | call `credential()` needing refresh from a plain thread and from `spawn_blocking` inside a `current_thread` and a `multi_thread` runtime; no panic, correct token |
| REFRESH-014 | `source` for each spec; unreadable or invalid auth file → `AuthConfigError` |
| REFRESH-015 | `invalidate` → `false` with no store, with no refresh token, with a set obtained 59 s ago, and right after an invalidation-forced refresh (clock +30 s); nothing recorded, zero requests |
| REFRESH-016 | a 503 refresh then 8 queued threads within 10 s: one request in total; before expiry they get the old token, after expiry `RefreshUnavailable`; at +11 s one new request |
| SESSION-001 | revocation fake receives exactly one request: the refresh token with `token_type_hint=refresh_token` and `client_id`; store deleted; a set without refresh token → no request, report says nothing revoked |
| SESSION-002 | revocation 503, 302 and connection reset → `RevocationFailed(_)`, store deleted, exactly one request |
| SESSION-003 | no store → `NotSignedIn`, zero requests |
| SESSION-004 | logout holds the lock while a second thread's `credential()` needs refresh: it returns `SignedOut` and the fake sees no refresh |
| SESSION-005 | status JSON for signed in, expired-but-refreshable, expired-not-refreshable, absent, corrupt; JSON excludes both tokens and the binding |
| SESSION-006 | logout over a `Corrupt` and a `Mismatch` store → deleted, zero requests, report says nothing revoked |
| SECRET-004 | every error path above formatted with `{}` and `{:?}` and checked against all secret strings used by the fake, including an `error_description` carrying a canary |

### 4. Sign-in flows (`signin`)

- `Pkce::generate`, `LoopbackLogin` (bind, `authorization_url`, `wait` with
  an injectable deadline, default 300 s), `DeviceLogin` with an injectable
  `Pacer` (`async fn wait(&self, Duration)`) so polling tests do not sleep,
  `login(config, flow, out: &mut dyn Write)`.

| ID | Test |
| --- | --- |
| GRANT-001 | verifier and state are 43 base64url chars from fresh randomness (two logins differ); the challenge the fake receives equals base64url(SHA-256(verifier the token request carries)) |
| GRANT-002 | listener address is `127.0.0.1` and the port is the kernel's; the redirect URI matches |
| GRANT-003 | parse the printed URL: every parameter present once; an endpoint with `?tenant=x` keeps it |
| GRANT-004 | a client `GET /callback?code=…&state=…` → 200; the fake token endpoint receives all five fields; store holds the set |
| GRANT-005 | missing and wrong `state` → 400 and `StateMismatch`; fake token endpoint saw nothing |
| GRANT-006 | `error=access_denied&error_description=<canary>` → `AuthorizationDenied(AccessDenied)`; canary absent from output and error |
| GRANT-007 | after the callback, a new connection to the port is refused |
| GRANT-008 | `/favicon.ico` → 404, `POST /callback` → 405, then a valid callback still succeeds; bodies do not echo the request |
| GRANT-009 | deadline of 200 ms → `LoginTimeout`; port closed |
| GRANT-010 | stdout contains URI, user code and complete URI, never the device code |
| GRANT-011 | missing `device_code`, `interval: 0`, `expires_in: 3601`, `verification_uri: "javascript:…"` → `DeviceResponse`, zero polls |
| GRANT-012 | `authorization_pending` ×2 then tokens: the pacer saw 5, 5, 5 s |
| GRANT-013 | `slow_down` twice then tokens: pacer saw 5, 10, 15 s |
| GRANT-014 | `access_denied` → `DeviceDenied`; `expired_token` → `DeviceExpired`; pending forever with `expires_in: 12` and a fake clock → `DeviceExpired` with no poll after the deadline |
| GRANT-018 | login over an existing set: new set stored, the fake revocation endpoint receives the old refresh token once; a 503 there still returns success with a warning |
| GRANT-019 | 503, 429 and a reset while polling with interval 5 → pacer saw 5, 10, 15, 20 s; starting at 58 s → 58, 60, 60; starting at 65 s after `slow_down` → 65, 65 |
| GRANT-020 | every listener response (200, 400, 404, 405) carries `Cache-Control: no-store` and `Referrer-Policy: no-referrer` |
| GRANT-021 | missing directory, missing key, and lock held past the wait: login fails with the exact `StoreError`; the fake saw no request and no listener was bound |
| GRANT-022 | login over a `Corrupt` and a `Mismatch` store: overwritten, zero revocation requests |

### 5. Proxy (`proxy`)

- `RawCredential::Server(PathBuf)`; `ConfigError::ServerCredential(tier)`
  for non-private tiers and relative paths; `Tier` holds
  `Arc<dyn CredentialSource>` built by `stockhouse_auth::source`; at bind,
  `ServeError::Credential { tier }` and `ServeError::CredentialAudience { tier }`
  (the CLI maps both to exit 2); resolution through `spawn_blocking`;
  `UpstreamError::Unauthorized` for 401; a per-request `Attempt` that allows
  one `invalidate`+re-resolve+retry and carries the current secret through
  cache-info, narinfo and NAR open; `credential` log lines append the
  credential error kind (`CredentialError::kind()`).
- Tests (`tests/credential.rs`, `tests/config.rs`, using a test
  `CredentialSource` that counts calls, can block, and returns scripted
  tokens and `invalidate` results):

| ID | Test |
| --- | --- |
| PROXY-CONFIG-011 | `server` on shared and public tiers, relative path → exact errors |
| PROXY-CONFIG-012 | bind with an unreadable auth file and with a mismatched `private_tier` → exact `ServeError`s; port not bound |
| PROXY-CREDENTIAL-005 | a source blocking 500 ms: `/nix-cache-info` answers in under 100 ms meanwhile |
| PROXY-CREDENTIAL-006 | fixture answers 401 to token A, 200 to token B; source rotates on `invalidate` → 200 served, fixture saw A then B |
| PROXY-CREDENTIAL-007 | fixture 401s every token → 503 `denied`, exactly one `invalidate`, two requests, later tier untouched |
| PROXY-CREDENTIAL-008 | source returns `SignedOut` after `invalidate` → 503 `credential`, no third request |
| PROXY-CREDENTIAL-009 | 403 with a refreshable source and 401 with a static source → one request, zero `invalidate` calls |
| PROXY-CREDENTIAL-010 | 401 on `nix-cache-info`, on narinfo, on NAR open: each retried once; the remaining requests of that proxy request carry token B |
| PROXY-CREDENTIAL-011 | the log line for a signed-out source is `stockhouse-proxy: narinfo private credential signed-out` |

Existing `LOOKUP-005` (401/403 → 503 `Denied`) keeps passing: a 401 with a
static credential is still `Denied` after one request.

### 6. CLI (`cli`)

- `Parsed::{Login { config, device }, Logout { config }, Refresh { config },
  Status { target: Queue(dir) | Session(config) }}`; dispatch builds a
  runtime only for `login`; outputs as in the integration design.
- Tests (`tests/cli.rs` with an in-test fake server and a file-mode store):

| ID | Test |
| --- | --- |
| CLI-001 | updated dispatch test for the new commands |
| CLI-007 | device login against the fake, which answers `interval: 1` and tokens on the first poll (no test-only configuration knob) → 0; `access_denied` → 1; bad config → 2; loopback login driven by a test client reading the printed URL → 0 |
| CLI-008 | revoke OK → 0; revoke 503 → 1 with "signed out locally"; store gone in both |
| CLI-009 | refresh OK → 0; `invalid_grant` → 1 with "sign in again" |
| CLI-010 | status signed in → 0 and JSON; absent → 1 and JSON |
| CLI-011 | `status --queue x --config y` and bare `status` → 2 |
| CLI-012 | all outputs above checked against every token, code, verifier and device code the fake issued; `state` and `code_challenge` occur only inside the printed authorization URL |
| CLI-002/003 | unchanged; `--token` still rejected for the new commands |

### 7. Module (`nix/module.nix`, `tests/module-auth.test.nix`)

- Options and behavior of the integration design's "Server sign-in" section;
  `auth.configFile` read-only; the `stockhouse-auth` wrapper
  (`pkgs.writeShellApplication`) with a `case` over the allowed argument
  lists and no `PrivateNetwork`/`IPAddressDeny`; tmpfiles rule; the
  `stockhouse-token-key.service` oneshot; the internal read-only
  `auth.unitCredential` value; proxy unit switches to the static user only
  with `auth.enable` and applies `unitCredential`. No `auth.privateTier`
  option: `private_tier` is `proxy.private.url`. W1 does not touch the
  uploader unit; W2 applies `unitCredential` to the uploader and lease units.
  Edits stay in a separate `auth` section of `module.nix`.
- `tests/module-auth.test.nix` (evaluation only):

| ID | Assertion |
| --- | --- |
| MODULE-002 | (in `module-proxy`) unchanged unit without auth |
| MODULE-010 | the generated TOML parses (`builtins.fromTOML`) with the expected fields, `private_tier == proxy.private.url` and no secret-looking field; private tier has `credential.server` = `configFile` |
| MODULE-011 | sealed: `LoadCredentialEncrypted` on the proxy (from `unitCredential`); `key_credential` set |
| MODULE-012 | file mode: no `LoadCredentialEncrypted` in `unitCredential` or the proxy, no key oneshot; `mode = "file"` |
| MODULE-013 | no `DynamicUser`, `User = Group = "stockhouse"`, MODULE-009 hardening, `ProtectHome`, `RemoveIPC`, `RestrictSUIDSGID`, `UMask`, `ReadWritePaths`, tmpfiles rule |
| MODULE-014 | each inconsistency produces its assertion message, including `http://localhost/` and `http://127.0.0.1.example.invalid/` refused and `http://[::1]:8486/` accepted by the IP-literal rule |
| MODULE-015 | wrapper script text contains `systemd-run`, `--pipe`, `User=stockhouse`, the credential and the `case` arms and no `PrivateNetwork`/`IPAddressDeny`; a `runCommand` executes it with a stubbed `systemd-run` on `PATH` to check that `login --token x`, `foo` and no arguments exit 2 and that allowed commands pass exactly the expected argv |
| MODULE-016 | sealed with `createKey`: oneshot defined, ordered before the proxy; its script runs `systemd-creds encrypt --name=stockhouse-token-key` only under a `test ! -e` guard; `createKey = false` → no oneshot |
| MODULE-017 | `unitCredential` has `toml.server = configFile`, the sealed `serviceConfig` and the oneshot ordering, and the proxy unit contains those settings |

### 8. VM stories (`tests/`)

- `tests/lib/fake_server.py`: stdlib `http.server` fake as described in the
  integration design ("Sign-in stories"); every token random per run; JSON
  state at `/var/lib/stockhouse-vm/fake-state.json` for assertions (counters,
  reuse events, revocations) without ever writing tokens to logs.
- `tests/lib/auth.nix`: extra node module: fake server unit on
  `127.0.0.1:8486`, `services.stockhouse.auth` sealed with the module's key
  oneshot (host key; no TPM2 in the VM), `proxy.private` overridden with
  `lib.mkForce` to `http://127.0.0.1:8486/t/1/`, and
  `narinfo-cache-negative-ttl` restored to 3600 for the no-negative-cache
  check.
- `tests/lib/vm.nix` (shared file, distinct hunk): `story` takes an optional
  `extraModule ? {}` imported by the node.
- Fixture paths are built with `--option substituters ''` so a signed-out
  proxy cannot affect fixture builds, then `nix copy --to file://` into the
  fake's cache directory and deleted from the store before substitution.

| Story | IDs | Outline |
| --- | --- | --- |
| `vm-signin` | VM-013, VM-014, VM-017, VM-018, VM-010 | loopback login (`stockhouse-auth login &`, read the URL from its output, `curl -sL`), status; logout; device login (read user code, approve through `/__fake/approve`), status, substitute a private path; permission, sealed-content, journal, `/proc/*/cmdline`, unit and config checks for both tokens (read from the fake's state); logout: one refresh-token revocation recorded, family ended, store gone, narinfo 503 |
| `vm-refresh` | VM-015, VM-016, VM-010 | device login with a 70 s access lifetime; for 90 s run concurrent proxy narinfo lookups and four parallel `stockhouse-auth refresh` loops; every lookup 200, at least one rotation, zero reuse events; revoke the family at the fake; `curl` through the proxy → 503, `nix-store --realise` of a deleted private path fails with Nix's output showing 503, status signed out, journal has `private credential signed-out`; device login again and realise the same path immediately → substituted (no negative cache entry) |

Each story runs with Nix 2.34.8 and 2.31.5 (`VM-010`).

### 9. Validation

Per commit: stage new files, `nix fmt`, `nix develop . --command bloomery sync`
(new crates and dependencies), `nix develop . --command bloomery check`
(commit the rewritten `.bloomery/code-health-summary.json`), and
`nix flake check` (module evaluation and all VM stories on both Nix pins).
Independent implementation review before the workstream closes.

## Decisions for review

1. **Three new crates** (`token_store`, `auth`, `signin`) instead of growing
   `credentials`, so the proxy and uploader never link the login listener and
   each crate stays far below 1,500 SLOC.
2. **`CredentialSource::invalidate(&self, rejected: &Secret) -> bool`**, a
   provided method defaulting to `false`, is the only trait change (frozen by
   the orchestrator). Consumers retry once only when it returns `true`.
   `ServerCredential` returns `false` when no refresh token is stored, when the
   set is less than 60 s old, or after an invalidation-forced refresh in the
   last 60 s, so a server-side denial does not churn the grant.
3. **`Unsupported` vs "not signed in"**: `stockhouse_credentials::source`
   given a `Server` spec fails with `CredentialError::Unsupported` so
   miswiring is visible; `NotConfigured` and `SignedOut` both mean "not signed
   in" to consumers. One serde `CredentialSpec` (file | systemd | server) is
   used everywhere and always built by `stockhouse_auth::source`.
4. **401 retry budget is one per proxy request**, shared across
   `nix-cache-info`, narinfo and NAR open; 403 is never retried; the final 401
   stays `ProxyError::Denied` (503) so existing `LOOKUP-005` semantics hold.
5. **`credential()` may block**; the proxy resolves it with `spawn_blocking`.
   The refresh runs on a scoped OS thread with its own current-thread runtime
   and a fresh async `reqwest` client, which works from sync (uploader) and
   async (proxy) callers without nesting runtimes; no blocking `reqwest` API
   is used by W1.
6. **Re-read the store on every `credential()` call** (no in-memory token
   cache), so a logout or re-login by another process applies immediately.
7. **Fixed 60 s refresh skew**; lock wait = 2 × request timeout (60 s
   default), timeout is `RefreshUnavailable` with no request. Reviewed
   alternative `min(60 s, lifetime / 2)`, kept fixed after the W1
   implementation review: with the protocol's 10-minute access tokens both
   give 60 s; for a lifetime of 60 s or less a fixed skew refreshes on every
   use, which is chatty but always safe (serialized by the lock), while a
   halved skew would add a second source of truth (`obtained_at`) to the
   freshness rule. Revisit only if a server issues very short tokens.
8. **Transient refresh failures** keep serving the current token before
   expiry (503 after), and start an in-memory 10 s backoff so queued threads
   and later calls send nothing meanwhile.
9. **Deferred, not designed around**: a failed store write after a
   successful refresh may lose the rotated refresh token; resending a refresh
   token whose request got no reply may trip server-side reuse detection (the
   protocol requires no grace window). Either can sign the machine out; both
   are in the CLIENT deferral table.
10. **Missing `expires_in` is refused** (`InvalidResponse`), since lifetimes
    come only from `expires_in`; values are bounded to 1–86 400.
11. **All endpoints must share the `server` origin**; the private tier may be
    a different origin. HTTP is accepted only to loopback IP literals.
12. **The proxy checks token audience**: a `server` credential is allowed only
    on the private tier, and its `private_tier` must equal the tier URL,
    compared as parsed normalized URLs.
13. **Store binding** (SHA-256 of token endpoint, client ID and private tier)
    is in the sealed header and the JSON; a store for another configuration is
    `Mismatch`.
14. **Untrusted stores**: `Corrupt`/`Mismatch` stores are never deleted by
    refresh; login overwrites them and logout deletes them, both without any
    revocation request.
15. **Login preflights** the directory, key and lock before binding a
    listener or contacting the server.
16. **Key**: 64 lowercase hex characters (`SystemdCredential` accepts only
    visible ASCII). The module creates it with a root oneshot when absent
    (`store.createKey`, default true, never replacing an existing file);
    manual creation with the same `systemd-creds encrypt` command is the
    documented alternative.
17. **User model**: with sign-in enabled, the proxy runs as the static
    `stockhouse` user (shared with the uploader) instead of `DynamicUser`,
    keeping its hardening and setting `ProtectHome`, `RemoveIPC`,
    `RestrictSUIDSGID` and `UMask = 0077` explicitly; the store directory is
    `0700` via tmpfiles. Without sign-in the proxy unit is unchanged.
18. **`auth.unitCredential`** is the single internal value that wires a unit
    to the token (credential spec, `serviceConfig`, key oneshot ordering). W1
    applies it to the proxy; W2 applies it to the uploader and lease units.
    `auth.privateTier` is dropped; the private tier is `proxy.private.url`.
19. **Sign-in commands run through a root-only `stockhouse-auth` wrapper**
    (`systemd-run --wait --pipe` as `stockhouse` with the same credential and
    hardening, no `PrivateNetwork`/`IPAddressDeny`), because the key exists
    only inside units. The loopback flow therefore needs a browser on the
    same machine; remote shells use `--device`. Both flows are kept.
20. **`status` gains `--config`** and requires exactly one of `--queue` or
    `--config`; exit 0 when signed in, 1 otherwise.
21. **Re-login revokes the replaced refresh token best-effort** after the new
    set is stored.
22. **Logout revokes only the refresh token** (ending the grant and its
    access tokens server-side), with no retry; it deletes the store regardless
    and exits 1 only if that revocation failed.
23. **Login wait is 300 s.** Device polling adds 5 s on `slow_down` (RFC 8628)
    and on transient failures or 429 (capped at 60 s), until the device
    code's `expires_in`.
24. **Callback hygiene**: listener responses are static with
    `Cache-Control: no-store` and `Referrer-Policy: no-referrer`; stray
    requests before the callback get 404/405, connections after it are
    refused; `state` uses plain comparison (one shot, no oracle) and appears
    only in the printed authorization URL.
25. **Error texts carry only classifications** from a fixed set of OAuth
    codes; `error_description`, `error_uri` and bodies are never shown.
26. **Retired IDs**: `CLIENT-CREDS-SOURCE-004` and
    `CLIENT-INTEGRATION-CLI-004` (deferred stubs) are removed with the stubs,
    their tests and the `DeferredGrantFlow` exports.
    `CLIENT-INTEGRATION-MODULE-002`'s trigger now excludes `auth.enable`.

## Implementation review fixes

- Device verification URIs must be visible ASCII, so no control character
  or terminal escape is printed (GRANT-011).
- Endpoint requests have a total timeout as well as connect and per-read
  timeouts (oauth.md "HTTP client").
- The key oneshot runs `set -euo pipefail` with `umask 077`, discards a
  leftover `$key.tmp` and never replaces an existing key (`mv -n`,
  MODULE-016).
- `invalidate` behavior is specified step by step (oauth.md, REFRESH-015):
  no store returns `false` at once; the no-refresh-token, young and
  recently-forced cases wait for the lock and re-read.
- The loopback listener serves connections concurrently and fails with
  `Listener` on an `accept` error (GRANT-008).
- The VM fake rejects expired access tokens, and VM-015 has a proxy-only
  80 s phase past the 70 s lifetime before the concurrent phase.
- `CredentialError::kind()` names every variant; the proxy `server`
  credential path rejects `.` and `..` components (PROXY-CONFIG-011); the
  proxy keeps the sent secret when `invalidate` returns `false`.

## Notes for the orchestrator and W2

- W2 builds every credential from the shared `CredentialSpec` with
  `stockhouse_auth::source` (`stockhouse_credentials::source` fails with
  `Unsupported` for `Server`), calls `credential()` from its synchronous
  backend, and on a 401 calls `invalidate(&sent)`: if it returns `true`, it
  retries once with a fresh `credential()`; otherwise, or on a second 401,
  `Retry(Transient)`. `SignedOut` and `NotConfigured` mean "not signed in".
- The module exposes `services.stockhouse.auth.configFile` and
  `auth.unitCredential`; W2 applies `unitCredential` to
  `stockhouse-uploader.service` and `stockhouse-lease.service` (TOML
  credential, `serviceConfig`, ordering, `User = stockhouse`), and its
  credential assertion is `credentialFile != null || auth.enable`. W1 does not
  edit the uploader unit.
- Shared-file hunks: `nix/module.nix` (auth section, key oneshot,
  `unitCredential`, proxy unit user switch), `libs/cli/src/{args,commands,lib}.rs` (sign-in
  commands, `status --config`), workspace `Cargo.toml` (three members, three
  dependencies), `tests/lib/vm.nix` (`extraModule`), `tests/flake.nix` (three
  new test files), `AGENTS.md` layout table (three new crate rows and the
  updated `libs/credentials` row), README.
- The `vm-refresh` story exercises cross-process refresh with the proxy and
  concurrent `stockhouse-auth refresh` processes. Once W2's server backend is
  in the stack, the uploader can be added as a further concurrent consumer in
  that story at linearization.
- The slice record (`docs/P2_09_CLIENT_SERVER.md`) should carry the non-claims:
  per-machine sign-in only; a compromised proxy shares the uploader's UID; a
  lost refresh reply or a resent refresh token may sign the machine out;
  qualification is against this repository's fake server only.
