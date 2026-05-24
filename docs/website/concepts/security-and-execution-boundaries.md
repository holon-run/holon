---
title: Security and execution boundaries
summary: "What Holon does and does not sandbox: local execution, workspace binding, remote access, capability secrets, and trust metadata."
order: 30
---

# Security and execution boundaries

Holon runs agents that can execute shell commands, delegate work, and wait for
external events. Understanding what boundaries Holon provides — and what it
does not — helps you run agents safely.

## The host-local runtime

Holon runs as a user-level process on your machine. It is **not a sandbox**:

- Agents can run any command your user account can run.
- File operations use your user permissions.
- Network access follows your host's network configuration.
- There is no container, VM, or seccomp isolation built into the runtime.

**What this means:** scope workspaces intentionally, use dedicated user
accounts or containers when running untrusted agent workloads, and treat agent
command execution with the same care you'd give a shell script running as your
user.

## Execution environment summary

Every agent receives an execution environment summary in context. This summary
describes the current policy snapshot and what is enforced:

| Boundary | Level | What it means |
|----------|-------|---------------|
| `cwd_rooting` | hard_enforced | Commands cannot escape the workspace root as their working directory |
| `projection_rooting` | hard_enforced | Filesystem views are constrained to the projected root |
| `path_confinement` | not_enforced | The runtime does not prevent access outside the workspace by path |
| `write_confinement` | not_enforced | Write operations are not restricted to the workspace |
| `network_confinement` | not_enforced | Network access is unrestricted by default |
| `secret_isolation` | not_enforced | The runtime does not isolate secrets from agent commands |

**Key takeaway:** boundaries marked `hard_enforced` are runtime guarantees.
Boundaries marked `not_enforced` rely on the operator's host configuration
(filesystem permissions, firewall rules, container isolation).

## Workspace binding

Holon provides workspace binding that constrains where the agent operates:

- **Workspace root** — the agent's default working directory. The runtime
  enforces that commands start inside this root, but the agent or its commands
  can navigate elsewhere by path.
- **Projection root** — constrains the filesystem view presented to the agent.
  When enforced, the agent only sees files under the projection root.
- **ApplyPatch targets** — file mutation tools resolve relative paths against
  the active workspace by default.

These bindings are **organizational guardrails**, not security boundaries:
an agent that can execute arbitrary shell commands can `cd /etc` or read files
outside the workspace if your OS permissions allow it. For true confinement,
pair Holon with a container, VM, or dedicated user account.

## Remote access (`holon serve`)

`holon serve` exposes the Holon control plane over HTTP. This is a powerful
surface and must be protected:

```bash
# Secure: local-only access on Unix socket (default daemon mode) — no
# additional protection needed beyond filesystem permissions
holon daemon start

# Caution: LAN-accessible — always require a token
holon serve --access lan --token "your-secret-token"
holon serve --access lan --token-file ~/.holon/remote.token

# Caution: tunnel access — the tunnel provider can route traffic to
# your Holon instance
holon serve --access tunnel --token "your-secret-token"
```

**Rules for remote access:**

- Always use `--token` or `--token-file` when exposing Holon beyond localhost.
- Prefer `--access local` or the default daemon Unix socket unless you have a
  specific integration need.
- Treat the token as a credential: rotate it, don't commit it, and don't share
  it through unencrypted channels.
- Tunnel mode exposes Holon through a tunnel provider; understand the provider's
  security model before using it.

## Capability-secret URLs

Holon generates callback URLs for external triggers and wake events. These URLs
contain capability secrets:

```
http://host:7878/callbacks/wake/cb_<secret>
```

Anyone who knows this URL can wake the agent. Treat these URLs as secrets:

- Do not log them, commit them, or share them in public channels.
- Rotate them when rotating other credentials.
- Use HTTPS and token-protected serve for production deployments.

## Trust and provenance

Holon classifies every input by its **origin** and **trust level**:

| Trust level | Example sources | What it means |
|-------------|----------------|---------------|
| `trusted-operator` | CLI, TUI, authenticated HTTP | You initiated this input |
| `trusted-system` | Internal runtime events, scheduled timers | The runtime generated this |
| `trusted-integration` | Authenticated webhook from a known service | A trusted external system sent this |
| `untrusted-external` | Public webhooks, user-submitted content | The source is unknown or unverified |

**Trust metadata is a policy signal, not a security guarantee.** The agent
sees the trust level and can adjust its behavior (e.g., refusing destructive
commands on untrusted input), but trust labels do not replace sandboxing.
External content marked `untrusted-external` still runs with your user's
privileges unless you add OS-level confinement.

## Practical recommendations

1. **Use dedicated workspaces** — give agents a specific project directory
   rather than your home directory.
2. **Protect remote access** — always use tokens, prefer local Unix sockets.
3. **Guard capability URLs** — treat wake/callback URLs as credentials.
4. **Don't rely on trust labels alone** — they inform agent behavior, not OS
   security.
5. **Add OS-level isolation for high-risk work** — containers, VMs, or
   dedicated user accounts.

## See also

- [Trust boundaries](/concepts/trust-boundaries) — how trust classification works end-to-end
- [Runtime model](/concepts/runtime-model) — execution environment and workspace binding
- [Integration guide](/guides/integration) — HTTP control plane access
- [Remote access](/guides/remote-access) — serving Holon beyond localhost
