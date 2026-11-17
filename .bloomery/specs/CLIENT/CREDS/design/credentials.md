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
called on every use, so a rotated file takes effect without a restart. A
source is selected only by explicit configuration:

| Configuration | Source |
| --- | --- |
| `{ file = "/abs/path" }` | `OwnerOnlyFile`: the explicitly configured owner-only file fallback |
| `{ systemd = "<name>" }` | `SystemdCredential`: `$CREDENTIALS_DIRECTORY/<name>` from systemd `LoadCredential=`, then the symlink, regular-file, size and content checks (no owner or mode check) |
| absent | no credential; a tier that requires one fails with `NotConfigured` |

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

## Grant flow

`GrantFlow` declares `login`, `logout` and `refresh`. The only implementation
in this slice, `DeferredGrantFlow`, returns
`GrantError::Unavailable { dependency: "P2.06" }` for each without network
access or local state changes. Real PKCE/device login, refresh-family rotation
and revocation arrive with P2.06 grants.
