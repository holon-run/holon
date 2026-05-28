---
title: HTTP control plane
summary: How to think about Holon's headless integration surface.
order: 20
---

# HTTP control plane

Holon is designed to be headless. HTTP and event-driven integration surfaces
should preserve the same runtime concepts as the CLI: origin, trust, priority,
work items, tasks, queues, wakeups, and user-facing delivery.

The current generated OpenAPI baseline is checked in at
[`openapi.json`](./openapi.json). It is a conservative schema for the present
route surface; some request and response schemas intentionally remain broad
until the Phase 2 route/type metadata and DTO contracts are stabilized.

## Authentication

When a control token is configured (e.g. `--token`, `--token-file`, or the
`control_token` config key), the HTTP server operates in **bearer mode**.
All `/control/*` routes require an `Authorization: Bearer <token>` header,
and read-only routes (agent state, events, tasks) require it for remote
access as well. Without a control token, the server runs in **local mode**
and trusts the local process boundary.

```
GET /handshake → { "auth": { "mode": "bearer" | "local", "required": bool } }
```

## Ingress trust and auth boundaries

Holon treats authentication, message origin, trust level, priority, and
authority as separate runtime facts. HTTP ingress handlers may authenticate a
caller, but they still construct the message provenance explicitly instead of
trusting caller-supplied provenance fields.

| Ingress class | Routes | Auth boundary | Origin/trust/authority | Priority | Supported posture |
|---------------|--------|---------------|------------------------|----------|-------------------|
| Public enqueue | `POST /enqueue`, `POST /agents/:id/enqueue` | Bearer token in bearer mode; local process boundary in local mode. | Caller may provide only channel or webhook origin. Caller-provided `trust` is rejected. Channel origins become untrusted external evidence; webhook origins become integration signals. Runtime-owned kinds such as `system_tick` and `callback_event` are rejected. | `next`, `normal`, or `background`; `interject` is rejected. | Candidate stable external ingress for non-operator evidence. |
| Callback capability | `POST /callbacks/wake/:callback_token`, `POST /callbacks/enqueue/:callback_token` | Capability token in the URL path resolves to an active external trigger and matching delivery mode. Do not log, repeat, or publish full callback URLs. | Delivery is admitted as an external-trigger capability and an integration signal. Wake callbacks enqueue runtime-owned inspection ticks; callback payload text is untrusted evidence for the agent to inspect. | Runtime-selected by delivery mode; callers do not choose queue priority. | Capability surface for durable external systems that need to wake or notify an agent. |
| Operator transport binding | `POST /control/agents/:id/operator-bindings` | Control-plane auth. Delivery credentials are stored on the binding and redacted from audit events. | Creates or updates the binding that later authorizes remote operator ingress. | N/A. | Experimental operator adapter setup surface. |
| Operator transport ingress | `POST /control/agents/:id/operator-ingress` | Control-plane auth plus active binding, matching target agent, matching operator actor, and matching provider when supplied. | Enqueues a `trusted_operator` `operator_prompt` with `operator_instruction` authority and remote-operator transport metadata. | Always `interject`. | Experimental authenticated operator adapter ingress. |
| Generic webhook compatibility | `POST /webhooks/generic/:agent_id` | Bearer token in bearer mode; local process boundary in local mode. | Converts JSON payload into a trusted-integration webhook event with `generic_webhook` origin. Callers cannot set origin, trust, or priority through this route. | Always `normal`. | Internal/debug compatibility route; prefer public enqueue or a dedicated capability callback for new external integrations. |

## Endpoint reference

### Discovery

**`GET /`** — Root

Returns the default agent ID.

```json
{ "ok": true, "default_agent": "main" }
```

**`GET /handshake`** — Protocol handshake

Returns auth mode, capabilities, and runtime info.

```json
{
  "ok": true,
  "protocol": { "name": "holon-control", "version": 1 },
  "auth": { "mode": "bearer", "required": true },
  "capabilities": ["agents.list", "agents.state", "agents.events", "agents.control", "tui.remote"],
  "runtime": {
    "default_agent": "main",
    "workspace_dir": "/path/to/workspace",
    "home_dir": "/path/to/holon/home",
    "listen": "127.0.0.1:9101",
    "advertise_url": null
  }
}
```

