# Token store design

`stockhouse-token-store` keeps one signed-in session (a `TokenSet`) in a
directory shared by the processes that use it: the proxy, the uploader and
the `stockhouse` CLI run through the module's `stockhouse-auth` wrapper. It
has no network code. All of them run as the same user (see
[the module's user model](../../INTEGRATION/design/integration.md#server-sign-in)),
so owner-only permissions protect the store from every other local user.

## Token set

```json
{"version":1,"binding":"<64 lowercase hex>","access_token":"…","expires_at":1700000600,
 "refresh_token":"…","scope":"cache:read cache:write","obtained_at":1700000000}
```

| Field | Rule |
| --- | --- |
| `version` | exactly 1 |
| `binding` | the configuration binding below, 64 lowercase hex |
| `access_token` | 1–4096 bytes of visible ASCII (`0x21..=0x7e`), so it is always a valid `Bearer` header value and a valid `Secret` |
| `expires_at` | Unix seconds; `obtained_at + expires_in` from the token response |
| `refresh_token` | `null` or 1–4096 bytes of visible ASCII |
| `scope` | `null` or the granted scope string (space-separated scope tokens) |
| `obtained_at` | Unix seconds when the token request that produced it was sent |

Unknown fields are refused. `TokenSet` holds both tokens as `Secret`; its
`Debug` names the fields and renders both tokens as `Secret(<redacted>)`. It
implements no `Display`. Its JSON form exists only inside `seal`/`write`
and is zeroed after use.

The configuration binding is the lowercase hex SHA-256 of
`"stockhouse-token-binding-v1\n" + token_endpoint + "\n" + client_id + "\n" + private_tier + "\n"`,
using the normalized URLs from the [auth configuration](oauth.md#configuration).
A store written for one server, client or tier is therefore never used for
another: reading it is `StoreError::Mismatch`.

## Location and modes

The configuration names a directory (default `/var/lib/stockhouse/auth`) and
a mode. The mode is always explicit in the configuration; the module
defaults it to `sealed`.

| Mode | File | Content |
| --- | --- | --- |
| `sealed` | `<dir>/tokens.sealed` | the sealed format below, under a key read from a systemd credential |
| `file` | `<dir>/tokens.json` | the token set JSON in plaintext, protected only by owner-only permissions; used only when the configuration says `mode = "file"` |

A write or delete in one mode also removes the other mode's file, so
switching modes never leaves a stale plaintext copy behind. Reads consult
only the configured mode's file.

### Sealed format

| Bytes | Content |
| --- | --- |
| 0–7 | magic `SHTKSL01` |
| 8–39 | the configuration binding (32 raw bytes, in clear) |
| 40–63 | a 24-byte nonce from the operating system's random source (`getrandom`), fresh for every write |
| 64– | XChaCha20-Poly1305 ciphertext and 16-byte tag of the token set JSON |

The associated data is bytes 0–39, so the magic and binding are
authenticated. Reading checks, in order: length at least 80 bytes, magic and
binding (a different binding is `Mismatch`, anything else wrong is
`Corrupt`), then decryption (`Corrupt` on failure), then the token set rules
(`Corrupt`, including a JSON binding that differs from the header).

### Sealing key

The key is a systemd credential (default name `stockhouse-token-key`)
delivered with `LoadCredentialEncrypted=`, so at rest it is encrypted by
systemd with the TPM2 and/or host key and it appears in plaintext only in the
service's private `$CREDENTIALS_DIRECTORY`. It is read with
`SystemdCredential` (symlink, regular-file, size and content checks) on every
store operation and must be exactly 64 lowercase hex characters (after the one
trailing newline that `SystemdCredential` strips), decoded into 32 bytes held
in a zeroed buffer.

| Condition | Error |
| --- | --- |
| `CREDENTIALS_DIRECTORY` unset, or the credential missing or unreadable | `KeyUnavailable` |
| content that is empty, too large, not visible ASCII, or not 64 lowercase hex | `KeyMalformed` |

The module documents creating it once per machine (see
[integration](../../INTEGRATION/design/integration.md#server-sign-in)).

## Filesystem checks

Every operation first opens the directory with
`O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC` and checks it with `fstat`: missing is
`DirectoryMissing`; a symbolic link, a non-directory, an owner other than the
effective UID or any group or other permission bit is `DirectoryUnsafe`. The
client never creates the directory itself (the module's `tmpfiles` rule does,
`0700` and owned by the service user).

Reading the store file opens it relative to that directory with
`O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK` and checks with `fstat`:

| Condition | Result |
| --- | --- |
| absent | `Ok(None)`: not signed in |
| symbolic link | `Symlink` |
| not a regular file | `NotRegularFile` |
| owner other than the effective UID | `WrongOwner` |
| any group or other permission bit | `PermissionsTooOpen { mode }` |
| more than 65 536 bytes within the first 65 537 read | `TooLarge` |

Reads take no lock: writes replace the file atomically, so a reader sees the
old or the new token set, never a mixture.

## Atomic writes

A write (always under the [lock](#lock)):

1. removes stale temporary files `.tokens.*.tmp` left by a crashed writer;
2. creates `.tokens.<pid>.<16 random hex>.tmp` in the store directory with
   `O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC` and mode `0600`;
3. writes the whole sealed or plaintext content and calls `fsync`;
4. renames it over the mode's store file;
5. removes the other mode's store file if present;
6. `fsync`s the directory, so the rename and the removal are both durable.

If any step fails, the temporary file is removed and the previous store file
is left unchanged; the error is `Io { op }` naming the step only.

`delete` (under the lock) removes both mode files and `fsync`s the directory;
deleting an absent store succeeds.

## Lock

Refresh, login and logout serialize on an exclusive advisory lock
(`flock` through `std::fs::File::lock`) on `<dir>/lock`, created on first use
with `O_CREAT | O_NOFOLLOW | O_CLOEXEC` and mode `0600` and checked with
`fstat` like the store file: a symbolic link is `Symlink`, a non-regular
file `NotRegularFile`, another owner `WrongOwner` and any group or other
permission bit `PermissionsTooOpen { mode }`; nothing is locked, read or
written after a failed check. The lock is per open file
description, so it also serializes threads of one process. Acquisition polls
`try_lock` every 100 ms up to the configured wait (twice the request timeout,
default 60 s); exceeding it is `LockTimeout` and nothing is read or written.
The lock is released when the guard is dropped, including on panic or process
exit, so a crashed holder never wedges the others.

The lock exists because a refresh token is single use. If two processes sent
the same refresh token, a server that detects reuse may revoke the whole
grant family. Every refresher therefore re-reads the store after acquiring
the lock and refreshes only if the token set it then holds still needs it
([refresh](oauth.md#refresh)).

## Errors

`StoreError` (`Copy`, no content bytes): `DirectoryMissing`,
`DirectoryUnsafe`, `Symlink`, `NotRegularFile`, `WrongOwner`,
`PermissionsTooOpen { mode }`, `TooLarge`, `Corrupt`, `Mismatch`,
`KeyUnavailable`, `KeyMalformed`, `LockTimeout`, `Io { op: &'static str }`.
Its messages name the store and the failed check, never content, paths of
secret values or key bytes. For credential callers it maps to
`CredentialError`: `KeyUnavailable` and `KeyMalformed` keep their names,
`Corrupt` is `StoreCorrupt`, `Mismatch` is `StoreMismatch`, `LockTimeout` and
`Io` are `RefreshUnavailable`, and every other variant is `StoreUnsafe`.
