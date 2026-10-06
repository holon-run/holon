# Session-First Authentication

## Status

Accepted for the first authentication storage and configuration slice of
Issue #2735. HTTP/OIDC protocol handlers are implemented in a later slice.

## Context

Holon supports a local, single-operator deployment as well as remote
deployments that need OIDC-backed identities. Sending a long-lived control
token with every request makes revocation, expiry, device management, and
audit difficult. The authentication model therefore treats an opaque session
as the normal request credential, regardless of how that session was created.

## Contract

### Authentication modes

`local` is the default mode. It is intended for a single operator and does
not require an OIDC provider. A local bootstrap/recovery credential can be
exchanged for an opaque session by a future authentication endpoint.

`oidc` requires an HTTPS issuer and client ID. OIDC authorization-code/PKCE
login creates the same opaque session type after the callback is validated.
Redirect URIs may use HTTPS or target `localhost` for local development.

Configuration rejects an OIDC provider in `local` mode and rejects `oidc`
mode without a provider. Session absolute and idle TTLs must both be
positive, and idle TTL must not exceed absolute TTL.

### Principals and sessions

An authenticated request is associated with a principal. OIDC principals are
identified by the pair `(issuer, subject)` and have a stable internal
`user_id`. Display name and email are profile attributes, not identity keys.

Sessions are random opaque values. Runtime storage keeps only a SHA-256
digest (`session_digest`), never the raw session value. A session records its
user, authentication method, creation/expiry timestamps, last-seen timestamp,
and optional revocation timestamp. A session is active only when it is not
revoked, has not passed its absolute expiry, and has not exceeded its idle
expiry.

Interactive clients should use a cookie jar or an equivalent secure session
store. A bearer form may be supported by protocol adapters, but it carries
the opaque session rather than a long-lived control token.

### Bootstrap and recovery credentials

Static control credentials are bootstrap/recovery credentials, not the normal
per-request credential. They are stored as digests, have an expiry, may be
bound to a user and scope, and can be revoked. Redemption must atomically
mark a credential consumed so concurrent requests cannot redeem it twice.

The initial implementation intentionally keeps transport and endpoint policy
out of the domain/storage module. A later HTTP slice defines how a local
operator or an explicitly configured recovery path presents the credential.
Unix-socket admission remains a separate trusted local channel and must not
be confused with an unauthenticated TCP listener.

### OIDC login transactions

OIDC state, nonce, and transaction values are stored as digests. Login
transactions have a bounded lifetime and a consumed marker, allowing the
callback handler to enforce one-time use without persisting protocol secrets.

### Native callback proof binding

The custom URI scheme is a delivery channel, not an application identity.
Native login requires `state` and an S256 `code_challenge` at
`/auth/oidc/native/start`. An omitted `code_challenge_method` means S256;
an explicit method other than `S256` (including `plain`) is rejected.
The application generates a fresh 32-byte random
verifier and keeps it in platform secure storage for at most ten minutes.
This proof is independent of the daemon-to-provider PKCE verifier.

The login transaction retains only the native challenge. Its callback ticket
has scope `native-session:<challenge>` and expires after two minutes. The
callback URL contains state, ticket and `code_challenge_method=S256`, never
the verifier. Updated apps reject callbacks without the method marker so they
do not silently downgrade when connected to an older daemon. Exchanging it at
`/auth/session/exchange/native` requires `native_verifier`; all bootstrap
exchange routes enforce the same scope check. A wrong or missing proof neither
issues a session nor consumes the ticket. Validation and consumption are atomic.
Intercepting the callback can deny delivery but cannot redeem its ticket.

Manual local tokens, recovery credentials, pairing, and browser OIDC are
unchanged. Older native apps must update to initiate OIDC; we do not fall back
to an unbound bearer callback. In-flight native transactions created by an old
daemon are rejected and restarted. This does not impose HTTPS-only on LANs
or bypass TLS certificate validation.

## Runtime database boundary

The runtime database contains:

- `auth_users`: internal user records keyed externally by issuer and subject.
- `auth_sessions`: opaque-session digests and lifecycle state.
- `auth_bootstrap_credentials`: one-time or short-lived bootstrap/recovery
  credential digests and atomic consumption state.
- `auth_login_transactions`: bounded OIDC transaction digests and consumption
  state.

Repositories operate on domain records and timestamps. They do not issue raw
credentials, perform OIDC discovery, or make transport authorization
decisions. Those responsibilities belong to the authentication protocol and
admission layers.

## Security invariants

1. Raw session, bootstrap, state, and nonce values are not persisted.
2. Expiry and revocation are checked at admission time.
3. Bootstrap redemption is single-use under concurrency.
4. OIDC identity matching uses issuer plus subject, never email alone.
5. Local mode remains usable without an OIDC provider.

### Menu-initiated pairing (local mode)

The macOS menu can request a two-minute, single-use pairing ticket from the
local daemon (`POST /api/auth/pairing/issue`) using an explicit valid control
token or existing session, even if general control admission is optional.
Trusted Unix admission is also allowed. OIDC mode rejects issuance; a menu must not bypass OIDC with
local credentials. The issuer keeps only a digest in bounded in-memory state.
Consumption is atomic and removes the ticket even if subsequent session
creation fails; daemon restart invalidates all outstanding tickets.
The store is daemon-wide rather than listener-local: a menu mints the ticket
over the loopback listener and its holder redeems it from the advertised LAN or
Tailscale address, so every listener of one daemon must serve the same tickets.

The browser receives the ticket in a `/login#pair=...` fragment, removes the
fragment from history before POSTing it to `/api/auth/pairing/redeem`, and gets
the normal HttpOnly session cookie. The Android client exchanges the
same ticket at `/api/auth/pairing/redeem/native` for an opaque session
credential after confirming the target and, for HTTP, the plaintext risk.
Neither QR nor browser URL contains the long-lived control token.
The QR is created only on operator request and is hidden on timeout.

The fragment avoids HTTP request-line and referrer leakage, **not** network
eavesdropping: plaintext LAN traffic still exposes ticket redemption and
session cookies to an on-path party. Prefer Tailscale HTTPS; the menu explicitly
warns when the selected destination is HTTP. Android redeems only after
confirmation and saves the revocable session credential.

## Follow-up slices

The next implementation slice adds `/auth/login`, `/auth/callback`, and
bootstrap-to-session issuance. A subsequent slice applies session admission
to HTTP, Web, TUI, and remote operator transports.
