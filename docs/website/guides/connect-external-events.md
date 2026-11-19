---
title: "Connect external events: Webhooks from GitHub to incident alerts"
summary: Configure external Webhook callbacks in natural dialogue, suspend tasks with zero token consumption, and resume securely on external events.
order: 100
---

# Connect external events: Webhooks from GitHub to incident alerts

In continuous integration (CI), code reviews, production alerts, and incident responses, keeping an agent continuously polling an external API burns tokens and wastes host system resources.

In Holon, you can instruct your agent in natural dialogue to provision and manage external Webhook callbacks. The agent issues a secure wake callback URL, parks long-running tasks in a **zero-token suspended state**, and wakes up instantly when GitHub Actions finishes, GitLab pipelines change, or incident bots (Slack, Teams, PagerDuty, Alertmanager) trigger an HTTP notification. When resumed, the agent enforces strict trust boundaries by actively verifying ground-truth state before completing work.

## Prerequisites

- A running Holon runtime and [Web GUI](./use-web-gui.md) (default at `http://localhost:8080`) or CLI session.
- An active long-lived agent (such as `builder` or `main`).
- Network reachability from the external sender (GitHub, CI server, or monitoring service) to the Holon HTTP port (use Cloudflare Tunnel or Tailscale for local development).
- No raw CLI script assembling or manual JSON payload construction required: everything is driven via natural dialogue with the agent.

---

## Practical Steps

### 1. Understand the Wake-Hint mechanism and trust boundaries

Holon's event-driven architecture is built on two core security principles:

1. **Wake-Hint Delivery**: An incoming Webhook POST request serves purely as a **wake signal** to the scheduler. During the wait, the agent process is completely parked: no model inference runs, and zero tokens are consumed.
2. **Strict Trust Boundaries**: External Webhooks originate from **untrusted input sources**. Anyone on the internet with network access could forge a request. Therefore, in Holon, **a Webhook wake never equals operator authorization or proof of task completion**. When awakened, the agent must use its own verified, read-only tools (such as querying GitHub's API for the recorded Commit SHA and check-run status) to inspect external reality before proceeding.

---

### 2. Request a Webhook callback URL in conversation

In your Web GUI or conversational session, simply ask the agent for its integration callback endpoint:

**Example operator prompt**:
> "I want to notify you when our GitHub Actions CI build finishes. Please provide an external Webhook callback URL for this agent and explain how to configure it in the GitHub repository."

**Agent response & instructions**:
The agent inspects its runtime ingress capability context and outputs:

> "I have prepared a dedicated external wake Webhook endpoint for you:  
> - **Webhook URL**: `https://your-holon-host.example.com/api/callbacks/wake/cb_xxxxxx...`  
> - **HTTP Method**: `POST`  
> - **Content-Type**: `application/json` (or empty body)  
> 
> **GitHub Configuration Steps**:  
> 1. Go to your GitHub repository -> **Settings** -> **Webhooks** -> **Add webhook**;  
> 2. Paste the URL above into **Payload URL**;  
> 3. Select `application/json` as the Content type;  
> 4. Under 'Which events would you like to trigger this webhook?', check **Workflow runs**."

> **Security Note**: This URL contains a capability secret. Anyone with access to the URL can wake the agent. Never commit it to public repositories or paste it into shared chat rooms.

---

### 3. Instruct the agent to start the task and park for completion

Once the Webhook is configured, describe the goal to your agent in natural language. The agent records the tracked objective into a WorkItem and registers an external wait:

**Example operator prompt**:
> "We pushed a major refactoring commit to PR #42. Trigger CI, park your execution, and wake up when CI finishes. If the build fails, extract the failed test logs; if it succeeds, report a verified completion summary."

**Agent execution sequence**:
1. The agent creates a dedicated WorkItem to track PR #42 verification;
2. It registers an external wait on `github:owner/repo#run/42` with an automatic 30-minute fallback deadline;
3. The agent yields the turn with a brief waiting summary and enters a **suspended state**, waiting for the incoming Webhook.

---

### 4. Send a test wake and verify end-to-end resumption

Before relying on real CI runs, you can send a dry-run test wake from a remote host or local terminal:

**Simulate a test wake**:

```bash
curl -X POST "https://your-holon-host.example.com/api/callbacks/wake/cb_xxxxxx..."
```

**Observe the agent's behavior**:
1. The Holon endpoint returns `200 OK` immediately;
2. The scheduler wakes the parked WorkItem;
3. The agent resumes execution and immediately queries GitHub API to check the actual state of PR #42;
4. If the build is still running (e.g., in a premature test wake), the agent recognizes the in-progress state and re-registers the wait;
5. When the build has actually finished, the agent parses the logs and delivers a clean completion brief in the Web GUI.

---

### 5. Common Integration Patterns

| External Service | Trigger Scenario | Recommended Webhook Configuration |
|---|---|---|
| **GitHub Actions** | CI build completion for PR reviews and merges | Repository Webhook subscribing to `Workflow runs`. The agent verifies Commit SHA and check-runs upon wake. |
| **GitLab CI/CD** | Pipeline success or failure notifications | Project Webhook subscribing to `Pipeline events`. |
| **Monitoring Alerts (Alertmanager / Sentry)** | Incident alerts waking an on-call diagnostic agent | Webhook receiver configured to wake the agent, which inspects cluster metrics and logs. |
| **Team Chatbots (Slack / Teams / Lark)** | Operator or team members pinging an agent bot | Bot webhook events wake the agent to create and resolve tracked WorkItems. |

---

## Production Security and Capability Lifecycle

### 1. Capability Rotation
If a Webhook URL is accidentally committed to a repository or exposed in application logs, rotate it immediately in conversation:

**Example operator prompt**:
> "Rotate our external Webhook callback credentials immediately and give me the new URL."

The agent triggers the runtime reset endpoint, revoking the compromised URL. The old endpoint instantly returns 404/401, neutralizing any unauthorized wake attempts.

### 2. Automatic Cleanup on Agent Deletion
When an ephemeral subagent completes its project and is cleaned up, Holon automatically deletes all registered external triggers and revokes associated callback capabilities, preventing dangling endpoints.

---

## Troubleshooting

### The external service sent a Webhook, but the agent did not resume
- **Verify network ingress**: If Holon runs on `127.0.0.1`, public cloud services cannot reach it without an ingress proxy or tunnel (such as Cloudflare Tunnel).
- **Check agent execution queue**: If the agent is actively executing a foreground command, the wake event is queued and will resume the task as soon as the current turn yields.
- **Verify credential freshness**: If the callback was rotated recently, ensure the sender updated its payload URL.

### Why doesn't the agent trust the payload content directly?
- This is an intentional security design. Payloads can be spoofed or manipulated. Holon treats webhooks strictly as wake signals; the agent must always inspect authoritative state via trusted tools to prevent prompt injection and unauthorized execution.

---

## Related links

- [Events, Waits, and Resumption](/concepts/events-and-waits.md) — Event-driven kernel architecture
- [Unify external notifications and async events with Agent Inbox](connect-event-inbox.md) — Shared multi-source subscriptions, filtering, and batch triage
- [Schedule Periodic Agent Work](schedule-agent-work.md) — Cron-like timers and recurring patrols
- [Wake Agents on Web Page Diffs with changedetection.io](changedetection-webhook-setup.md) — Monitoring web pages with Webhook wakeups
- [Trust Boundaries and Authorization](/concepts/trust-boundaries.md) — Safe handling of untrusted inputs
