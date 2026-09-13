---
title: Integration guide
summary: Programmatic access to Holon's HTTP control plane with curl examples and endpoint reference.
order: 25
---

# Integration Guide

Holon exposes an HTTP control plane for programmatic access. Start the server with `holon serve` and interact with agents, tasks, and work items through a REST-style API. Every route is served under the `/api` prefix.

## Starting the Server

```bash
# Local-only access
holon serve --port 8787

# With token-based authentication
holon serve --port 8787 --token "your-secret-token"
```

Access modes: `local`, `tunnel`, `lan`, `tailnet`. The default listen address is `127.0.0.1:7878`. A non-loopback listen address, or `lan`/`tailnet` access, requires a token.

## API Conventions

- **Base URL:** `http://localhost:8787`
- **Route prefix:** `/api`
- **Content-Type:** `application/json`
- **Authentication:** Bearer token in the `Authorization` header (when `--token` is set)

## Core Endpoints

### Agent Management

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/agents/list` | List active agent entries with metadata |
| `POST` | `/api/control/agents/:agent_id/create` | Create a new agent |
| `GET` | `/api/agents/:agent_id/status` | Get agent status and lifecycle |
| `GET` | `/api/agents/:agent_id/state` | Get lightweight agent state bootstrap |

### Messaging

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/api/agents/:agent_id/enqueue` | Enqueue a message into an agent |
| `POST` | `/api/control/agents/:agent_id/prompt` | Send an operator prompt |
| `POST` | `/api/control/agents/:agent_id/wake` | Wake a sleeping agent |
| `POST` | `/api/control/agents/:agent_id/control` | Send a control instruction |

### Tasks & Work Items

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/api/control/agents/:agent_id/tasks` | Create a command task |
| `POST` | `/api/control/agents/:agent_id/work-items` | Create a work item |
| `POST` | `/api/control/agents/:agent_id/work-items/:work_item_id/pick` | Pick the current work item |
| `PATCH` | `/api/control/agents/:agent_id/work-items/:work_item_id` | Update a work item |
| `POST` | `/api/control/agents/:agent_id/work-items/:work_item_id/complete` | Complete a work item |
| `GET` | `/api/agents/:agent_id/tasks` | List agent tasks |
| `GET` | `/api/agents/:agent_id/briefs` | Get recent briefs/context |
| `GET` | `/api/agents/:agent_id/transcript` | Get agent transcript |
| `GET` | `/api/agents/:agent_id/events` | Get agent event stream |

### Workspace & Skills

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/api/control/agents/:agent_id/workspace/attach` | Attach a workspace |
| `POST` | `/api/control/agents/:agent_id/workspace/detach` | Detach workspace |
| `GET` | `/api/agents/:agent_id/skills` | List agent skills |
| `POST` | `/api/control/agents/:agent_id/skills/enable` | Enable a skill for an agent |
| `POST` | `/api/control/agents/:agent_id/skills/disable` | Disable a skill for an agent |
| `GET` | `/api/skills/catalog` | List Skill Library catalog |
| `POST` | `/api/skills/catalog/add` | Add a skill to the library |
| `POST` | `/api/skills/catalog/remove` | Remove a skill from the library |
| `POST` | `/api/skills/catalog/reconcile` | Reconcile library with lock file |

### Callbacks & Webhooks

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/api/callbacks/enqueue/:callback_token` | External callback with payload |
| `POST` | `/api/callbacks/wake/:callback_token` | External wake trigger |
| `POST` | `/api/webhooks/generic/:agent_id` | Generic webhook ingress |

### Runtime Control

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/control/runtime/status` | Runtime health status |
| `POST` | `/api/control/runtime/shutdown` | Graceful shutdown |

## Examples

### Send a message to an agent

```bash
curl -X POST http://localhost:8787/api/agents/my-agent/enqueue \
  -H "Content-Type: application/json" \
  -d '{
    "text": "Review the latest changes in src/",
    "priority": "normal",
    "origin": {
      "kind": "webhook",
      "source": "my-service"
    }
  }'
```

