---
title: Configure OIDC Single Sign-On with Google Workspace
summary: Create an OAuth client in Google Cloud, configure Google Workspace as Holon's identity provider, enforce organization access boundaries, and verify single sign-on.
order: 185
---

# Configure OIDC Single Sign-On with Google Workspace

Google Workspace can serve as an OpenID Connect (OIDC) identity provider for Holon: team members log in with their organization accounts, allowing Holon to accurately attribute every prompt to the individual user. This guide walks through creating the Google Cloud project, configuring OAuth credentials, enforcing organization access, configuring the Holon runtime, and verifying the integration.

For general OIDC concepts (issuer, client ID, redirect URI, session TTL, and prompt attribution), see [Configure OIDC authentication](./configure-oidc-authentication.md). This guide focuses specifically on Google Workspace.

## Distinguish Two Sets of Credentials

Connecting Google Workspace to Holon involves two distinct scenarios with very different credentials and security profiles. Never combine them into a single client:

| Goal | Client Type | Required APIs | Credential Longevity |
| --- | --- | --- | --- |
| **User Sign-In to Holon** (this guide) | Web application (confidential client) | None (no Workspace APIs required; only `openid`, `profile`, `email`) | Re-issued on each login; does not rely on refresh tokens |
| Agent reading/writing Gmail / Calendar / Drive on behalf of users | Dedicated client + `offline` consent | Target Workspace APIs must be enabled | Relies on refresh tokens; affected by consent screen User Type |

This guide covers only user authentication (the first row). Agent data integration with Google APIs (the second row) should always use separate credentials and scopes to prevent login access from granting broad data access permissions.

## Prerequisites

- Administrative access in your Google Workspace organization (to configure OAuth consent screens and API access controls).
- A reachable Holon deployment. Production environments must use HTTPS; callbacks over plain HTTP are allowed only when the host is strictly `localhost`.
- This guide uses `example.com` and `holon.example.com` as placeholder domains. Replace them with your actual domain names.

## Step 1: Create a Google Cloud Project Under Your Organization

