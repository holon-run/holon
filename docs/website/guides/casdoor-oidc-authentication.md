---
title: Configure OIDC Single Sign-On with Self-Hosted Casdoor
summary: Deploy a self-hosted Casdoor instance with Docker, initialize organizations and applications, configure Holon's OIDC authentication, and verify end-to-end single sign-on without external accounts.
order: 188
---

# Configure OIDC Single Sign-On with Self-Hosted Casdoor

Casdoor is an open-source centralized Identity and Access Management (IAM) platform (Apache-2.0) built by the Casbin community. It features a built-in administrative web UI and natively supports OAuth 2.0, OIDC, SAML, LDAP, SCIM, and more. When used as Holon's OIDC identity provider, organizations, users, applications, and session policies are managed entirely within your own Casdoor deployment—no registration with external third-party providers is needed. Compared to Keycloak, Casdoor offers a lighter footprint (a single container plus a database) and starting from v4.26.0 supports organization-level path-based discovery issuers. This guide walks through the complete process from deployment to end-to-end verification, reproducible on a single host.

For general OIDC concepts (issuer, client ID, redirect URI, session TTL, and prompt attribution), see [Configure OIDC authentication](./configure-oidc-authentication.md). This guide focuses specifically on self-hosted Casdoor.

## Prerequisites

