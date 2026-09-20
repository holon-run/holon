---
title: Automate Holon over HTTP
summary: Authenticate, submit work, follow status, and read results from code.
order: 14
---

# Automate Holon over HTTP

Holon's HTTP control plane lets a script or service drive agents, tasks, and work
items without the TUI. This guide starts the server, authenticates, and walks one
request through to a result. Every route is served under `/api`.

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

The full endpoint list — methods, payloads, and auth requirements — lives in the
[HTTP control plane reference](/reference/http-control-plane.md). Find your route
there, then come back here for the workflow.

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

Every request carries an origin and a trust level, and the runtime keeps them
apart. How those classifications work is in
[Trust boundaries](/concepts/trust-boundaries.md).

Advanced operator-side controls, including transport bindings, are collected in the
[HTTP control plane reference](/reference/http-control-plane.md).

## See Also

- [HTTP control plane reference](/reference/http-control-plane.md) — design goals and endpoint details
- [CLI reference](/reference/cli.md) — the same operations from the command line
- [Configuration reference](/reference/configuration.md) — server and runtime configuration
