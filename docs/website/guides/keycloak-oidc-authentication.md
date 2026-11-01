---
title: Configure OIDC Single Sign-On with Self-Hosted Keycloak
summary: Deploy a self-hosted Keycloak instance with Docker, configure realms and clients, set up Holon's OIDC authentication, and verify end-to-end single sign-on without external accounts.
order: 187
---

# Configure OIDC Single Sign-On with Self-Hosted Keycloak

Keycloak is an open-source identity and access management platform that can serve as an OpenID Connect (OIDC) identity provider for Holon. Users, clients, and session policies are managed entirely within your own Keycloak instance, with no registration required with third-party providers. Unlike platforms such as GitLab where OAuth applications are a supplementary feature, Keycloak is a dedicated IdP designed to serve as an organization-wide single sign-on hub. This guide provides a complete walkthrough from deployment to end-to-end verification, reproducible on a single host.

For general OIDC concepts (issuer, client ID, redirect URI, session TTL, and prompt attribution), see [Configure OIDC authentication](./configure-oidc-authentication.md). This guide focuses specifically on self-hosted Keycloak.

## Prerequisites

- A host capable of running Docker (a production Keycloak container requires ~1 GB RAM, significantly lighter than GitLab).
- Keycloak must be served over HTTPS. Holon's OIDC client only trusts the built-in Mozilla root certificates; **self-signed CAs are rejected**. For private networks, we recommend using Tailscale's `tailscale cert` to issue public Let's Encrypt certificates without exposing ports to the public internet.
- An accessible Holon instance. The redirect URI supports HTTPS, or HTTP strictly with the hostname `localhost` (any port). IP literals like `127.0.0.1` or `::1` are rejected.

## Step 1: Deploy Keycloak

Start Keycloak in production mode (`start`), mounting your certificates into the container. `KC_HOSTNAME` must include the **complete external URL including the port**; otherwise, endpoints and the `issuer` claim in the discovery document will omit the port, causing issuer validation in Holon to fail:

```bash
mkdir -p keycloak/certs
cp site.crt site.key keycloak/certs/
docker run -d --name keycloak \
  -p 9444:8443 \
  -v "$PWD/keycloak/certs/site.crt:/opt/keycloak/conf/certs/tls.crt:ro" \
  -v "$PWD/keycloak/certs/site.key:/opt/keycloak/conf/certs/tls.key:ro" \
  -e KC_BOOTSTRAP_ADMIN_USERNAME=admin \
  -e KC_BOOTSTRAP_ADMIN_PASSWORD="$(openssl rand -hex 16)" \
  -e KC_HOSTNAME=https://keycloak.example.com:9444 \
  -e KC_HTTPS_CERTIFICATE_FILE=/opt/keycloak/conf/certs/tls.crt \
  -e KC_HTTPS_CERTIFICATE_KEY_FILE=/opt/keycloak/conf/certs/tls.key \
  quay.io/keycloak/keycloak:26.3 \
  start
```

Notes:

- Certificate files mounted from the host are typically root-only by default, whereas Keycloak inside the container runs as UID 1000. Run `chown 1000 keycloak/certs/site.key` on the host (or ensure UID 1000 has read access) before starting.
- `KC_BOOTSTRAP_ADMIN_USERNAME` and `KC_BOOTSTRAP_ADMIN_PASSWORD` are bootstrap administrator variables in Keycloak 26+ (replacing the legacy `KEYCLOAK_ADMIN`).
- `start-dev` (HTTP development mode) is not suitable: Holon validates that the issuer uses HTTPS, and production deployments require TLS.
- Readiness check: `curl https://keycloak.example.com:9444/realms/master/.well-known/openid-configuration` returns 200 with valid certificate verification (without `-k`).

## Step 2: Create a realm, client, and user

Log in to the Keycloak admin console (`https://keycloak.example.com:9444/` with the bootstrap admin credentials), or use the Admin REST API:

1. **Create a realm**: In the top-left dropdown, select **Create realm** and specify a name, such as `holon-test`. A realm is an isolated namespace for users and clients; in production, dedicated realms are recommended.
2. **Create a client**: Within the realm, go to **Clients → Create client**:
   - Keep the Client type as **OpenID Connect**; set Client ID to `holon-rp-test`.
   - Enable **Client authentication** (confidential client).
   - Check **Standard flow**; disable Direct access grants (optional, tightens the attack surface).
   - In **Valid redirect URIs**, enter Holon's callback address, e.g., `http://localhost:7880/api/auth/oidc/callback`.
3. **Record the client secret**: Navigate to Clients → `holon-rp-test` → **Credentials** tab and copy the Client secret.
4. **Create a user**: Go to **Users → Add user**, fill in the username and email (check Email verified). In the **Credentials** tab, click **Set password** and turn **Temporary off**—temporary passwords force a password change on first login, which halts automated or embedded authentication flows.

Reproducible Admin REST API configuration script (reads credentials from files, avoids echoing secrets):

```bash
BASE=https://keycloak.example.com:9444
TOKEN=$(curl -s "$BASE/realms/master/protocol/openid-connect/token" \
  -d grant_type=password -d client_id=admin-cli \
  -d username=admin -d password="$ADMIN_PASS" | jq -r .access_token)
curl -s -X POST "$BASE/admin/realms" -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"realm":"holon-test","enabled":true,"sslRequired":"external","registrationAllowed":false}'
curl -s -X POST "$BASE/admin/realms/holon-test/clients" -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"clientId":"holon-rp-test","enabled":true,"protocol":"openid-connect","publicClient":false,
       "standardFlowEnabled":true,"directAccessGrantsEnabled":false,
       "redirectUris":["http://localhost:7880/api/auth/oidc/callback"]}'
curl -s -X POST "$BASE/admin/realms/holon-test/users" -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"username":"alice","enabled":true,"email":"alice@example.com","emailVerified":true}'
# Set password: PUT /admin/realms/holon-test/users/<uuid>/reset-password with temporary:false in JSON body
```

