# Credential design

Credentials are runtime values. They are never accepted on a command line,
in an environment variable, in a configuration file, or in a Nix option that
would place them in a derivation or the store. Configuration names only
*where* a credential lives.

## Secret values

`Secret` wraps credential bytes. Its `Debug` and `Display` render
`Secret(<redacted>)`; it implements no serialization; it zeroes its own buffer on
drop. Copies made by an HTTP client for a request header are not tracked. The only accessor that exposes the value is an explicit
`expose()` used when constructing an `Authorization` header.

## Sources

`CredentialSource::credential(&self) -> Result<Secret, CredentialError>` is
called on every use, so a rotated file takes effect without a restart. It may
block (file reads, and for the server source a token refresh), so async
callers run it on a blocking thread. A source is selected only by explicit
configuration:

| Configuration | Source |
| --- | --- |
| `{ file = "/abs/path" }` | `OwnerOnlyFile`: the explicitly configured owner-only file fallback |
| `{ systemd = "<name>" }` | `SystemdCredential`: `$CREDENTIALS_DIRECTORY/<name>` from systemd `LoadCredential=`, then the symlink, regular-file, size and content checks (no owner or mode check) |
| `{ server = "/abs/auth.toml" }` | `ServerCredential` (`stockhouse-auth`): the current access token from the signed-in token store, refreshed before expiry ([server sign-in](oauth.md)) |
| absent | no credential; a tier that requires one fails with `NotConfigured` |

`CredentialSpec` has the three variants `File`, `Systemd` and `Server`, and
one serde form (`{ file = … }`, `{ systemd = … }` or `{ server = … }`, with
`deny_unknown_fields`) shared by every configuration that names a credential:
proxy tiers, the uploader's server backend and the lease configuration.
`stockhouse_credentials::source` builds `File` and `Systemd` sources; it
cannot build a server source (the credentials crate has no network or store
code), so for `Server` it returns a source that fails with
`CredentialError::Unsupported`, making miswiring visible instead of looking
signed out. Every production caller (the proxy, the uploader, leases) builds
its source with `stockhouse_auth::source`, which delegates `File` and
`Systemd` to `stockhouse_credentials::source` and builds `ServerCredential`
for `Server`.

For consumers, `NotConfigured` and `SignedOut` both mean "not signed in":
the request is not sent, and the consumer reports it as such (the proxy
answers 503; the uploader retries later without consuming an attempt).

### Rejected credentials

`CredentialSource` has a second, provided method:

```rust
fn invalidate(&self, rejected: &Secret) -> bool { false }
```

A caller whose request was answered 401 passes the credential it sent. The
return value says whether a different credential may now be available, that
is, whether one retry is worthwhile. The default (used by `OwnerOnlyFile`,
`SystemdCredential` and `StaticCredential`) returns `false` and changes
nothing: a static credential cannot be refreshed, so the caller does not
retry. `ServerCredential` overrides it ([refresh](oauth.md#rejected-access-tokens)).
Callers retry at most once per request, whatever it returns.

There is no implicit search of home directories, `/run/secrets`, or any
default location.

`OwnerOnlyFile` checks, in order:

1. The path is absolute (`RelativePath`), has no `.` or `..` component
   (`RelativePath`), and is not under `/nix/store` (`InStore`).
2. It opens with `O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK` (so a FIFO cannot block); a missing file is `Missing`, a
   symbolic link is `Symlink`, other open errors are `Unreadable`.
3. `fstat` on the open descriptor: not a regular file is `NotRegularFile`;
   an owner other than the effective UID is `WrongOwner`; any group or other
   permission bit is `PermissionsTooOpen { mode }`.
4. At most 4097 bytes are read, whatever `st_size` says; more than 4096 is
   `TooLarge`.
5. One trailing `\n` is removed. Empty content is `Empty`; any byte outside
   visible ASCII (`0x21..=0x7e`) is `Malformed`.

`SystemdCredential` returns `NotConfigured` when `CREDENTIALS_DIRECTORY` is
unset and `InvalidName` when the name is empty, is `.` or `..`, or contains
`/`. It then applies checks 2, 3 (regular file only), 4 and 5 to
`$CREDENTIALS_DIRECTORY/<name>`, but not the owner or mode checks. systemd
protects that directory itself, and it commonly grants the service read access
through a root-owned file with an ACL, whose mode shows group bits.

`LoadCredential=` copies its source as root before the service starts, so the
client never sees the source file. Protecting that source (for example
`root:root 0400`) is the operator's job; the module documents it.

`CredentialError` messages name the source and the failed check, never the
content. `StaticCredential` exists only for tests and is not reachable from
configuration.

`CredentialError` stays a `Copy` enum without payload bytes. The server
source adds these variants, none of which carries token material:

| Variant | Meaning |
| --- | --- |
| `SignedOut` | no tokens are stored, the server ended the grant (`invalid_grant`), or the access token expired with no refresh token |
| `RefreshUnavailable` | the access token has expired and a refresh failed transiently (or the refresh lock was not acquired in time); tokens are kept |
| `RefreshRejected` | the token endpoint refused the refresh with an error other than `invalid_grant` (for example `invalid_client`); tokens are kept |
| `StoreUnsafe` | the token store directory or file failed an ownership, mode, type or size check |
| `StoreCorrupt` | the store could not be decrypted or parsed |
| `StoreMismatch` | the store was written for a different server configuration |
| `KeyUnavailable` | the sealing key credential is not loaded |
| `KeyMalformed` | the sealing key credential is not 64 lowercase hex characters |
| `AuthConfig` | the server sign-in configuration named by `{ server = … }` could not be loaded |
| `Unsupported` | `stockhouse_credentials::source` was asked for a `Server` source, which only `stockhouse_auth::source` can build |

## Grant flow

The groundwork's `GrantFlow` trait and `DeferredGrantFlow` stub are removed.
Server sign-in, refresh, logout and status are specified in
[server sign-in](oauth.md) (`stockhouse-auth`, `stockhouse-signin`) and the
token store in [token store](store.md) (`stockhouse-token-store`).
`CLIENT-CREDS-SOURCE-004` (the deferred grant flow) is retired with the stub.