**`GET /models`** — Available models

Returns model catalog and runtime availability.

```json
{
  "available_models": [
    { "id": "claude-sonnet-4-20250514", "display_name": "Claude Sonnet 4", … }
  ],
  "model_availability": { "claude-sonnet-4-20250514": true, … }
}
```

### Agents

**`GET /agents/list`** — List agent entries

Returns lightweight public agent entries for selection and navigation without
loading full per-agent runtime summaries.

**`GET /agents/:id/status`** — Single agent status

Returns the same `AgentSummary` shape for the named agent.

**`GET /agents/:id/state`** — Full agent state snapshot

Returns a combined state page: agent summary, session info (current run, pending
count), active tasks, recent timers, work items, waiting intents, external
triggers, operator notifications, execution snapshot, and workspace occupancy.

**`GET /agents/:id/briefs`** — Recent briefs

Returns recent briefs (acknowledgements and results) for the agent.

**`GET /agents/:id/tasks`** — Active tasks

Returns active and recent tasks with status, kind, and timing metadata.

**`GET /agents/:id/tasks/:task_id`** — Task status

Returns the structured task lifecycle snapshot for one managed task.

**`GET /agents/:id/tasks/:task_id/output`** — Task output

Returns a bounded task output result. Query parameters:

| Param | Description |
|-------|-------------|
| `block` | Whether to wait for output/completion before returning |
| `timeout_ms` | Optional bounded wait duration when `block=true` |

**`GET /agents/:id/timers`** — Recent timers

Returns recent timer records.

**`GET /agents/:id/timers/:timer_id`** — Timer detail

Returns a single timer record by id, or a shared error envelope when the timer
is not found for the target agent.

**`GET /agents/:id/events`** — Event log

Returns recent runtime events (turn entries, system events). Query parameters:

| Param | Description |
|-------|-------------|
| `before_seq` | Return events with durable `event_seq` lower than this value |
| `after_seq` | Return events with durable `event_seq` higher than this value |
| `limit` | Max events to return (default 128) |
| `order` | `asc` or `desc` (default) |
| `max_level` | Optional inclusion filter: `info`, `verbose`, or `debug` |

The JSON response is an `EventsPageResponse`:

| Field | Contract |
|-------|----------|
| `events` | Array of full-payload `StreamEventEnvelope` records matching the requested level filter |
| `oldest_seq` / `newest_seq` | Lowest/highest durable `event_seq` in the returned page, or `null` for an empty page |
| `cursor_seq` | Raw event-log high-watermark captured while serving the page; clients can start the raw stream after this cursor |
| `has_older` / `has_newer` | Whether more matching records are available before/after the returned window |
| `order` | Echoes the requested order |
| `limit` | Effective limit after server clamping |

`before_seq` and `after_seq` are exclusive cursors. When both are supplied, the
page contains events where `after_seq < event_seq < before_seq`. Event pages are
loaded from the durable event log, so an unknown cursor can yield an empty page
rather than a cursor error.

**`GET /agents/:id/events/stream`** — Server-sent events

SSE stream of raw agent events. Supports `after_seq` and `limit` query params.
The SSE `id` field is the per-agent durable `event_seq`, and the
SSE `event` field is set to the raw audit event kind (e.g. `turn_entry`,
`wake_requested`, `task_create_requested`), not a limited set of names.

Each SSE `data` frame is a JSON `StreamEventEnvelope` with these stable fields:

| Field | Contract |
|-------|----------|
| `id` | Audit event id. This is not the SSE frame id. |
| `event_seq` | Per-agent durable sequence number; equal to the SSE `id` field |
| `ts` | RFC 3339 timestamp |
| `agent_id` | Agent that owns the event log |
| `type` | Raw audit event kind; equal to the SSE `event` field |
| `provenance` | Stable provenance fields extracted from the raw payload when present |
| `payload` | Full event payload |

