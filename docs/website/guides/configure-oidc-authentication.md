---
title: Configure OIDC authentication
summary: Set up OpenID Connect single sign-on, attribute prompts in shared team runtimes, tune session policies, and audit user activity.
order: 180
---

# Configure OIDC authentication

By default, Holon uses local control tokens for authentication. For shared team
servers or remote deployments, you can switch to OpenID Connect (OIDC). Team
members log in through your existing identity provider (IdP), and Holon records
which user prompted an agent, enabling multiple team members to share an
assistant with clear accountability. To see how OIDC fits into the broader team
collaboration workflow, see [Share AI assistants with your team](./share-with-your-team.md).

This guide covers registering an OIDC client, configuring Holon, tuning
session timeouts, and verifying logins.

## Prerequisites

- A running Holon instance reachable by your users (for example via `--access tunnel`, `--access tailnet`, or a reverse proxy).
- An OIDC-compliant identity provider, such as [self-hosted Keycloak](./keycloak-oidc-authentication.md), Okta, Authentik, [Google Workspace](./google-workspace-oidc-authentication.md), [self-hosted GitLab CE](./gitlab-ce-oidc-authentication.md), or Microsoft Entra ID.
- Administrative access in your IdP to register a new client application.

> **Security note:** Holon requires HTTPS for the OIDC issuer URL and callback
> endpoint. The callback alone may use HTTP when its host is exactly
> `localhost`; the issuer must still use HTTPS.

Keep administrative access to the runtime host while changing authentication,
so you can correct a failed configuration. OIDC identifies signed-in users; it
does not create a sandbox or imply per-user workspace isolation. See
[Security and execution boundaries](../concepts/security-and-execution-boundaries.md).

## Step 1: Register Holon in your IdP

> **Provider-Specific Guides:**
> - If your team uses Google Workspace, see the dedicated walkthrough: [Configure OIDC Single Sign-On with Google Workspace](./google-workspace-oidc-authentication.md), which covers Internal user type enforcement, admin access controls, and verified test scopes.
> - If you want a completely self-hosted setup without external identity dependencies, see [Configure OIDC Single Sign-On with Self-Hosted GitLab CE](./gitlab-ce-oidc-authentication.md), which covers Docker deployment, Let's Encrypt certificates, confidential client setup, and verified test scopes.
> - If you use a dedicated open-source identity platform as your organization-wide SSO hub, see [Configure OIDC Single Sign-On with Self-Hosted Keycloak](./keycloak-oidc-authentication.md), which covers Docker deployment, realm and confidential client setup, and verified test scopes.

Create a new OpenID Connect application in your identity provider:

1. **Client ID**: Choose an identifier, such as `holon`.
2. **Client Authentication**: Register a confidential client for the authorization
   code login flow and generate a **Client Secret**. This is user sign-in, not
   the client-credentials grant.
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

Keep the secret out of Holon's configuration and source control. Supply it
through an environment variable where the daemon runs:

```bash
export HOLON_OIDC_CLIENT_SECRET="your-oidc-client-secret"
```

If you run Holon as a service or container, supply it through that process's
secret or environment mechanism. Restrict access to any backing environment
file. Exporting it in an unrelated shell does not update an existing service.

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
# Idle session lifetime in seconds (default: 604800, or 7 days)
# Authenticated activity refreshes this timer.
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
holon serve --access local
```

Use the same protected network endpoint or reverse proxy as before. If you
also change the public hostname, update the registered redirect URI. Direct
non-loopback, LAN, and tailnet CLI listeners still require a configured
control token at startup; OIDC login does not remove that listener check.

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
/api/agents/{agent_id}/messages/{message_id}`, check who submitted the prompt.
That attribution does not mean the user individually approved every later
tool call.

### 4. Verify a shared task with two test users

For team deployments where multiple users interact with the same agent runtime,
verify that individual sessions produce attributable tasks and that team members
can follow shared work according to your intended policy:

1. **Pre-flight scope table**: Agree on the testing boundaries before running the verification:

   | Scope item | Setting in test |
   | --- | --- |
   | **Test users** | Two provisioned test accounts (e.g. `alice@example.com` and `bob@example.com`) |
   | **Target agent** | An existing shared agent (e.g. `team-assistant`) |
   | **Permitted shared task** | A bounded, read-only check (e.g. inspecting a status report or build result) |
   | **Expected sharing boundary** | Both members can inspect the task result; each prompt records its own `actor_id` |
   | **Out of scope** | File system sandboxing or per-user workspace isolation (not provided by OIDC) |

