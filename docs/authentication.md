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
      "idle_ttl_seconds": 604800
    }
  }
}
```

`absolute_ttl_seconds: null` disables the absolute session lifetime. The
default idle lifetime is 604,800 seconds (7 days), and activity refreshes the
idle expiry. A finite absolute lifetime may still be configured; it must be
positive and no shorter than the idle lifetime. For compatibility, persisted
configuration using `0` for the absolute lifetime is normalized to `null`.
When `idle_ttl_seconds` is omitted, the derived default is clamped to a
shorter explicit absolute lifetime so existing configurations keep loading.

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

Cookie Origin protection is disabled by default so browser sessions continue to
work through Tailscale Serve, HTTPS termination, reverse proxies, custom
domains, and standalone Vite proxies without deployment-specific configuration.
To opt into strict protection, configure one or more exact
`api.csrf.trusted_origins` values. Once the list is non-empty, unsafe requests
authenticated by the `holon_session` cookie must include a same-origin `Origin`
(matching request scheme, host, and port), or an exact configured origin. When
`Origin` is absent, same-origin `Sec-Fetch-Site` or `Referer` headers are
accepted as browser fallbacks; otherwise the request is rejected with HTTP
`403`. The guard is independent of CORS and applies to both `/api` and `/apps`,
including browser session exchange and pairing redemption.

In OIDC mode, normal API, SSE, and Web requests require an active session.
Missing, expired, revoked, or disabled-user sessions return HTTP `401` with the
`auth_required` error code. Bootstrap/session exchange, OIDC callback, callback
ingress, `/login`, and webhook routes retain their separate non-session
credentials and are not treated as browser sessions. The Web GUI uses the
Holon service's same origin for all API, SSE, and login requests.

## Native OIDC login

Native apps initiate `/auth/oidc/native/start` with a fresh random state and
an S256 challenge derived from a locally retained, high-entropy PKCE verifier.
`code_challenge_method` may be omitted or set to `S256`; explicit `plain` or
other methods are rejected. The callback's `ticket` is a two-minute, single-use
authorization code, not a bearer session. Redeeming it through
`/auth/session/exchange/native` requires the matching `native_verifier`.
Wrong or missing proof cannot consume the code, and ordinary session exchange
cannot redeem it. Intercepting the custom-scheme callback therefore does not
grant a session; expiry and atomic consumption prevent replay.

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
