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
      "absolute_ttl_seconds": null,
      "idle_ttl_seconds": 86400
    }
  }
}
```

`absolute_ttl_seconds: null` disables the absolute session lifetime. The
default idle lifetime is 86,400 seconds (24 hours), and activity refreshes the
idle expiry. A finite absolute lifetime may still be configured; it must be
positive and no shorter than the idle lifetime. For compatibility, persisted
configuration using `0` for the absolute lifetime is normalized to `null`.

`issuer_url` and OIDC endpoints must use HTTPS. A localhost callback may use
HTTP for local development.

Open `/login` in a browser to begin login. In OIDC mode the page provides an
organization-login button and starts the OIDC flow after discovering the
configured authentication mode. In local mode, enter the static control token
on the same page; it is exchanged once for an HttpOnly `holon_session` cookie.
The callback creates the same cookie before redirecting to `/`.
The same session can be supplied to API clients as
`Authorization: Bearer <session>`. `POST /api/auth/session/logout` revokes the
current session and clears the cookie.

When a request presents both a Bearer session credential and a session cookie,
the runtime tries the Bearer credential first and falls back to the cookie. A
stale Bearer token (for example a static control token cached by a browser
before the deployment switched to OIDC) therefore cannot mask a valid session
cookie.

In OIDC mode, normal API, SSE, and Web requests require an active session.
Missing, expired, revoked, or disabled-user sessions return HTTP `401` with the
`auth_required` error code. Bootstrap/session exchange, OIDC callback, callback
ingress, `/login`, and webhook routes retain their separate non-session
credentials and are not treated as browser sessions. The Web GUI uses the
Holon service's same origin for all API, SSE, and login requests.

## Message attribution

Operator messages created through the control plane (`POST
/api/control/agents/{agent_id}/prompt`) record who sent them in the message
origin:

- In OIDC mode, the origin is the authenticated user: `actor_id` is the stable
  user id (for example `oidc-<uuid>`), and `actor_display_name` snapshots the
  user's display name at send time. When the user has no name claim, the user
  id is used as the display name fallback. Persisted messages therefore stay
  self-contained and do not drift when a user is renamed later.
- In local mode, the static control token is a shared control credential, not a
  per-user identity. Messages keep the stable `actor_id` of `control` and no
  `actor_display_name`. Local deployments see no per-user attribution, which
  matches the single-operator deployment model.

The origin is carried on `GET /api/agents/{agent_id}/messages/{message_id}`,
`messages:batchGet`, and the `message_enqueued` SSE event, so history and
real-time views observe the same attribution fields.
