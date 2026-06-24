---
title: Integration guide
summary: Programmatic access to Holon's HTTP control plane with curl examples and endpoint reference.
order: 25
---

# Integration Guide

Holon exposes an HTTP control plane for programmatic access. Start the server with `holon serve` and interact with agents, tasks, and work items through a REST-style API.

## Starting the Server

```bash
# Local-only access
holon serve --port 8787

# With token-based authentication
holon serve --port 8787 --token "your-secret-token"
```

Access modes: `local`, `tunnel`, `lan`, `tailnet`.

## API Conventions

- **Base URL:** `http://localhost:8787`
- **Content-Type:** `application/json`
- **Authentication:** Bearer token in `Authorization` header (when `--token` is set)

## Core Endpoints

### Agent Management

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/agents/list` | List active agent entries with metadata |
| `POST` | `/control/agents/:agent_id/create` | Create a new agent |
| `GET` | `/agents/:agent_id/status` | Get agent status and lifecycle |
| `GET` | `/agents/:agent_id/state` | Get lightweight agent state bootstrap |

### Messaging

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/agents/:agent_id/enqueue` | Enqueue a message into an agent |
| `POST` | `/control/agents/:agent_id/prompt` | Send an operator prompt |
| `POST` | `/control/agents/:agent_id/wake` | Wake a sleeping agent |
| `POST` | `/control/agents/:agent_id/control` | Send a control instruction |

### Tasks & Work Items

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/control/agents/:agent_id/tasks` | Create a command task |
| `POST` | `/control/agents/:agent_id/work-items` | Create a work item |
| `POST` | `/control/agents/:agent_id/work-items/:work_item_id/pick` | Pick the current work item |
| `PATCH` | `/control/agents/:agent_id/work-items/:work_item_id` | Update a work item |
| `POST` | `/control/agents/:agent_id/work-items/:work_item_id/complete` | Complete a work item |
| `GET` | `/agents/:agent_id/tasks` | List agent tasks |
| `GET` | `/agents/:agent_id/briefs` | Get recent briefs/context |
| `GET` | `/agents/:agent_id/transcript` | Get agent transcript |
| `GET` | `/agents/:agent_id/events` | Get agent event stream |

### Workspace & Skills

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/control/agents/:agent_id/workspace/attach` | Attach a workspace |
| `POST` | `/control/agents/:agent_id/workspace/detach` | Detach workspace |
| `GET` | `/agents/:agent_id/skills` | List agent skills |
| `POST` | `/control/agents/:agent_id/skills/enable` | Enable a skill for an agent |
| `POST` | `/control/agents/:agent_id/skills/disable` | Disable a skill for an agent |
| `GET` | `/api/skills/catalog` | List Skill Library catalog |
| `POST` | `/api/skills/catalog/add` | Add a skill to the library |
| `POST` | `/api/skills/catalog/remove` | Remove a skill from the library |
| `POST` | `/api/skills/catalog/reconcile` | Reconcile library with lock file |

### Callbacks & Webhooks

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/callbacks/enqueue/:callback_token` | External callback with payload |
| `POST` | `/callbacks/wake/:callback_token` | External wake trigger |
| `POST` | `/webhooks/generic/:agent_id` | Generic webhook ingress |

### Runtime Control

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/control/runtime/status` | Runtime health status |
| `POST` | `/control/runtime/shutdown` | Graceful shutdown |

## Examples

### Send a message to an agent

```bash
curl -X POST http://localhost:8787/agents/my-agent/enqueue \
  -H "Content-Type: application/json" \
  -d '{
    "text": "Review the latest changes in src/",
    "priority": "normal",
    "origin": {
      "kind": "operator",
      "actor_id": "my-service"
    }
  }'
```

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
curl -X POST http://localhost:8787/control/agents/reviewer/create \
  -H "Content-Type: application/json" \
  -d '{"template": null}'
```

### Check agent status

```bash
curl http://localhost:8787/agents/my-agent/status
```

### Create a work item

```bash
curl -X POST http://localhost:8787/control/agents/my-agent/work-items \
  -H "Content-Type: application/json" \
  -d '{"objective": "Review and fix all clippy warnings"}'
```

### Update and complete a work item

```bash
curl -X PATCH http://localhost:8787/control/agents/my-agent/work-items/work_123 \
  -H "Content-Type: application/json" \
  -d '{
    "plan_status": "ready",
    "todo_list": [
      { "text": "Run cargo check", "state": "completed" }
    ],
    "blocked_by": "waiting for CI",
    "recheck_after": 600000
  }'

curl -X POST http://localhost:8787/control/agents/my-agent/work-items/work_123/complete \
  -H "Content-Type: application/json" \
  -d '{}'
```

### Create and cancel a timer

```bash
curl -X POST http://localhost:8787/control/agents/my-agent/timers \
  -H "Content-Type: application/json" \
  -d '{"duration_ms": 60000, "summary": "reminder"}'

curl -X POST http://localhost:8787/control/agents/my-agent/timers/timer_123/cancel \
  -H "Content-Type: application/json" \
  -d '{}'
```

### Wake a sleeping agent

```bash
curl -X POST http://localhost:8787/control/agents/my-agent/wake \
  -H "Content-Type: application/json" \
  -d '{
    "reason": "CI build completed",
    "source": "github-actions"
  }'
```

### List agent tasks

```bash
curl http://localhost:8787/agents/my-agent/tasks
```

### Get agent transcript

```bash
curl "http://localhost:8787/agents/my-agent/transcript?limit=50"
```

## Trust & Provenance

Every inbound message carries an `origin` that classifies its source. The runtime uses this to enforce trust boundaries:

- `operator` — Human operator via trusted channel
- `channel` — External integration channel
- `webhook` — Third-party webhook
- `timer` — Scheduled timer trigger
- `system` — Internal runtime subsystem
- `task` — Child task completion

Messages also carry `priority` (`interject`, `next`, `normal`, `background`) and `trust` level metadata.

## Operator Transport Bindings

For persistent integration channels, register an operator transport binding:

```bash
curl -X POST http://localhost:8787/control/agents/my-agent/operator-bindings \
  -H "Content-Type: application/json" \
  -d '{
    "transport": "http_callback",
    "operator_actor_id": "slack-bot-01",
    "default_route_id": "slack-channel-general",
    "delivery_callback_url": "https://my-service.example.com/holon-delivery",
    "delivery_auth": {
      "kind": "bearer_token",
      "token": "my-delivery-token"
    },
    "capabilities": {
      "supports_interject": true,
      "supports_rich_text": true
    }
  }'
```

Once bound, use the operator ingress endpoint to relay messages:

```bash
curl -X POST http://localhost:8787/control/agents/my-agent/operator-ingress \
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
