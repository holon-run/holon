---
title: Share AI assistants with your team
summary: Deploy Holon on a shared server, enable team sign-in, assign tasks to common assistants, and track progress together.
order: 185
---

# Share AI assistants with your team

Once you have Holon running on your local machine, the next natural step is
sharing an assistant with your team. Instead of having every teammate configure
their own local environment and copy prompts back and forth, you can run Holon
on a shared server as a dedicated, always-on AI teammate that remembers project
context, handles tasks across sessions, and delivers concise summaries when work
is done.

This guide outlines the three steps to set up team collaboration, followed by
best practices for day-to-day teamwork.

## Why share an assistant with your team?

Running an assistant for a team gives you three key advantages over isolated
desktop chat tools:

1. **Shared context and standing rules:** The assistant operates in your
   team's canonical repository or shared workspace. It reads the same
   `AGENTS.md` guidelines, accesses shared documentation, and produces
   consistent work without individual prompt drift.
2. **Delegated work with clear accountability:** Anyone on the team can
   assign a task with concrete acceptance criteria. Through OpenID Connect
   (OIDC), Holon records which teammate initiated each action.
3. **Unattended follow-up:** When an agent waits for slow CI builds, external
   downloads, or human review, teammates can close their laptops. The agent
   resumes when dependencies resolve and delivers a structured brief.

## Step 1: Deploy Holon on a shared server

Choose a machine accessible to your team—such as a cloud instance, an office
server, or a continuous workstation.

Install Holon and start the daemon with the network mode that matches your
infrastructure:

- **Cloudflare Named Tunnel (`--access cloudflare`):** Public HTTPS with a
  preconfigured dedicated tunnel, hostname and connector token.
- **Tailscale (`--access tailnet`):** Ideal for private mesh networks between
  team devices.
- **LAN or reverse proxy (`--access lan`):** Best for on-premise servers behind
  an existing Nginx, Caddy, or Kubernetes ingress.

```bash
# Preconfigure Named Tunnel ingress to http://127.0.0.1:8787 first.
holon daemon start --access cloudflare --port 8787 \
  --advertise https://holon.example.com \
  --cloudflare-token-file ~/.cloudflared/holon-connector.token \
  --token-file ~/.holon/remote.token
```

For detailed network configuration and service management, see
[Connect to a remote Holon runtime](./connect-remote-runtime.md).

## Step 2: Connect team single sign-on (OIDC)

Sharing a static control token among teammates makes auditing impossible. For
team environments, configure OpenID Connect (OIDC) so members log in using
your organization's existing identity provider (such as Google Workspace,
GitLab CE, Keycloak, Okta, Authentik, or Microsoft Entra ID).

With OIDC enabled:
- Teammates log in via browser using their work accounts.
- Holon attributes prompts and task creation to individual users.
- Access can be revoked instantly from your identity provider when team
  membership changes.

```bash
# Switch to OIDC authentication
holon config set auth.mode "oidc"
holon config set auth.oidc.issuer_url "https://auth.example.com/realms/team"
holon config set auth.oidc.client_id "holon"
holon config set auth.oidc.client_secret_env "HOLON_OIDC_CLIENT_SECRET"
```

For step-by-step IdP registration and session tuning, see
[Configure OIDC authentication](./configure-oidc-authentication.md) or the
dedicated provider walkthroughs for
[Google Workspace](./google-workspace-oidc-authentication.md),
[self-hosted GitLab CE](./gitlab-ce-oidc-authentication.md), and
[self-hosted Keycloak](./keycloak-oidc-authentication.md).

## Step 3: Collaborate through the Web GUI

Once deployed and authenticated, open your server's URL in a browser to access
the [Web GUI](./use-web-gui.md):

1. **Select an assistant:** Choose the long-lived agent dedicated to the
   project (for example, a release manager, docs steward, or code reviewer).
2. **Assign a task (WorkItem):** Create a tracked work item with clear scope
   and acceptance criteria (e.g., "Audit outdated dependencies and prepare PR").
3. **Follow progress and handover:** All teammates can inspect active work
   items, review logs, or add follow-up instructions without disrupting ongoing
   execution.
4. **Review completion briefs:** When work settles, the assistant publishes a
   concise result brief highlighting key outcomes and required human decisions.

To see how an actual engineering team uses this setup in practice, read
[How a small team works with long-lived AI assistants](/blog/agents-in-a-small-team).