## Step 3: Configure Holon

```bash
holon config set auth.oidc.issuer_url https://keycloak.example.com:9444/realms/holon-test
holon config set auth.oidc.client_id holon-rp-test
holon config set auth.oidc.client_secret_env KEYCLOAK_OIDC_CLIENT_SECRET
holon config set auth.oidc.redirect_uri http://localhost:7880/api/auth/oidc/callback
holon config set auth.mode oidc
```

Two common pitfalls:

- `issuer_url` must end exactly with **`/realms/<realm-name>`**, matching Keycloak's discovery `issuer` verbatim. Trailing slashes or mismatched casing will cause verification to fail.
- `KEYCLOAK_OIDC_CLIENT_SECRET` is the environment variable name. Inject the secret when launching Holon; do not write secrets into the configuration file:

```bash
KEYCLOAK_OIDC_CLIENT_SECRET="<Client Secret>" holon serve --port 7880
```

Known limitation (same as GitLab): Currently, `holon config set` validates the full authentication configuration upon each write. Therefore, setting `auth.mode oidc` before populating `auth.oidc.*` child keys will be rejected. Following the sequence above (sub-keys first, mode last) avoids this error. If mode was already configured, edit `config.json` directly and restart.

## Step 4: Verification

1. Navigate to the Holon login page. You should see "Continue with organization login". Clicking it redirects to Keycloak's realm login page (URL includes `/realms/<realm>/`).
2. Log in with the Keycloak user credentials. After returning to Holon, verify the active session:

```bash
curl -s http://localhost:7880/api/auth/session/me
# {"ok":true,"user_id":"oidc-…","display_name":"…","auth_method":"oidc"}
```

3. Logging out should cause the same endpoint to return 401:

```bash
curl -s -X POST http://localhost:7880/api/auth/session/logout
```

Note: Logging out of Holon terminates only the **local** session. Keycloak's browser SSO session remains active; initiating authorization from the same browser grants a new authorization code without prompting for password re-entry. This is expected behavior matching GitLab and Google Workspace. Although Keycloak advertises `end_session_endpoint` (the RP-initiated logout specification endpoint) in discovery, Holon currently does not invoke it. If your organization requires full cross-application logout, configure a shorter Keycloak SSO Session Idle timeout as a safeguard.

Negative checks:

- Invalid client secret: Token endpoint returns 401. Note that Keycloak returns `unauthorized_client` (GitLab and Google return `invalid_client`). While the rejection behavior is consistent, the error code differs.
- Unregistered redirect URI: The authorize endpoint directly returns a 400 error page without issuing an authorization code.
- Missing `openid` scope: The token response lacks an `id_token` (Keycloak issues a refresh token by default with `refresh_expires_in` defaulting to 1800 seconds).
- Tampered ID token signature: Local RS256 signature verification with JWKS public keys fails.

## Differences from GitLab CE and Google Workspace

| Dimension | Self-Hosted Keycloak | Self-Hosted GitLab CE | Google Workspace |
| --- | --- | --- | --- |
| External Dependencies | None (fully local) | None (fully local) | Requires Google Cloud project and workspace domain |
| IdP Role | Dedicated IAM platform with realm-level isolation | Built-in OAuth in DevOps platform | Cloud productivity suite identity |
| Access Control | Realm user management; LDAP/SAML federation | GitLab user/group management | OAuth consent screen User Type = Internal + `hd` domain (Holon does not enforce `hd`) |
| Single Logout | Discovery exposes `end_session_endpoint`, but Holon currently does not call it; logout ends local session only | Discovery lacks the endpoint; local session logout only | Not enforced by Holon |
| Refresh Token | Issued by default (`refresh_expires_in` 1800s) | Issued by default (2h, configurable) | Requires explicit `offline_access` (`access_type=offline`) |
| Mandatory Password Reset | Users with the Temporary password flag halt automated flows (must be disabled on creation) | No such mechanism | No such mechanism |

## Verified Scope

The following conclusions reflect testing on 2026-11-01 (official Keycloak 26.3.5 Docker image + isolated Holon instance with issuer `https://<tailscale host>:9444/realms/holon-test`):

- Verified: End-to-end programmatic authorization code flow (PKCE S256, state, nonce) between Keycloak and Holon with successful form login and session creation (`/api/auth/session/me` returns `auth_method=oidc`, 401 after logout); ID token RS256 signature verified against local JWKS keys with valid `iss`, `aud`, `exp`, `iat`, `nonce`, `sub`, and `at_hash` claims; `preferred_username` and `email` claims mapped correctly with matching `sub` in userinfo; refresh tokens issued and exchanged for new access tokens; invalid client secrets rejected (401 `unauthorized_client`); unregistered redirect URIs rejected (400); requests missing `openid` scope omit ID tokens; tampered signatures rejected by local verification; revoked access tokens return 401 on userinfo; Keycloak SSO session persists after Holon logout (silent re-authentication).
- Observed behavior (non-blocking finding): When an invalid OIDC callback occurs (such as state or browser session mismatch), Holon correctly rejects the request and does not establish a session, but returns a 500 `runtime_error` rather than a 4xx client error. Look for refined error semantics in future releases.
- Not verified: Manual end-to-end browser UI clicks (testing was conducted via automated HTTP flows), realm federation (LDAP/AD/SAML), user group claims (`groups` mapper), RP-initiated logout integration, external database configurations (default embedded H2 is intended for evaluation only), and production-scale backup/recovery.
