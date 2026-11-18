# Server sign-in and refresh design

The client is an OAuth 2.0 public client (RFC 6749, no client secret). It
signs in with the authorization code grant over a loopback redirect (RFC 8252)
with S256 PKCE (RFC 7636), or with the device authorization grant (RFC 8628).
It refreshes the access token before it expires, serialized across processes,
and revokes the tokens on logout (RFC 7009). Access tokens are opaque; their
lifetime comes only from `expires_in`. Everything here uses only those public
RFCs and this repository's own protocol; docs and fixtures use
`example.invalid` hosts.

Crates:

| Crate | Owns |
| --- | --- |
| `stockhouse-token-store` | the [token store](store.md) and lock |
| `stockhouse-auth` | the configuration, the token and revocation endpoint client, refresh, `ServerCredential`, logout, status and the `source` factory |
| `stockhouse-signin` | PKCE, the loopback listener, the device flow and login; used only by the CLI |

## Configuration

`AuthConfig::parse` reads one TOML file (`deny_unknown_fields`; no field can
hold a token or key value). The module generates it into the store; it is
not secret.

```toml
version = 1
server = "https://example.invalid/"              # the server origin
client_id = "stockhouse-cli"
scopes = ["cache:read", "cache:write"]
private_tier = "https://cache.example.invalid/t/1/"
request_timeout_secs = 30                        # optional, 1–300, default 30

[endpoints]
authorization = "https://example.invalid/oauth/authorize"
token = "https://example.invalid/oauth/token"
device_authorization = "https://example.invalid/oauth/device"
revocation = "https://example.invalid/oauth/revoke"

[store]
mode = "sealed"                                  # or "file"
directory = "/var/lib/stockhouse/auth"
key_credential = "stockhouse-token-key"          # sealed only
```

| Condition | `AuthConfigError` |
| --- | --- |
| TOML syntax, unknown field (such as `token`), wrong type, missing field | `Parse` |
| `version` other than 1 | `UnsupportedVersion` |
| `server` not `https://` (or `http://` to a loopback IP), with a path other than `/`, a query, fragment or userinfo | `InvalidServer` |
| an endpoint not `https://` (or `http://` to a loopback IP), with a fragment or userinfo; `token`, `device_authorization` and `revocation` also with a query | `InvalidEndpoint { endpoint }` |
| an endpoint whose origin (scheme, host, port) differs from `server` | `ForeignEndpoint { endpoint }` |
| `private_tier` not `https://` (or `http://` to a loopback IP), with a query, fragment or userinfo, or not ending in `/` | `InvalidPrivateTier` |
| `client_id` empty, longer than 255 bytes or outside visible ASCII | `InvalidClientId` |
| `scopes` empty, a duplicate, or a scope with a character outside RFC 6749 `scope-token` (`%x21 / %x23-5B / %x5D-7E`) | `InvalidScope` |
| `request_timeout_secs` outside 1–300 | `InvalidTimeout` |
| `directory` relative, with a `.` or `..` component, or under `/nix/store`; `sealed` without a valid systemd credential name (`key_credential` non-empty, not `.`/`..`, no `/`); `file` with a `key_credential` | `InvalidStore` |

The `authorization` endpoint may keep a query (RFC 6749 §3.1); the client
appends its parameters to it. The private tier may have a different origin
from the server (a separate cache host). Error messages name the field, never
its value.

## HTTP client

