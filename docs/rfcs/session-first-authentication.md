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

## Follow-up slices

The next implementation slice adds `/auth/login`, `/auth/callback`, and
bootstrap-to-session issuance. A subsequent slice applies session admission
to HTTP, Web, TUI, and remote operator transports.