When `after_seq` is omitted, the stream starts after the current tail and only
emits future events. When `after_seq=0`, it replays from the beginning of the
current replay window. For non-zero `after_seq`, the cursor must still be inside
the replay window; otherwise the route returns `404` with `code:
cursor_not_found` and `after_seq`/`event_seq` extension fields before opening
the SSE stream. If an already-open stream falls behind the replay window, the
server closes that stream.

Filtering behavior:

- Event payloads are always included in full.
- `/agents/:id/events` may use `max_level` to filter which events are returned.
- `/agents/:id/events/stream` is raw and does not support `max_level`.

**`GET /agents/:id/transcript`** — Turn transcript

Returns the current turn transcript entries.

**`GET /agents/:id/worktree-summary`** — Worktree summary

Returns managed worktree entries for the agent's workspace.

### Enqueue (public ingress)

**`POST /agents/:id/enqueue`** — Enqueue a message

Accepts external callers on the public HTTP surface. When the server is in
**bearer mode**, this route calls `authorize_remote_access` and requires the
control token just like read-only routes. In **local mode** no auth header is
needed.

The runtime classifies origin, trust, and priority; public callers may not
override trust or use `interject` priority.
Request shape:

```json
{
  "kind": "channel_event | webhook_event",
  "priority": "next | normal | background",
  "text": "plain text body",
  "json": { "structured": "body" },
  "body": { "type": "text", "text": "…" },
  "origin": {
    "kind": "channel",
    "channel_id": "slack-general",
    "sender_id": "U123"
  },
  "metadata": {},
  "correlation_id": "optional-correlation",
  "causation_id": "optional-causation"
}
```

Response:

```json
{ "ok": true, "agent_id": "main", "message_id": "msg-abc123" }
```

**`POST /enqueue`** (no agent in path) — Enqueue to default agent.

**`POST /webhooks/generic/:agent_id`** — Generic webhook compatibility

Accepts a JSON payload and converts it to a `webhook_event` from
`generic_webhook`. This route is kept for local/debug compatibility and simple
trusted integration tests. It requires the bearer token when bearer mode is
enabled, but unlike public enqueue it does not let callers provide explicit
origin/trust/priority fields. New integrations should usually use public
enqueue for external evidence or a callback capability when the integration
needs a secret URL.


### Control plane (authenticated)

All `/control/*` routes require a control token when the server is in bearer
mode.

**`POST /control/agents/:id/prompt`** — Send an operator prompt

Sends a prompt that enters the agent queue as an operator message with
`trusted_operator` classification.

```json
{ "text": "What is the current status?" }
```

**`POST /control/agents/:id/wake`** — Explicit wake

Wakes a sleeping agent with a control-plane wake hint.

```json
{ "reason": "manual-wake", "source": "operator" }
```

Response:

```json
{ "ok": true, "agent_id": "main", "disposition": "woken" }
```

**`POST /control/agents/:id/control`** — Control action

Sends a control action. Request body:

```json
{ "action": "stop", "authority_class": "operator_instruction" }
```

**`POST /control/agents/:id/current-run/abort`** — Abort current run

Aborts the current agent run loop. New callers should use
`mode: "stop_after_abort"`. The legacy `pause_after_abort` value is accepted as
a compatibility alias and is treated as `stop_after_abort`.

```json
{ "mode": "stop_after_abort" }
```

**`POST /control/agents/:id/create`** — Create agent

Creates a new agent managed by the host. The agent id comes from the URL path.

```json
{ "template": null, "authority_class": "operator_instruction" }
```

**`POST /control/agents/:id/tasks`** — Create command task

Starts a background command task for the agent.

```json
{
  "summary": "Build project",
  "cmd": "cargo build",
  "workdir": null,
  "shell": null,
  "login": false
}
```

**`POST /control/agents/:id/tasks/:task_id/input`** — Send task input

Delivers text input to a managed task using trusted operator authority. Command
tasks receive stdin or TTY text when they were created with interactive input
enabled; supervised child-agent tasks receive a follow-up input.

```json
{ "text": "continue\n", "authority_class": "operator_instruction" }
```

**`POST /control/agents/:id/tasks/:task_id/stop`** — Stop task

Requests cancellation for a managed task and returns the structured task stop
receipt.