In the [Google Cloud Console](https://console.cloud.google.com), create a new project and select your Google Workspace organization as the parent. Equivalent command via `gcloud`:

```bash
gcloud projects create holon-sso --name="Holon SSO" --organization=<ORG_ID>
```

Organization parentage is critical: it determines whether you can select the **Internal** user type in the next step. Projects created under personal Gmail accounts do not belong to an organization and cannot select Internal.

No Workspace APIs need to be enabled for OIDC sign-in. If you later configure Gmail or Google Calendar integrations for agents, use a separate client.

## Step 2: Configure the OAuth Consent Screen (User Type = Internal)

Navigate to **APIs & Services → OAuth consent screen**:

1. Fill in the App name and user support email.
2. Under **User type**, select **Internal**.
3. Save the configuration.

Selecting **Internal** has two major benefits:

- **No Google App Verification Required**: Because only members of your organization can sign in, you do not need to submit the app to Google for OAuth verification.
- **No 7-Day Refresh Token Expiration in Testing**: External apps in Testing status have refresh tokens that expire after 7 days, causing team members to be logged out periodically.

> Avoid configuring the app as External in Testing status. That mode is intended only for short-lived experiments and will cause recurring authentication and integration failures in long-term operations.

## Step 3: Create an OAuth Client ID (Web Application)

Navigate to **APIs & Services → Credentials → Create credentials → OAuth client ID**:

| Field | Value |
| --- | --- |
| Application type | **Web application** |
| Name | `holon` (or your preferred name) |
| Authorized redirect URIs | `https://holon.example.com/api/auth/oidc/callback` |

For local testing on port `7878`, add an additional redirect URI: `http://localhost:7878/api/auth/oidc/callback`.

After saving, record the **Client ID** (formatted like `1234567890-xxxx.apps.googleusercontent.com`) and **Client secret**.

Two client type considerations:

- Holon uses the authorization code flow with client authentication, requiring a confidential client. **Web application** is the correct choice.
- Desktop (installed app) clients are suitable only for local loopback testing: their client secrets are not treated as confidential by Google, and `gcloud` cannot create generic Desktop clients (they must be created in the console).

## Step 4: Enforce Organization Access on the Google Side

Holon identifies *who* is signing in, but access boundaries must be enforced by the identity provider. Google Workspace provides two layers of enforcement:

1. **User type = Internal**: Users outside your organization cannot complete authorization. This is the baseline requirement.
2. **Admin Console App Access Control** (recommended): In the Google Workspace Admin console under **Security → Access and data control → API controls → App access control**, add the application by Client ID and set its policy to "Trusted" or "Limited" to restrict access to specific organizational units (OUs).

Regarding the `hd` (hosted domain) claim: While Google Workspace ID tokens include `hd=<your-domain>` and `email_verified=true`, **Holon does not currently evaluate the `hd` claim**. Holon requests the standard `openid profile email` scopes and verifies `iss`, `aud`, `exp`, `iat`, `nonce`, and the cryptographic signature without domain allowlisting. Therefore, do not assume setting a domain in Holon's configuration restricts organization access—this enforcement must be configured in Google Workspace.

## Step 5: Configure the Holon Runtime

The **Issuer URL is `https://accounts.google.com`** (without a trailing slash). Google's OpenID Connect discovery document is at `https://accounts.google.com/.well-known/openid-configuration`, and the `issuer` value must match byte-for-byte.

### 1. Provide the Client Secret

Do not store client secrets in plain text configuration files or version control. Pass the secret to the daemon using an environment variable:

```bash
export HOLON_OIDC_CLIENT_SECRET="<client-secret>"
```

For systemd or containerized deployments, inject the secret using service environment definitions or secure secret injection, ensuring appropriate file permissions. Running `export` in an unrelated shell session does not affect an already running daemon.

### 2. Configure Authentication Settings

```bash
holon config set auth.mode "oidc"
holon config set auth.oidc.issuer_url "https://accounts.google.com"
holon config set auth.oidc.client_id "1234567890-xxxx.apps.googleusercontent.com"
holon config set auth.oidc.client_secret_env "HOLON_OIDC_CLIENT_SECRET"
holon config set auth.oidc.redirect_uri "https://holon.example.com/api/auth/oidc/callback"
```

The `auth.oidc.redirect_uri` setting must match the authorized redirect URI registered in Google Cloud **byte-for-byte** (scheme, host, port, and path). Any mismatch causes Google to reject the request with `redirect_uri_mismatch`.

### 3. Configure Session Lifetime Policies

```bash
# Idle session timeout in seconds; valid activity resets the timer (default: 604800 / 7 days)
holon config set auth.session.idle_ttl_seconds 43200

# Optional absolute lifetime limit in seconds (must be >= idle_ttl_seconds)
holon config set auth.session.absolute_ttl_seconds 604800
```

### 4. Restart the Daemon

```bash
holon daemon restart
```

If your public hostname changes, update both the redirect URI in Google Cloud Console and `auth.oidc.redirect_uri` in Holon.

## Step 6: Verify the Setup

1. Open `https://holon.example.com/login`. The page should display **Continue with organization login**.
2. Click the button to navigate to the Google sign-in page, sign in with your organization account, and complete the redirect back to `/api/auth/oidc/callback`. The browser receives the `holon_session` cookie and redirects to the console.
3. Check the authenticated session:

   ```bash
   curl -b "holon_session=<session-cookie>" https://holon.example.com/api/auth/session/me
   ```

   Expected response contains `auth_method: "oidc"` and a `user_id` in the format `oidc-<uuid-v4>`.

4. Check audit attribution: In OIDC mode, each prompt attributes `actor_id` and `actor_display_name`, inspectable via `GET /api/agents/{agent_id}/messages`.
5. Verify unauthenticated request rejection:

   ```bash
   curl -i https://holon.example.com/api/auth/session/me
   ```

   The endpoint should return an unauthenticated HTTP error rather than user information.

We also recommend testing negative cases to verify validation enforcement: tokens with mismatched `aud`, expired tokens, tampered signatures, `alg=none`, unknown `kid`, and accounts from outside the organization. With User type set to Internal, non-organization accounts are blocked during Google's authorization flow.

## Troubleshooting

- **`redirect_uri_mismatch`**: The redirect URI in Google Cloud Console does not match `auth.oidc.redirect_uri`, or `localhost` was accidentally used in production.
- **`invalid_client` / Login Failure**: The Client ID is incorrect, the client secret environment variable was not supplied to the daemon, or the client was created as Desktop type instead of Web application.
- **`403 org_internal` or Empty Account Chooser**: The account does not belong to the Google Workspace organization, or it is blocked by Google Admin console App access control policies.
- **`invalid issuer`**: `issuer_url` must be exactly `https://accounts.google.com` (no trailing slash and no paths like `/oauth2`).
- **Login Succeeds but User Identity Is Mismatched**: Call `/api/auth/session/me` to inspect the authenticated identity instead of inferring it from display names.
- **Logging Out and Refreshing Logs Back In Immediately**: Logging out of Holon invalidates the Holon session cookie, not Google's session. These are separate authentication layers.

## Scope of Verification in This Guide

- **Verified on a Real Google Workspace Organization**: Discovery document retrieval, JWKS signature verification, claims validation (`iss`, `aud`, `azp`, `exp`, `iat`, `nonce`), `hd` and `email_verified` inspection, authorization code exchange, and token refresh. Negative cases (personal Gmail without `hd`, mismatched `hd`, `email_verified=false`, substituted `aud`, expired tokens, signature tampering, `alg=none`, unknown `kid`, forged refresh tokens) were all correctly rejected.
- **Client Type Used in Protocol Testing**: Desktop client with `127.0.0.1` loopback callback to verify Google protocol behavior, **not** an end-to-end Holon web browser flow. Production setups must use Web application clients as described in Step 3.
- **Not Verified in Real Deployments**: End-to-end web browser login on a production Holon instance, Google Admin console OU-level app access control enforcement, `/revoke` sign-out coordination, and exact UI error banners presented to external accounts during Google consent.
- References: Google OpenID Connect discovery document (`https://accounts.google.com/.well-known/openid-configuration`) and integration test logs from 2026-11-01.
