---
title: Integration guide
summary: Step-by-step how-to guide for integrating external systems with Holon via the HTTP control plane.
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

> **Authoritative reference:** For the complete list of endpoints, request/response schemas, and error codes, see the [HTTP Control Plane Reference](/reference/http-control-plane.md) and the machine-readable [OpenAPI 3.1 schema](/reference/openapi.json).

## End-to-end Integration Workflow

### 1. Send a request to an agent

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