```json
{ "authority_class": "operator_instruction" }
```

**`POST /control/agents/:id/work-items`** — Create work item

Creates a durable work item for the agent.

```json
{
  "objective": "Fix the build",
  "authority_class": "operator_instruction"
}
```

**`POST /control/agents/:id/work-items/:work_item_id/pick`** — Pick work item

Makes an existing open work item the current focus for the agent. The response
returns the previous focus, current focus, current work item id, and the
recorded focus transition.

```json
{ "reason": "external scheduler selected next work", "authority_class": "integration_signal" }
```

**`PATCH /control/agents/:id/work-items/:work_item_id`** — Update work item

Mutates one or more WorkItem fields. Empty updates are rejected. `blocked_by`
uses a nested optional shape: a string sets the blocker, `null` clears it, and
omitting the field leaves it unchanged. `recheck_after` is milliseconds and
requires a non-empty blocker.

```json
{
  "objective": "Fix the build and update docs",
  "plan_status": "ready",
  "todo_list": [{ "text": "Run cargo check", "state": "completed" }],
  "blocked_by": "waiting for CI",
  "recheck_after": 600000,
  "authority_class": "operator_instruction"
}
```

**`POST /control/agents/:id/work-items/:work_item_id/complete`** — Complete work item

Marks an open work item completed and returns the updated `WorkItemRecord`.
Cancel, close-without-completion, and delete are intentionally out of scope for
this lifecycle surface.

```json
{ "authority_class": "operator_instruction" }
```

**`POST /control/agents/:id/timers`** — Create timer

Creates a timer that will deliver a `TimerTick` to the agent.

```json
{
  "duration_ms": 60000,
  "interval_ms": null,
  "summary": "reminder",
  "authority_class": "operator_instruction"
}
```

**`POST /control/agents/:id/timers/:timer_id/cancel`** — Cancel timer

Cancels an active timer and returns the updated `TimerRecord`. Cancellation is
idempotent for an already-cancelled timer. A missing timer returns a shared
404 error envelope; a completed timer returns a shared 400 lifecycle error
because it already fired. Cancellation immediately updates timer list/detail
projections and emits `timer_cancelled`.

```json
{ "authority_class": "operator_instruction" }
```

**`POST /control/agents/:id/debug-prompt`** — Debug prompt

Sends a debug-mode prompt (runtime-internal classification). Request body:

```json
{ "text": "debug instruction", "authority_class": "operator_instruction" }
```

**`POST /control/agents/:id/operator-bindings`** — Create operator transport binding

Sets up a callback URL or transport binding for operator notifications.
Request body:

```json
{
  "binding_id": "my-binding",
  "transport": "http-callback",
  "operator_actor_id": "operator-1",
  "default_route_id": "default",
  "delivery_callback_url": "https://example.com/callback",
  "delivery_auth": { "type": "bearer", "token": "secret" },
  "capabilities": { "send_prompt": true },
  "provider": "anthropic",
  "provider_identity_ref": "user-123",
  "metadata": {}
}
```

**`POST /control/agents/:id/operator-ingress`** — Operator ingress

Direct ingress path for operator-origin messages through the control plane.
Request body:

```json
{
  "text": "operator message",
  "actor_id": "operator-1",
  "binding_id": "my-binding",
  "reply_route_id": "route-1",
  "provider": "anthropic",
  "correlation_id": "corr-123"
}
```

**`POST /control/agents/:id/workspace/attach`** — Attach workspace

```json
{ "path": "/path/to/workspace" }
```

**`POST /control/agents/:id/workspace/exit`** — Exit current workspace

Returns to the agent home workspace. Accepts an optional body:

```json
{ "authority_class": "operator_instruction" }
```

**`POST /control/agents/:id/workspace/detach`** — Detach workspace

Removes a workspace registration without switching. Request body:

```json
{ "workspace_id": "ws-abc123", "authority_class": "operator_instruction" }
```

**`POST /control/agents/:id/model`** — Set agent model

```json
{ "model": "claude-sonnet-4-20250514" }
```

**`POST /control/agents/:id/model/clear`** — Clear model override