The endpoint client is `reqwest` (already locked by the proxy) with
redirects disabled, the system certificate verifier for `https://` and no
trust roots for loopback `http://`, connect and per-read timeouts of
`request_timeout_secs` plus a total request timeout of the same length (so a
reply trickling in byte by byte cannot hold the store lock; exceeding it is
`Transient(Transport)`), and response bodies read to at most 65 536 bytes
(`TooLarge` beyond). Requests are `POST` with
`Content-Type: application/x-www-form-urlencoded`, `Accept: application/json`
and the public client's `client_id` in the body. No request ever carries an
`Authorization` header: the refresh token, authorization code, PKCE verifier
and device code travel only in the body to the token endpoint, and tokens
being revoked only in the body to the revocation endpoint. Form bodies and
response buffers are zeroed after use; `reqwest` takes its own copy of the
form body, which is not zeroed, and copies inside `reqwest` are not tracked
(as for the proxy's header, see [secrets](credentials.md#secret-values)).

The access token itself is sent only by credential consumers (the proxy and
the uploader) as `Authorization: Bearer` to the private tier, under their own
rules (`CLIENT-PROXY-CREDENTIAL-001`).

### Token responses

A token response (code exchange, device poll, refresh) is classified:

| Response | Classification |
| --- | --- |
| 200 with a JSON object: `access_token` (1–4096 visible ASCII), `token_type` equal to `Bearer` ignoring case, `expires_in` an integer 1–86 400, optional `refresh_token` (1–4096 visible ASCII), optional `scope` string | `Tokens` |
| 200 that breaks any of those rules, or is not JSON | `InvalidResponse` |
| 400 or 401 with a JSON object whose `error` is a known code | `OAuth(code)` |
| 400 or 401 otherwise | `OAuth(Other)` |
| 429, 500–599 | `Transient(Status)` |
| 3xx | `Transient(Redirect)` (never followed) |
| other statuses | `InvalidResponse` |
| connect, read or timeout failure | `Transient(Transport)` |

Known codes are `invalid_request`, `invalid_client`, `invalid_grant`,
`unauthorized_client`, `unsupported_grant_type`, `invalid_scope`,
`access_denied`, `authorization_pending`, `slow_down`, `expired_token` and
`temporarily_unavailable`; anything else is `Other`. `OAuth(temporarily_unavailable)`
counts as transient. Errors carry only the classification: never
`error_description`, `error_uri`, the body or a header value. `expires_at` is
the time the request was sent plus `expires_in`, so latency only shortens the
lifetime.

## Login

`stockhouse login --config <auth.toml> [--device]` (see
[CLI](../../INTEGRATION/design/integration.md#commands)) runs one flow, then
stores the token set. Every random value comes from the operating system
(`getrandom`); none is printed except the device user code.

### Loopback authorization code with PKCE

0. Preflight the store before any network contact: check the directory,
   load the sealing key (sealed mode) and acquire and release the lock once.
   A failure stops the login with its `StoreError`, so a sign-in never
   succeeds at the server only to fail to store its tokens.
1. Generate `state` (32 random bytes) and a PKCE `code_verifier` (32 random
   bytes), both base64url without padding (43 characters).
   `code_challenge` is base64url(SHA-256(`code_verifier`)) and
   `code_challenge_method` is `S256`.
2. Bind a TCP listener on `127.0.0.1:0` (IPv4 loopback only, never a
   wildcard), letting the kernel choose a port from its ephemeral range
   (always 1024–65535 on Linux). `redirect_uri` is
   `http://127.0.0.1:<port>/callback`.
3. Print the authorization URL to stdout: the authorization endpoint with
   `response_type=code`, `client_id`, `redirect_uri`, `scope` (the configured
   scopes joined by spaces), `state`, `code_challenge` and
   `code_challenge_method=S256` appended to any query it already has. The
   verifier never leaves the process. `state` and `code_challenge` appear
   only in this printed URL (which the operator must open), never in a log
   line or error text.
4. Serve HTTP/1.1 on the listener until the first `GET /callback`, at most
   300 s (`LoginTimeout`, listener closed). Connections are served
   concurrently, so an idle or slow connection cannot delay the callback. A
   failing `accept` ends the login with `Listener`. Other paths answer 404
   and other methods 405, without ending the wait (browsers ask for
   `/favicon.ico`).
5. The first `GET /callback` ends the wait whatever it contains; the listener
   is closed immediately after its response (one shot).
   - `state` absent or not equal to the generated value (a plain byte
     comparison: the listener is one shot, so there is no timing oracle to
     probe): answer 400 and fail with `StateMismatch`, sending no token request.
   - `error` present: answer 200 with a static page and fail with
     `AuthorizationDenied(code)`, where `code` is the known-code
     classification above; `error_description` is ignored.
   - otherwise `code` must be present and 1–4096 visible ASCII
     (`InvalidCallback` with a 400 answer otherwise); answer 200 with a static
     "you can close this window" page.
6. Exchange the code at the token endpoint: `grant_type=authorization_code`,
   `code`, `redirect_uri`, `client_id`, `code_verifier`. `Tokens` is stored;
   anything else fails with `TokenEndpoint(classification)`.

Responses from the listener are static: they never echo the request, the
code or the state, and every response carries `Cache-Control: no-store` and
`Referrer-Policy: no-referrer` so the callback URL is neither cached nor sent
onwards. A stray request before the callback (another path or method) is
answered 404 or 405 and ignored; any connection after the one-shot callback,
including a browser reload or a second tab, is refused because the listener
is closed.

### Device authorization

1. `POST` the device authorization endpoint with `client_id` and `scope`.
   A 200 JSON object needs `device_code` (1–4096 visible ASCII),
   `user_code` (1–64 visible ASCII or space), `verification_uri` (an
   `https://` URL, or `http://` to a loopback IP, every byte visible ASCII so
   no control character or terminal escape is ever printed), `expires_in` (1–3600) and
   optionally `verification_uri_complete` (same URL rule) and `interval`
   (1–60, default 5). Anything else is `DeviceResponse`; error statuses are
   classified as for token responses.
2. Print `verification_uri`, `user_code` and, when present,
   `verification_uri_complete` to stdout. The `device_code` is never printed
   or logged.
3. Poll the token endpoint with
   `grant_type=urn:ietf:params:oauth:grant-type:device_code`, `device_code`
   and `client_id`, waiting `interval` seconds before each request:
   - `Tokens`: store them; done.
   - `OAuth(authorization_pending)`: keep polling.
   - `OAuth(slow_down)`: add 5 s to `interval` for this and every later poll.
   - `OAuth(access_denied)`: stop with `DeviceDenied`.
   - `OAuth(expired_token)`: stop with `DeviceExpired`.
   - `Transient(_)` (including 429): add 5 s to `interval`, but not beyond
     60 s (an interval already above 60 s from `slow_down` is kept), and keep
     polling.
   - anything else: stop with `TokenEndpoint(classification)`.
4. Stop with `DeviceExpired`, without another request, once the next poll
   would be sent after `expires_in` seconds from the device response.

### Storing a login

The device flow runs the same preflight (step 0 above) before its first
request. Login then acquires the store lock, reads the current token set (if
any), writes the new one atomically, releases the lock, then, if a different
refresh token was replaced, revokes it best-effort
(`token_type_hint=refresh_token`), so a re-login does not leave the previous
grant usable. A failed revocation of the replaced token is reported as a
warning and does not fail the login. When the current store is `Corrupt` or
`Mismatch`, login overwrites it without revoking anything (its tokens cannot
be trusted or are for another server).

## Refresh

`ServerCredential` (`stockhouse-auth`) implements `CredentialSource` over the
configuration and store. Every `credential()` call re-reads the store (no
in-memory cache of tokens), so a login, refresh or logout by another process
takes effect on the next use. With `skew` = 60 s and `now` the current Unix
time:

1. Read the store. `None` is `SignedOut`. Store errors map as in
   [the store's error table](store.md#errors).
2. If `now < expires_at - skew` and the access token is not
   [rejected](#rejected-access-tokens), return it. No lock, no network.
3. Otherwise acquire the lock (`LockTimeout` → `RefreshUnavailable`, nothing
   sent) and read the store again. If the re-read token set now passes step 2
   (another process refreshed), release the lock and return its access token
   without a request.
4. Without a refresh token: `SignedOut` if `now >= expires_at` or the access
   token is rejected, otherwise return the current access token.
5. If a transient refresh failure happened less than 10 s ago (the
   in-memory backoff), send nothing: return the current access token if it
   has not expired and is not rejected, otherwise `RefreshUnavailable`.
6. Send `grant_type=refresh_token`, `refresh_token`, `client_id` to the token
   endpoint, still holding the lock:
   - `Tokens`: build the new set (keep the old refresh token if the response
     has none, as RFC 6749 §6 allows), write it atomically, then release the
     lock and return the new access token. A failed write is
     `RefreshUnavailable` and is logged once; the server may already have
     rotated the refresh token, so the next refresh may report signed out.
   - `OAuth(invalid_grant)`: the grant is over. Delete the store, release the
     lock, log once that the client is signed out, and return `SignedOut`.
   - `Transient(_)` or `OAuth(temporarily_unavailable)`: keep the store and
     start the 10 s backoff. If `now < expires_at` and the token is not
     rejected, return the current access token; otherwise
     `RefreshUnavailable`. Threads queued on the in-process mutex see the
     backoff at step 5, so one outage produces one request per process per
     10 s, not a burst.
   - any other `OAuth(_)` or `InvalidResponse`: keep the store and return
     `RefreshRejected`.

No path sends a refresh request without holding the lock, every refresh
request uses the refresh token read after acquiring it, and no refresh token
is sent again after the server answered it with 200. A refresh token whose
request got no reply (timeout, reset) is sent again by the next refresh after
the backoff; a server with reuse detection may treat that as reuse and end
the grant, which then shows as `SignedOut`. The protocol requires no
server-side grace window; this is a recorded deferral.

`credential()` is synchronous and may be called from synchronous code (the
uploader) or from a blocking-pool thread of an async runtime (the proxy). The
refresh request runs on a short-lived scoped OS thread with its own
current-thread Tokio runtime and a fresh HTTP client, so it never nests a
runtime inside the caller's and works the same from both kinds of caller.
`credential()` holds an in-process mutex around steps 3–6, so concurrent
callers in one process wait for one refresh instead of polling the file lock.
Only `reqwest`'s async API is used, and only on that private runtime; no
blocking HTTP API is ever called from the proxy's runtime or its blocking
pool.

### Rejected access tokens

`ServerCredential::invalidate(rejected)` reads the store, then:

1. No token set is stored (or the store cannot be read): return `false` at
   once, recording nothing.
2. The stored access token differs from `rejected` (another process has
   already replaced it): return `true`; the retry uses the stored token.
3. The set has no refresh token, was obtained less than 60 s ago, or this
   source performed an invalidation-forced refresh less than 60 s ago: a
   retry is not worthwhile on its own (a token that fresh being refused is a
   server-side denial, and refreshing again would only churn the grant). But
   a server may stop accepting the old access token as soon as it rotates,
   before the refreshing process has written the new one, so first wait for
   the store lock (at most the lock wait; a refresh in progress holds it) and
   read the store again: return `true` if it now holds a different access
   token, otherwise `false`, recording nothing. Without a refresh token the
   next `credential()` is then `SignedOut` for the rejected token.
4. Otherwise it records the SHA-256 of the rejected access token in memory and
returns `true`. Step 2 treats a stored access token with that digest as
needing refresh, whatever its expiry. The record is dropped as soon as the
store holds a different access token. So after a 401:

- if another process has already refreshed, the next `credential()` returns
  the newer token from the store without a request;
- otherwise it refreshes once under the lock;
- if the refresh ends the grant, the next call is `SignedOut`.

A consumer retries at most once per request (proxy:
`CLIENT-PROXY-CREDENTIAL-006`); `invalidate` never sends anything itself.

### Explicit refresh

`stockhouse refresh --config` reads the store, treats the stored access token
as [rejected](#rejected-access-tokens) (without the 60 s limits of
`invalidate`, which apply to automatic retries only) and then runs
`credential()`. So it
refreshes once, unless another process replaced the token between that read
and acquiring the lock, in which case it reports the newer token without a
second request. It reports `refreshed`, `signed out` or the failure.

## Logout

`stockhouse logout --config`:

1. Acquire the lock and read the store. `None`: report "not signed in", no
   request, success. `Corrupt` or `Mismatch`: delete the store without any
   request and report that nothing was revoked (its tokens cannot be
   trusted or belong to another server); success.
2. If the set has a refresh token, `POST` the revocation endpoint with
   `token=<refresh token>`, `token_type_hint=refresh_token` and `client_id`.
   Revoking the refresh token ends the grant, and with it the grant's access
   tokens, server-side, so the access token is not revoked separately. Only
   200 counts as revoked (RFC 7009 §2.2: the server answers 200 for an
   already invalid token). Any other status, redirect or transport failure is
   a failed revocation; there is no retry. Without a refresh token nothing is
   sent and the report says nothing was revoked.
3. Delete the store, whatever step 2 returned, then release the lock.
4. Report failure only if the revocation failed (`RevocationFailed(classification)`:
   signed out locally, the server may still honor the grant) or the store
   could not be deleted.

Holding the lock through step 3 means a concurrent refresh either completes
before logout reads the store (and logout revokes the new tokens) or starts
after the store is gone (and reports `SignedOut`).

## Status

`stockhouse status --config` reads the store without the lock and without
network and prints one JSON object:

```json
{"signed_in":true,"expires_at":1700000600,"expires_in_secs":512,"refreshable":true,
 "scope":"cache:read cache:write","store":"sealed"}
```

When there is no token set it prints `{"signed_in":false,"store":"sealed"}`;
when the store cannot be read it prints
`{"signed_in":false,"store":"sealed","error":"<store error kind>"}`.
`signed_in` is true when a token set is stored and either the access token
has not expired or it has a refresh token. Status never prints a token, the
binding or the key.

## Source factory

`stockhouse_auth::source(spec: Option<&CredentialSpec>) -> Result<Source, AuthConfigError>`
builds file and systemd sources through `stockhouse_credentials::source`
(`Source::Static`), and for `Server(path)` loads the configuration (an
unreadable file is `AuthConfigError::Read`, never echoing content) and
returns `Source::Server(Arc<ServerCredential>)`. `Source::shared()` yields
the `Arc<dyn CredentialSource>` a consumer stores.
`ServerCredential::private_tier()` exposes the configured private tier URL so
a consumer can check, before using it, that it sends the token only to the
tier it was issued for (`CLIENT-PROXY-CONFIG-012`).

## Logging and secrecy

Library code reports through return values; the CLI and the proxy log. Each
of these is one line on stderr naming the operation and the classification:
a refresh failure, a signed-out transition, a failed revocation, a failed
write. No log line, error message, `Debug` output or CLI output contains an
access or refresh token, an authorization code, the PKCE verifier, `state`,
the device code, the sealing key, a response body or an `error_description`.
The one exception is the authorization URL that `login` prints to stdout for
the operator to open: it carries `state` and `code_challenge` (never the
verifier), as the flow requires.
None of them appears in process arguments, environment variables or files
other than the token store.
