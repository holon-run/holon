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
GitLab CE, Keycloak, Casdoor, Okta, Authentik, or Microsoft Entra ID).

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
[self-hosted GitLab CE](./gitlab-ce-oidc-authentication.md),
[self-hosted Keycloak](./keycloak-oidc-authentication.md), and
[self-hosted Casdoor](./casdoor-oidc-authentication.md).

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

## Day-to-day teamwork

Deployment gets the assistant running. These three habits decide whether the
team actually shares it or quietly drifts back to individual chat windows.

### Put the acceptance criteria in the WorkItem

Anyone on the team can assign work. What makes it shareable is that the task
carries its own definition of done: the objective, the boundaries (which files
or systems are out of scope), and how the result will be judged.

```text
Upgrade dependency X. Keep the change to the manifest and lockfile.
Do not reformat unrelated code. Acceptance: the full check passes and the
CI workflow is green.
```

Instructions that exist only in a chat message disappear as soon as the
conversation scrolls. A tracked work item keeps its plan, checklist, and
acceptance criteria readable for whoever picks the work up next—including the
person who assigned it, a week later.

### Hand work off instead of restarting it

When the assistant stops and waits, read the recorded wait reason before
writing anything. It may need your decision, a background task result, or an
external event such as CI or a review. If it needs a decision, answering that
question continues the same work item; there is no need to create a new one or
re-explain the context.

A request sent while the assistant is still working is queued as a message for
it. You do not have to wait for it to finish before adding information, and the
in-flight turn is not discarded.

Keep the two levels of completion separate before treating anything as done:

- **A finished Task** means one command, build, or delegated subagent call
  returned a result. It is evidence.
- **A finished WorkItem** means the objective and its acceptance criteria are
  met. A green build is not the same as a reviewed change.

### Revoke access in the identity provider, keep the history

With OIDC enabled, each prompt records a stable `actor_id` for the authenticated
user, plus `actor_display_name` captured at send time. History therefore stays
self-contained: it keeps who sent a request even after that person is renamed,
and nothing needs to be rewritten when someone leaves.

When a teammate leaves the project, disable their account in your identity
provider. That is the step that ends their access.

OIDC answers "who asked for this". It does not partition files or tool output
between teammates: everyone operating the same runtime works against the same
workspaces, so use the identity provider for access and the work items for
accountability rather than expecting per-user isolation.

## See Also

- [Work items and tasks](../concepts/work-items-and-tasks.md) — durable
  objectives versus execution tasks
- [Observe agent work](./observe-agent-work.md) — follow progress, waits, and
  results in the Web GUI
- [Configure OIDC authentication](./configure-oidc-authentication.md) — IdP
  registration, claims, and session lifetime
- [Connect to a remote Holon runtime](./connect-remote-runtime.md) — network
  modes, verification, and troubleshooting
