---
title: Configure OIDC authentication
summary: Set up OpenID Connect single sign-on, configure session policies, and audit user prompts.
order: 17
---

# Configure OIDC authentication

By default, Holon uses local control tokens for authentication. For shared team
servers or remote deployments, you can switch to OpenID Connect (OIDC). Team
members log in through your existing identity provider (IdP), and Holon records
which user prompted an agent.

This guide covers registering an OIDC client, configuring Holon, tuning
session timeouts, and verifying logins.

## Prerequisites

- A running Holon instance reachable by your users (for example via `--access tunnel`, `--access tailnet`, or a reverse proxy).
- An OIDC-compliant identity provider, such as Keycloak, Okta, Authentik, Google Workspace, or Microsoft Entra ID.
- Administrative access in your IdP to register a new client application.

> **Security note:** Holon requires HTTPS for the OIDC issuer URL and callback
> endpoints in production. HTTP is only permitted when the callback host is
> `localhost`.

## Step 1: Register Holon in your IdP

Create a new OpenID Connect application in your identity provider:

1. **Client ID**: Choose an identifier, such as `holon`.
2. **Client Authentication**: Enable client credentials (confidential client) and generate a **Client Secret**.
3. **Redirect URI**: Set the callback URL:
   ```text
   https://<your-holon-host>/api/auth/oidc/callback
   ```
   If testing locally on port `7878`, use:
   ```text
   http://localhost:7878/api/auth/oidc/callback
   ```
4. **Scopes**: Ensure the client requests at least `openid`, `profile`, and `email`.

Note down the **Issuer URL**, **Client ID**, and **Client Secret**.

## Step 2: Store the Client Secret in an environment variable

Never store client secrets in configuration files on disk. Set the secret as
an environment variable where the Holon daemon runs:

```bash
export HOLON_OIDC_CLIENT_SECRET="your-oidc-client-secret"
```

If you run Holon as a systemd service or container, supply this variable in your
service unit or environment file.

## Step 3: Configure Holon

Set the authentication mode and provider parameters with `holon config set`:

```bash
# Switch to OIDC authentication mode
holon config set auth.mode "oidc"

# Set the issuer URL (must support OIDC discovery at /.well-known/openid-configuration)
holon config set auth.oidc.issuer_url "https://auth.example.com/realms/team"

# Set your registered Client ID
holon config set auth.oidc.client_id "holon"

# Point to the environment variable containing the secret
holon config set auth.oidc.client_secret_env "HOLON_OIDC_CLIENT_SECRET"

# Set the public callback URL (recommended behind reverse proxies)
holon config set auth.oidc.redirect_uri "https://holon.example.com/api/auth/oidc/callback"
```

## Step 4: Configure session policies

Holon issues HttpOnly session cookies for browsers and session credentials for
API clients. Configure how long sessions stay valid:

```bash
# Idle session lifetime in seconds (default: 86400, or 24 hours)
# Every user interaction refreshes this timer.
holon config set auth.session.idle_ttl_seconds 43200

# Optional absolute session lifetime in seconds (must be >= idle_ttl_seconds)
# When set, the session expires after this period regardless of activity.
holon config set auth.session.absolute_ttl_seconds 604800
```

To disable the absolute timeout and allow active users to stay signed in, omit
`auth.session.absolute_ttl_seconds` or set it to `null`.

## Step 5: Restart the daemon

Restart the daemon to apply authentication changes:

```bash
holon daemon restart
```

If running Holon interactively:

```bash
holon serve --access tunnel
```

## Verify the setup

### 1. Log in via the Web GUI

1. Open `https://<your-holon-host>/login` in your browser.
2. The login page detects OIDC mode and displays a **Continue with organization login** link.
3. Click the link to redirect to your identity provider.
4. Sign in. The IdP redirects back to `/api/auth/oidc/callback`, which sets a secure `holon_session` cookie and lands on the dashboard (`/`).

### 2. Verify session identity

Inspect the current session using the session API:

```bash
curl -b "holon_session=<session-cookie>" https://<your-holon-host>/api/auth/session/me
```

Or provide the session token as a bearer credential:

```bash
curl -H "Authorization: Bearer <session-token>" https://<your-holon-host>/api/auth/session/me
```

The endpoint returns the authenticated user identity and authentication method:

```json
{
  "ok": true,
  "user_id": "oidc-550e8400-e29b-41d4-a716-446655440000",
  "display_name": "Alice Chen",
  "auth_method": "oidc"
}
```

### 3. Check message attribution

In OIDC mode, every prompt sent through the control plane records the user's
identity in the message origin:

- `actor_id`: The persistent user identifier in Holon (formatted as `oidc-<uuid-v4>`).
- `actor_display_name`: The user's display name at send time (falls back to `actor_id` if no name claim exists).

When auditing agent transcripts or inspecting messages via `GET
/api/agents/{agent_id}/messages/{message_id}`, you can verify exactly who
triggered each action.

### 4. Log out

To end a session, click **Log out** in the Web GUI or issue:

```bash
curl -X POST -H "Authorization: Bearer <session-token>" \
  https://<your-holon-host>/api/auth/session/logout
```

This invalidates the session record on the server and clears the browser cookie.

## Troubleshooting

- **Redirect URI mismatch**: Ensure the callback URI in your IdP matches `auth.oidc.redirect_uri` byte for byte, including protocol (`https://`), port, and trailing path (`/api/auth/oidc/callback`).
- **Invalid issuer URL**: The issuer URL must match the `iss` claim in ID tokens and serve `/.well-known/openid-configuration` over HTTPS.
- **Secret not found**: Verify that the environment variable named in `auth.oidc.client_secret_env` is exported in the daemon process environment.
- **Invalid TTL configuration**: If you configure `auth.session.absolute_ttl_seconds`, it must be greater than or equal to `auth.session.idle_ttl_seconds`. Otherwise, Holon fails configuration validation on startup.

## See Also

- [HTTP Control Plane reference](/reference/http-control-plane.md) — API endpoints for session exchange, inspection, and logout
- [Configuration reference](/reference/configuration.md) — Full list of `auth.*` options
- [Connect to a remote Holon runtime](/guides/connect-remote-runtime.md) — Remote access options and networking
- [Use the Web GUI](/guides/use-web-gui.md) — Working with agents and tasks in the browser