- A host capable of running Docker; Casdoor officially recommends MySQL 8 as its persistence backend.
- An HTTPS domain pointing to this host (this guide uses `https://casdoor.example.com:9445` as an example; a Tailscale domain with `tailscale cert` or a public domain with Let's Encrypt works equally well).
- Any recent version of Holon (with support for `auth.oidc.*` configuration).
- Casdoor **v4.26.0 or higher** (verified with v4.26.0 in November 2026; organization-level path issuers require v4.26.0+).

## Step 1: Deploy Casdoor

Key concept: Casdoor listens on port `8000` (HTTP) inside its container. The `origin` environment variable must match the exact external base URL accessed by browsers, or the `issuer` claim in the discovery document and authorization redirect URLs will point to incorrect endpoints. Terminating TLS at a reverse proxy (in this guide, an nginx sidecar with TLS certificates) is recommended, while Casdoor itself stays on plain HTTP.

Start the three containers in order: MySQL → Casdoor → nginx (nginx must start last so it can resolve the upstream hostname):

```bash
docker network create casdoor-net
docker run -d --name casdoor-mysql --network casdoor-net \
  -e MYSQL_ROOT_PASSWORD="<database-root-password>" \
  -v "$PWD/mysql-data:/var/lib/mysql" \
  mysql:8.0

# Wait for MySQL readiness: you must use a TCP probe (-h 127.0.0.1).
# During initial bootstrapping, the MySQL entrypoint temporarily runs
# a socket-only instance. Omitting -h 127.0.0.1 returns success prematurely,
# causing Casdoor's immediate database connection to fail.
for i in $(seq 1 60); do
  docker exec casdoor-mysql mysqladmin ping -h 127.0.0.1 -uroot -p"<database-root-password>" --silent && break
  sleep 2
done

# Once the persistent data directory is initialized, MYSQL_DATABASE is not re-executed;
# therefore, create the database idempotently:
docker exec casdoor-mysql mysql -uroot -p"<database-root-password>" \
  -e "CREATE DATABASE IF NOT EXISTS casdoor CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci"

docker run -d --name casdoor --network casdoor-net \
  -e driverName=mysql \
  -e "dataSourceName=root:<database-root-password>@tcp(casdoor-mysql:3306)/" \
  -e origin="https://casdoor.example.com:9445" \
  -v "$PWD/init_data.json:/init_data.json:ro" \
  casbin/casdoor:v4.26.0
```

nginx TLS sidecar configuration (replace certificate paths with your actual domain certs):

```nginx
server {
    listen 9445 ssl;
    server_name casdoor.example.com;
    ssl_certificate     /etc/nginx/certs/tls.crt;
    ssl_certificate_key /etc/nginx/certs/tls.key;
    location / {
        proxy_pass http://casdoor:8000;
        proxy_set_header Host $host:9445;
        proxy_set_header X-Forwarded-Proto https;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    }
}
```

```bash
docker run -d --name casdoor-nginx --network casdoor-net \
  -p 9445:9445 \
  -v "$PWD/tls.crt:/etc/nginx/certs/tls.crt:ro" \
  -v "$PWD/tls.key:/etc/nginx/certs/tls.key:ro" \
  -v "$PWD/nginx-casdoor.conf:/etc/nginx/conf.d/default.conf:ro" \
  nginx:alpine
```

Verify the deployment:

```bash
curl -s https://casdoor.example.com:9445/.well-known/openid-configuration | jq -r .issuer
# https://casdoor.example.com:9445
```

The administration console is available at `https://casdoor.example.com:9445/`, with default username `admin` (change the password immediately upon first login).

## Step 2: Initialize Organization, Application, and Users

Choose either approach:

**Option A: Manual creation in the Admin Console.** Create an organization, users within that organization, and an application in order. Under the application, configure Redirect URLs, grant types (Authorization code + Refresh token), and token format (JWT). Note down the application's Client ID and Client Secret.

**Option B: Declarative initialization via `init_data.json` (recommended for reproducible setups).** On startup, Casdoor reads configuration mounted to `/init_data.json`, creating the organization, users, and application in a single step. Below is a minimal reproducible example (replace client secrets and user passwords with your own values):

```json
{
  "organizations": [
    {
      "owner": "admin",
      "name": "holon-org",
      "displayName": "Holon Org",
      "passwordType": "bcrypt",
      "defaultTokenFormat": "JWT",
      "useOrganizationIssuer": false
    }
  ],
  "users": [
    {
      "owner": "holon-org",
      "name": "alice",
      "type": "normal-user",
      "password": "<user-password>",
      "displayName": "Alice",
      "email": "alice@example.test",
      "emailVerified": true
    }
  ],
  "applications": [
    {
      "owner": "admin",
      "name": "holon-app",
      "displayName": "Holon RP",
      "organization": "holon-org",
      "clientId": "holon-rp-<your-suffix>",
      "clientSecret": "<client-secret>",
      "signinMethods": [{ "name": "Password", "displayName": "Password", "rule": "All" }],
      "grantTypes": ["authorization_code", "refresh_token"],
      "redirectUris": ["http://localhost:7880/api/auth/oidc/callback"],
      "tokenFormat": "JWT",
      "expireInHours": 168
    }
  ]
}
```

**Organization-level issuer (v4.26.0+ optional):** Setting `useOrganizationIssuer` to `true` on an organization changes the discovery and issuer URL for that organization's applications to a path-based format: `https://casdoor.example.com:9445/orgs/<org-name>`. Holon supports this path-based issuer directly with no special configuration (see Step 3).

## Step 3: Configure Holon

Global issuer (default configuration from Step 1):

```bash
holon config set auth.oidc.issuer_url https://casdoor.example.com:9445
holon config set auth.oidc.client_id holon-rp-<your-suffix>
holon config set auth.oidc.client_secret_env CASDOOR_OIDC_CLIENT_SECRET
holon config set auth.oidc.redirect_uri http://localhost:7880/api/auth/oidc/callback
holon config set auth.mode oidc
```

Organization-level path issuer (optional): only `issuer_url` differs, pointing directly to the organization discovery root:

```bash
holon config set auth.oidc.issuer_url https://casdoor.example.com:9445/orgs/holon-org
```

Three common pitfalls:

- `issuer_url` must match the `issuer` returned by discovery **verbatim, character-for-character** (including ports, and for organization-level issuers, the `/orgs/<org-name>` prefix).
- `redirect_uri` must appear verbatim in the application's Redirect URLs whitelist in Casdoor: Casdoor expects `http://localhost:7880/api/auth/oidc/callback`, exactly matching Holon's configuration (localhost and loopback IP hosts are permitted with non-80 ports).
- `client_secret_env` specifies the **environment variable name**. The secret value is injected when launching Holon, rather than stored in the configuration file:

```bash
CASDOOR_OIDC_CLIENT_SECRET="<client-secret>" holon serve --port 7880
```

The configuration write order is identical to Keycloak: set the `auth.oidc.*` child keys first, and set `auth.mode oidc` last (currently `holon config set` validates the complete authentication schema on every write, so reversing this order causes validation to fail).

## Step 4: Verification

1. Open the Holon login page. You should see "Continue with organization login". Clicking it redirects to Casdoor's login page (at your Casdoor domain).
2. Log in with user credentials from the organization. After returning to Holon, inspect the active session:

```bash
curl -s http://localhost:7880/api/auth/session/me
# {"ok":true,"user_id":"oidc-…","display_name":"…","auth_method":"oidc"}
```

3. Logging out should cause the same endpoint to return 401.

**Casdoor-specific secret semantics (check here first during troubleshooting):** Casdoor's token endpoint evaluates Client Secret under a "validate if provided" rule: Holon operates as a confidential client and submits `client_id + client_secret` via form-encoded post, where an incorrect secret returns `401 invalid_client`. However, if the application's Client Secret is left blank in Casdoor, Casdoor allows PKCE requests through as a public client. Normal setups (setting an application secret in Casdoor + configuring `client_secret_env` in Holon) are unaffected, but mixing an empty secret on Casdoor with an incorrect secret in Holon creates confusing edge cases. Always set explicit secrets.

## Differences Across GitLab CE, Keycloak, and Google Workspace

| Dimension | Self-Hosted Casdoor | Self-Hosted Keycloak | Self-Hosted GitLab CE | Google Workspace |
| --- | --- | --- | --- | --- |
| External Dependencies | MySQL 8 (recommended for persistence) | None (embedded H2 suitable for evaluation) | None (packaged embedded DB) | Requires Google Cloud project and workspace domain |
| IdP Role | Centralized IAM, multi-protocol (OIDC/SAML/LDAP/SCIM) | Dedicated identity platform, realm-level isolation | Built-in OAuth in DevOps platform | Cloud productivity suite identity |
| Access Control | Applications bound to organizations, users managed per organization | Realm user management; identity federation | GitLab user/group management | OAuth consent screen User Type = Internal + `hd` domain |
| Issuer Format | Global, or path-based `/orgs/<org-name>` (v4.26.0+) | Realm path `/realms/<realm>` | Root origin URL | Fixed `accounts.google.com` |
| Single Logout | Holon logout terminates local session only | Same (discovery advertises `end_session_endpoint` but Holon does not call it) | Same | Same |
| Refresh Token | Issued when application grantTypes includes `refresh_token`; validity `expireInHours` configurable (default 168h) | Issued by default (1800s) | Issued by default (2h, configurable) | Requires explicit `offline_access` |
| Secret Semantics | "Validate if provided"; blank secret + PKCE treated as public client | Standard confidential client | Standard OAuth application | Standard OAuth client |

## Verified Scope of This Guide

The following conclusions reflect testing conducted on 2026-11-08 (official Casdoor v4.26.0 Docker image + MySQL 8.0.46 + nginx TLS sidecar + isolated Holon instance, testing both global and organization-level path issuers):

- Verified: End-to-end programmatic authorization code flow (PKCE S256, state, nonce) between Casdoor and an isolated Holon instance completed genuine login and established sessions (login requests matched Casdoor React frontend API calls; `/api/auth/session/me` returned `auth_method=oidc`, 401 after logout); both global issuer and organization-level path issuer (`/orgs/<org-name>`) passed completely, with Holon accepting path-based discovery without issuer mismatch; negative test cases—unregistered redirect URI rejected without code grant, invalid client_id rejected, invalid client_secret returning 401 `invalid_client`, credential revocation taking effect immediately upon secret rotation, and disabled users prevented from creating sessions—all rejected as expected.
- Observed behavior (non-blocking finding): When an invalid OIDC callback occurs (such as state or browser session mismatch), Holon correctly rejects the request and does not establish a session, but returns a 500 `runtime_error` rather than a 4xx client error (holon-run/holon#3882), reproducible across GitLab, Keycloak, and Casdoor; this does not affect functional correctness.
- Not verified: Manual browser UI click workflows (verification used programmatic HTTP flows matching frontend requests), Casdoor versions prior to v4.26.0, SAML/LDAP/SCIM and external federation, JWKS key difference comparisons between global and organization path issuers, and production-scale deployment or backup/recovery procedures.
