# RFC: Origin/CSRF protection for cookie-authenticated HTTP writes

## Status

Implemented with issue #3315.

## Contract

Holon protects unsafe browser writes at both the `/api` and `/apps` HTTP
boundaries. The guard is always enabled and is independent of CORS.

For `POST`, `PUT`, `PATCH`, and `DELETE` requests:

- Bearer-authenticated requests retain their existing compatibility.
- Unix-socket `trusted-local` requests retain their existing compatibility.
- Requests using the ambient `holon_session` cookie must provide an allowed
  browser source.
- Browser session exchange and pairing redemption are protected even before a
  session cookie exists.
- Native credential endpoints that do not set a browser cookie retain their
  credential-authentication behavior and do not require browser-origin headers.

An allowed source is:

1. an `Origin` whose `http`/`https` scheme, host, and port match the request
   `Host`; or
2. an exact configured origin from `api.csrf.trusted_origins`.

If `Origin` is absent, `Sec-Fetch-Site: same-origin` or a same-origin
`Referer` is accepted as a fallback. Otherwise the request fails closed with
HTTP `403` and machine code `csrf_origin_rejected`.

## Configuration

`api.csrf.trusted_origins` is an optional list of explicit `http://` or
`https://` origins. Entries may include a port but cannot contain credentials,
paths, queries, fragments, or the wildcard `*`. Same-origin matching and
configured matching use scheme plus host and port; `Forwarded` and
`X-Forwarded-*` headers are intentionally ignored.

Cookie attributes remain unchanged by this RFC: the existing `HttpOnly` and
`SameSite=Lax` behavior is preserved, and no new `SameSite`, `Secure`, or
`Domain` configuration is introduced.

## Rationale and boundary

CORS is a response-read policy and does not prevent a browser from submitting
ambient-cookie writes. `SameSite=Lax` remains useful browser behavior but is
not the sole authorization boundary for same-site, cross-origin deployments,
future cookie-policy changes, or browser differences. Explicit source
validation closes that gap without changing non-browser bearer or trusted-local
clients.
