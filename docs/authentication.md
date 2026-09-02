# HTTP authentication

Holon supports local control-token authentication and session-first OIDC
authentication for its HTTP surface.

## OIDC mode

Configure an OIDC provider in the runtime configuration:

```json
{
  "auth": {
    "mode": "oidc",
    "oidc": {
      "issuer_url": "https://id.example.com",
      "client_id": "holon",
      "client_secret_env": "HOLON_OIDC_CLIENT_SECRET",
      "redirect_uri": "https://holon.example/api/auth/oidc/callback"
    },
    "session": {
      "absolute_ttl_seconds": 28800,
      "idle_ttl_seconds": 1800
    }
  }
}
```

`issuer_url` and OIDC endpoints must use HTTPS. A localhost callback may use
HTTP for local development.

Open `/api/auth/oidc/start` in a browser to begin login. The callback creates a
session and sets the `holon_session` HttpOnly cookie before redirecting to `/`.
The same session can be supplied to API clients as
`Authorization: Bearer <session>`. `POST /api/auth/session/logout` revokes the
current session and clears the cookie.

In OIDC mode, normal API, SSE, and Web requests require an active session.
Missing, expired, revoked, or disabled-user sessions return HTTP `401` with the
`auth_required` error code. Bootstrap/session exchange, OIDC callback, callback
ingress, and webhook routes retain their separate non-session credentials and
are not treated as browser sessions.