2. **User A submits the task**:
   - Alice logs in via browser session A and submits a prompt: *"Check the build status of commit 6a21e3c4"*.
   - Alice receives a WorkItem or task ID.
   - Inspecting the message record confirms `actor_display_name: "Alice Chen"` and Alice's `actor_id`.

3. **User B follows up on the shared result**:
   - In an independent browser window (or private session), Bob logs in as `bob@example.com`.
   - Bob navigates to the shared agent, locates Alice's completed task, and submits a follow-up query: *"Summarize the verification items from that check"*.
   - The message transcript records Bob's distinct `actor_id` and display name while maintaining context continuity on the shared WorkItem.

4. **Verify audit trail**:
   - Inspect the agent transcript via `GET /api/agents/{agent_id}/messages`:
   - Both prompts appear in order, each with its authenticated user attribution (`actor_id` and `actor_display_name`).

> **Security & isolation boundary:**
> - **Authentication vs Authorization vs Execution:** OIDC verifies *who* the user is and attributes prompt submissions. It does not provide multi-tenant filesystem virtualization, container boundaries, or per-user workspace isolation.
> - **Execution environment:** Tools invoked by agents execute under the daemon process user on the host. To isolate untrusted workloads, configure separate daemon instances or containerized host environments. See [Security and execution boundaries](../concepts/security-and-execution-boundaries.md) and [Agents in a small team](../blog/agents-in-a-small-team.md).
> - **Testing requirements:** End-to-end multi-user verification requires an authorized non-production test instance and two test IdP credentials. In environments without multi-user test credentials, verify single-user attribution (Step 3) and mark multi-user task tests as pending authorization.

### 5. Log out

To end a session, click **Log out** in the Web GUI or issue:

```bash
curl -X POST -H "Authorization: Bearer <session-token>" \
  https://<your-holon-host>/api/auth/session/logout
```

Logout invalidates the session record. Browser logout also clears its cookie;
a separate `curl` request does not clear a cookie in your browser.

### 6. Verify rejection of unauthorized requests

Make a request without a cookie or bearer credential:

```bash
curl -i https://<your-holon-host>/api/auth/session/me
```

Expect an authentication error rather than a user identity. Repeat with the
revoked session credential after logout and confirm that it is rejected.
Use a private browser window to check the sign-in flow separately from an
already authenticated session.

## Troubleshooting

- **Redirect URI mismatch**: Ensure the callback URI in your IdP matches `auth.oidc.redirect_uri` byte for byte, including protocol (`https://`), port, and trailing path (`/api/auth/oidc/callback`).
- **Invalid issuer URL**: The issuer URL must match the `iss` claim in ID tokens and serve `/.well-known/openid-configuration` over HTTPS.
- **Secret not found**: Verify that the environment variable named in `auth.oidc.client_secret_env` is exported in the daemon process environment.
- **Invalid TTL configuration**: If you configure `auth.session.absolute_ttl_seconds`, it must be greater than or equal to `auth.session.idle_ttl_seconds`. Otherwise, Holon fails configuration validation on startup.
- **Login works but the wrong identity appears**: Inspect `/api/auth/session/me`
  in that same session and check the IdP account used. Do not infer identity
  from a display name alone.
- **Logged-out browser immediately signs in again**: Check whether the IdP
  still has an active browser session. Holon session logout does not imply
  organization-wide IdP logout.

## See Also

- [HTTP Control Plane reference](/reference/http-control-plane.md) — API endpoints for session exchange, inspection, and logout
- [Configuration reference](/reference/configuration.md) — Full list of `auth.*` options
- [Connect to a remote Holon runtime](/guides/connect-remote-runtime.md) — Remote access options and networking
- [Use the Web GUI](/guides/use-web-gui.md) — Working with agents and tasks in the browser
- [Security and execution boundaries](../concepts/security-and-execution-boundaries.md) — Authentication, authorization, and isolation boundaries
- [Agents in a small team](../blog/agents-in-a-small-team.md) — Shared team agent workflows and attribution
