---
title: Configure OIDC Single Sign-On with Self-Hosted GitLab CE
summary: Deploy a self-hosted GitLab CE instance, register an OAuth application, configure Holon's OIDC authentication, and verify end-to-end single sign-on without external accounts.
order: 186
---

# Configure OIDC Single Sign-On with Self-Hosted GitLab CE

Self-hosted GitLab Community Edition (GitLab CE, Free tier is sufficient) can serve as an OpenID Connect (OIDC) identity provider for Holon. Users, OAuth applications, and client credentials are fully managed within your own GitLab instance, with no dependency on external identity providers or third-party accounts. This guide provides a complete walkthrough from deploying an instance to end-to-end verification, reproducible on a single host.

For general OIDC concepts (issuer, client ID, redirect URI, session TTL, and prompt attribution), see [Configure OIDC authentication](./configure-oidc-authentication.md). This guide focuses specifically on self-hosted GitLab.

## Prerequisites

- A host capable of running Docker (a GitLab CE container requires approximately 4 GB RAM and 15–20 GB disk space).
- GitLab must be served over HTTPS. Holon's OIDC client only trusts the built-in Mozilla root certificates; **self-signed CAs are rejected**. For private networks, we recommend using Tailscale's `tailscale cert` to obtain public Let's Encrypt certificates without exposing ports to the public internet.
- An accessible Holon instance. The redirect URI supports HTTPS, or HTTP strictly with the hostname `localhost` (any port). IP literals like `127.0.0.1` or `::1` are rejected.

## Step 1: Deploy GitLab CE

Start GitLab using Docker, mount your SSL certificate into the container, and disable the built-in Let's Encrypt service:

```bash
mkdir -p gitlab/{config,logs,data}
docker run -d --name gitlab --shm-size 256m \
  -p 9443:443 \
  -v "$PWD/gitlab/config:/etc/gitlab" \
  -v "$PWD/gitlab/logs:/var/log/gitlab" \
  -v "$PWD/gitlab/data:/var/opt/gitlab" \
  -v "$PWD/certs/site.crt:/etc/gitlab/ssl/site.crt:ro" \
  -v "$PWD/certs/site.key:/etc/gitlab/ssl/site.key:ro" \
  -e GITLAB_ROOT_PASSWORD="$(openssl rand -hex 16)" \
  -e GITLAB_OMNIBUS_CONFIG="external_url 'https://gitlab.example.com:9443'; nginx['listen_port'] = 443; nginx['listen_https'] = true; nginx['ssl_certificate'] = '/etc/gitlab/ssl/site.crt'; nginx['ssl_certificate_key'] = '/etc/gitlab/ssl/site.key'; letsencrypt['enable'] = false; prometheus_monitoring['enable'] = false" \
  gitlab/gitlab-ce:latest
```

The first startup takes several minutes to complete initialization. Readiness check: `curl https://gitlab.example.com:9443/.well-known/openid-configuration` returns HTTP 200, and the `issuer` field matches the site URL (including the port).

Note: When `external_url` uses a non-443 port, the SSL certificate is still a standard domain certificate (certificate validation ignores port numbers), and the discovery document and issuer will automatically include the port number.

## Step 2: Create an OAuth Application

Create a confidential OAuth application in GitLab:

1. User avatar → **Edit profile** → **Applications** → **New application** (instance-level applications are created in the Admin Area).
2. Set any **Name** (e.g. `holon`); set **Redirect URI** to Holon's callback address, e.g. `http://localhost:7879/api/auth/oidc/callback`.
3. Check scopes: **openid**, **profile**, and **email**.
4. Keep **Confidential** enabled (GitLab 18+ enforces confidential clients at the database layer).
5. Save and record the **Application ID** and **Secret**.

Scope details: `openid` is required for issuing an `id_token` (without it, no `id_token` is returned); `email` and `profile` provide the user's email and display name for Holon. If you need user group claims in the `id_token`, register and request the `groups` scope separately; this walkthrough leaves it disabled by default.