The public enqueue route accepts only `channel` or `webhook` origins, and rejects `interject` priority. To send a message that carries operator trust, register an operator transport binding instead (see below).

Response:
```json
{
  "ok": true,
  "agent_id": "my-agent",
  "message_id": "msg_abc123"
}
```

### Create an agent

```bash
curl -X POST http://localhost:8787/api/control/agents/reviewer/create \
  -H "Content-Type: application/json" \
  -d '{"template": null}'
```

### Check agent status

```bash
curl http://localhost:8787/api/agents/my-agent/status
```

### Create a work item

```bash
curl -X POST http://localhost:8787/api/control/agents/my-agent/work-items \
  -H "Content-Type: application/json" \
  -d '{"objective": "Review and fix all clippy warnings"}'
```

### Update and complete a work item

```bash
curl -X PATCH http://localhost:8787/api/control/agents/my-agent/work-items/work_123 \
  -H "Content-Type: application/json" \
  -d '{
    "plan_status": "ready",
    "todo_list": [
      { "text": "Run cargo check", "state": "completed" }
    ],
    "blocked_by": "waiting for CI",
    "recheck_after": 600000
  }'

curl -X POST http://localhost:8787/api/control/agents/my-agent/work-items/work_123/complete \
  -H "Content-Type: application/json" \
  -d '{"report_text": "Build fixed and all checks passed."}'
```

### Create and cancel a timer

```bash
curl -X POST http://localhost:8787/api/control/agents/my-agent/timers \
  -H "Content-Type: application/json" \
  -d '{"duration_ms": 60000, "summary": "reminder"}'

curl -X POST http://localhost:8787/api/control/agents/my-agent/timers/timer_123/cancel \
  -H "Content-Type: application/json" \
  -d '{}'
```

### Wake a sleeping agent

```bash
curl -X POST http://localhost:8787/api/control/agents/my-agent/wake \
  -H "Content-Type: application/json" \
  -d '{
    "reason": "CI build completed",
    "source": "github-actions"
  }'
```

### List agent tasks

```bash
curl http://localhost:8787/api/agents/my-agent/tasks
```

### Get agent transcript

```bash
curl "http://localhost:8787/api/agents/my-agent/transcript?limit=50"
```

## Trust & Provenance

Every inbound message carries an `origin` that classifies its source. The runtime uses it to enforce trust boundaries:

- `operator` — Human operator via a trusted channel
- `channel` — External integration channel
- `webhook` — Third-party webhook
- `callback` — Runtime-delivered external trigger callback
- `timer` — Scheduled timer trigger
- `system` — Internal runtime subsystem
- `task` — Child task completion

Messages also carry `priority` (`interject`, `next`, `normal`, `background`) and trust-level metadata.

## Operator Transport Bindings

For persistent integration channels, register an operator transport binding:

```bash
curl -X POST http://localhost:8787/api/control/agents/my-agent/operator-bindings \
  -H "Content-Type: application/json" \
  -d '{
    "transport": "http_callback",
    "operator_actor_id": "slack-bot-01",
    "default_route_id": "slack-channel-general",
    "delivery_callback_url": "https://my-service.example.com/holon-delivery",
    "delivery_auth": {
      "kind": "bearer",
      "bearer_token": "my-delivery-token"
    },
    "capabilities": {
      "text": true,
      "markdown": true
    }
  }'
```

Once bound, use the operator ingress endpoint to relay messages:

```bash
curl -X POST http://localhost:8787/api/control/agents/my-agent/operator-ingress \
  -H "Content-Type: application/json" \
  -d '{
    "text": "User asked: can you explain the build error?",
    "actor_id": "slack-bot-01",
    "binding_id": "binding-abc"
  }'
```

## See Also

- [HTTP Control Plane Reference](/reference/http-control-plane.md) — Design philosophy and concepts
- [CLI Reference](/reference/cli.md) — Command-line equivalent operations
- [Configuration Reference](/reference/configuration.md) — Server and runtime configuration