Reverts to the default model. Accepts an optional body:

```json
{ "authority_class": "operator_instruction" }
```

### Runtime management

**`GET /control/runtime/status`** — Runtime status

Returns daemon and runtime health info including configured models, control
token status, and activity markers.

**`POST /control/runtime/shutdown`** — Graceful shutdown

Shuts down the runtime and daemon gracefully.

### Webhooks & callbacks

**`POST /webhooks/generic/:agent_id`** — Generic webhook

Accepts arbitrary JSON payloads and enqueues them as `WebhookEvent` messages
to the named agent. Useful for GitHub webhooks, CI notifications, and external
service integrations.

**`POST /callbacks/enqueue/:callback_token`** — Callback enqueue

Receives enqueue callbacks from registered callback URLs. Body limit: 256 KB.

**`POST /callbacks/wake/:callback_token`** — Callback wake

Receives wake callbacks from registered callback URLs.

## Message shapes

### MessageKind

Valid enqueue kinds for external callers: `channel_event`, `webhook_event`.
The policy only allows `operator_prompt` with an operator origin; public
enqueue rejects it. Runtime-owned kinds (`system_tick`, `task_result`,
`task_status`, `control`, `internal_followup`) are rejected from
external enqueue.

### Priority

| Value | Behavior |
|-------|----------|
| `interject` | Preempts normal queue; control-plane only |
| `next` | After current turn, before queued |
| `normal` | Standard queue position |
| `background` | Low urgency, processed when idle |

### TrustLevel

| Value | Default origin | Meaning |
|-------|----------------|---------|
| `trusted_operator` | `operator` | Direct operator action |
| `trusted_system` | `system`, `task`, `timer` | Runtime-internal action |
| `trusted_integration` | `webhook`, `callback` | Known integration with explicit trust |
| `untrusted_external` | `channel` | Public channel / unauthenticated caller |

### MessageOrigin

| Kind | Fields |
|------|--------|
| `operator` | `actor_id` (optional) |
| `channel` | `channel_id`, `sender_id` (optional) |
| `webhook` | `source`, `event_type` (optional) |
| `callback` | `descriptor_id`, `source` (optional) |
| `timer` | `timer_id` |
| `system` | `subsystem` |
| `task` | `task_id` |

### MessageBody

| Type | Fields |
|------|--------|
| `text` | `text: string` |
| `json` | `value: object` |
| `brief` | `title`, `text`, `attachments` |

## Design goals

- Keep transport details outside the core runtime model.
- Preserve provenance for inbound messages and external events.
- Return structured lifecycle state rather than only streaming text.
- Make wake, sleep, enqueue, and task supervision visible to integrations.
- Keep user-facing output separate from internal traces.

## Integration posture

Treat the HTTP surface as a control plane for runtime state, not a chat-only
endpoint. A good integration should be able to ask:

- What work is active?
- Which tasks are running or waiting?
- What event woke the agent?
- Which output is safe to show to a user?
- Which evidence is internal runtime detail?

### Routes not yet documented

The following routes exist in `src/http.rs` but are not yet fully documented
on this reference page:

- `GET /agents/:id/skills`
- `POST /control/agents/:id/skills/install`
- `POST /control/agents/:id/skills/uninstall`
- Default-agent aliases: `/status`, `/briefs`, `/state`, `/transcript`,
  `/worktree-summary`

These will be added as the surface stabilizes.

## Common curl examples

```bash
# Check server health
curl http://127.0.0.1:9101/handshake

# List agents
curl http://127.0.0.1:9101/agents/list

# Get agent state
curl http://127.0.0.1:9101/agents/main/state

# Send a prompt (control token required)
curl -X POST http://127.0.0.1:9101/control/agents/main/prompt \
  -H "Authorization: Bearer $HOLON_CONTROL_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"text": "Run cargo check"}'

# Enqueue via webhook (public)
curl -X POST http://127.0.0.1:9101/webhooks/generic/main \
  -H "Content-Type: application/json" \
  -d '{"event": "ci-complete", "status": "success"}'

# Stream agent events
curl -N http://127.0.0.1:9101/agents/main/events/stream
```