## Step 3: Create Users and Confirm Email

Administrators create users in the Admin Area or allow self-registration. **A common pitfall**: GitLab requires users to confirm their email before completing OAuth authorization. Users with unconfirmed emails can sign in, but the authorization screen will fail with "Verify the email address in your account". In test environments, administrators can manually confirm email addresses or relax confirmation requirements in instance settings.

## Step 4: Configure Holon

```bash
holon config set auth.oidc.issuer_url https://gitlab.example.com:9443
holon config set auth.oidc.client_id <Application ID>
holon config set auth.oidc.client_secret_env GITLAB_OIDC_CLIENT_SECRET
holon config set auth.oidc.redirect_uri http://localhost:7879/api/auth/oidc/callback
holon config set auth.mode oidc
```

`GITLAB_OIDC_CLIENT_SECRET` is the environment variable name. Inject the secret when launching Holon, avoiding writing secrets into configuration files:

```bash
GITLAB_OIDC_CLIENT_SECRET="<Application Secret>" holon serve --port 7879
```

Known limitation: Current versions of `holon config set` validate the entire authentication configuration on each write. Consequently, **setting `auth.mode oidc` before configuring the `auth.oidc.*` sub-keys will be rejected** (it enters an intermediate state that is neither valid oidc nor local). Setting sub-keys first and mode last avoids this issue. If `auth.mode` was already set, edit `config.json` directly and restart the daemon.

## Step 5: Verify the Setup

1. Open the Holon sign-in page. It should display "Continue with organization login". Click it to redirect to GitLab sign-in.
2. Sign in to GitLab and authorize the application. Once redirected back to Holon, check the session:

```bash
curl -s http://localhost:7879/api/auth/session/me
# {"ok":true,"user_id":"oidc-…","display_name":"…","auth_method":"oidc"}
```

3. After logging out, the same endpoint should return HTTP 401:

```bash
curl -s -X POST http://localhost:7879/api/auth/session/logout
```

Negative checks: An incorrect `client_secret` causes the token endpoint to return HTTP 401 `invalid_client`; an unregistered `redirect_uri` fails to receive an authorization code; omitting the `openid` scope results in no `id_token`.

## Differences from Google Workspace

| Dimension | Self-Hosted GitLab CE | Google Workspace |
| --- | --- | --- |
| External Dependencies | None (entirely self-contained) | Requires Google Cloud project and organization |
| Organization Access | GitLab user/group management (uninvited users do not exist) | OAuth consent screen User type = Internal + `hd` domain (Holon does not validate `hd`) |
| Single Sign-Out | Discovery lacks `end_session_endpoint`; Holon sign-out only terminates the local session | Similarly not enforced by Holon |
| User Group Claims | Requires registering and requesting `groups` scope | Not applicable |

## Scope of Verification in This Guide

The following conclusions are drawn from testing on 2026-11-01 (official GitLab CE Docker image + isolated Holon instance, issuer `https://<tailscale-hostname>:9443`):

- Verified: Programmatic code flow with PKCE S256, state, and nonce completes authentication and establishes sessions between GitLab and the isolated Holon instance; `id_token` RS256 signature verified via locally reconstructed JWKS public keys, with compliant `iss`, `aud`, `exp`, `iat`, `nonce`, and `sub`, and matching `sub` in userinfo; incorrect `client_secret` rejected (401 `invalid_client`), unregistered `redirect_uri` cannot obtain an authorization code, missing `openid` scope yields no `id_token`; `/api/auth/session/me` and logout behaviors work as expected.
- Not verified: Complete manual browser UI workflow (verification relied on programmatic HTTP flows), LDAP/enterprise user directories, group synchronization with `groups` scope enabled, GitLab.com SaaS, production-scale deployment and backup/restore.
