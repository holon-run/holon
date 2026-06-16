---
title: API contract inventory
summary: Post-baseline stability inventory for Holon's HTTP control-plane API parameters, responses, and Phase 2 contract work.
order: 25
---

# API contract inventory

This page is the post-baseline inventory for Holon's **HTTP control-plane API**.
It complements the [HTTP control plane](./http-control-plane.md) reference by
recording the route list, request parameters, response shapes, and contract
gaps that still need stabilization before scripts and integrations can rely on
the API long term.

- **Last reviewed against:** `holon` v0.14.1, `main` at `bff2293`.
- **Primary source:** `src/http.rs` Axum router and request/response structs.
- **Generated schema:** [`openapi.json`](./openapi.json), produced by
  `holon::openapi::generate_openapi_json()` and checked by
  `cargo test --test openapi_snapshot`.
- **Route/schema drift check:** `tests/snapshots/http_route_inventory.json`,
  produced from the Axum route tree and generated OpenAPI baseline by
  `cargo test --test http_route_snapshot`.
- **Client source:** `src/client.rs` for the subset consumed by the TUI/CLI.
- **Current status:** pre-1.0 baseline. Treat shapes below as observed
  behavior, not a final compatibility promise. Milestone 8 established the
  first checked baseline; the remaining gaps are Phase 2 stabilization work.

## Stability levels

| Level | Meaning |
|-------|---------|
| Candidate stable | Publicly useful and already documented or consumed by the CLI/TUI, but still needs snapshot tests or explicit schema guarantees. |
| Experimental | Available in the current runtime but likely to change as the runtime model evolves. |
| Capability | Addressed through a generated callback capability token. Do not expose or log tokens. |
| Internal/debug | Intended for local diagnostics, compatibility aliases, or temporary integration paths. |
| Gap | Missing or underspecified API surface that should be designed before it is treated as stable. |

## Cross-cutting contracts

### Transport and authentication

| Surface | Current behavior | Stability |
|---------|------------------|-----------|
| HTTP TCP | Routes are served by the Axum router in `src/http.rs`. | Candidate stable |
| Unix socket client fallback | `LocalClient` tries the configured Unix socket first when present, then falls back to HTTP. | Candidate stable for local clients; not documented as a general remote API |
| Bearer auth | When `require_control_token` is true, read routes and `/control/*` routes require `Authorization: Bearer <token>`. | Candidate stable |
| Local mode | When no control token is required, the server trusts the local process boundary. | Candidate stable, but should be tied to explicit deployment guidance |
| Callback capability tokens | `/callbacks/*/:callback_token` routes authenticate by resolving the token to an external trigger record. | Capability |

### Ingress trust/auth boundaries

| Ingress class | Routes | Auth boundary | Runtime provenance | Priority policy | Stability |
|---------------|--------|---------------|--------------------|-----------------|-----------|
| Public enqueue | `/enqueue`, `/agents/:agent_id/enqueue` | Bearer token in bearer mode; local process boundary in local mode. | Accepts only channel or webhook origins. Caller-provided `trust` is rejected. Channel origins become untrusted external evidence; webhook origins become integration signals. Runtime-owned message kinds are rejected. | Allows `next`, `normal`, and `background`; rejects `interject`. | Candidate stable |
| Callback capability | `/callbacks/wake/:callback_token`, `/callbacks/enqueue/:callback_token` | Capability token in path must resolve to an active external trigger and match the route delivery mode. Full callback URLs are secrets. | Admitted as an external-trigger capability and integration signal. Wake mode emits a runtime-owned inspection tick rather than trusting the payload as an operator instruction. | Caller does not choose queue priority. | Capability |
| Operator transport binding | `/control/agents/:agent_id/operator-bindings` | Control auth. Delivery bearer token is validated on input and redacted in audit events. | Records the binding used by remote operator ingress and delivery callbacks. | N/A. | Experimental |
| Operator transport ingress | `/control/agents/:agent_id/operator-ingress` | Control auth plus active binding, matching agent, matching actor, and matching provider when supplied. | Enqueues a trusted operator prompt with `operator_instruction` authority and remote-operator transport metadata. | Always `interject`. | Experimental |
| Generic webhook compatibility | `/webhooks/generic/:agent_id` | Bearer token in bearer mode; local process boundary in local mode. | Converts the JSON payload into a trusted-integration webhook event from `generic_webhook`; route ignores caller-supplied provenance because the whole body is the payload. | Always `normal`. | Internal/debug |

### Common JSON and response behavior

Most successful responses are JSON, but Holon does not use one global success
envelope. The stable policy is route-class based:

| Route class | Success policy | Rationale |
|-------------|----------------|-----------|
| Discovery | Envelope responses include `ok: true` plus discovery fields, except catalog-style discovery routes such as `/models` that intentionally return a direct record. | Small discovery handshakes benefit from an explicit liveness marker; catalog records should remain directly consumable. |
| Read model | Direct records or arrays, without a synthetic `ok` field. | Read routes expose existing runtime records and lists; adding wrapper envelopes would make CLI/TUI consumers unwrap data without adding state-transition information. |
| Control mutations | Envelope responses include `ok: true` plus mutation outcome fields, unless the endpoint creates and returns a first-class record. | Mutation callers need a clear admission/side-effect acknowledgement; record-creation routes can use the created record as the acknowledgement. |
| Streams | Server-Sent Events frames with JSON event data; no JSON success envelope after the stream opens. | The successful response is the stream itself. Cursor and event-envelope details are covered by the event/SSE contract. |
| Capability callbacks | Envelope responses include `ok: true` plus callback delivery result fields. | Callback callers need a compact acknowledgement while preserving the capability-specific delivery result. |

Representative current examples:

- `/` and `/handshake` return `{ "ok": true, ... }`.
- `/models` returns `{ "available_models": ..., "model_availability": ... }`
  without an `ok` field.
- `/agents/list`, `/agents/:id/status`, `/agents/:id/state`,
  `/agents/:id/tasks`, and similar read routes return direct records or arrays.
- `/control/agents/:id/prompt`, `/control/agents/:id/wake`, and
  workspace/model mutation routes return `{ "ok": true, ... }`.
- `/control/agents/:id/tasks`, `/control/agents/:id/work-items`, and
  `/control/agents/:id/timers` return the created record directly.
- `/agents/:id/events/stream` returns Server-Sent Events rather than a JSON
  response body after the stream opens.

Handler-produced control-plane errors use one shared JSON envelope:

```json
{
  "ok": false,
  "error": "message",
  "code": "machine_readable_code",
  "hint": "optional operator guidance"
}
```

For those handler-produced errors, `ok` is always `false`; `error` is a
human-readable message. `code` and `hint` are optional shared fields. Routes may
add documented route-specific extension fields alongside the shared fields.
Framework-level rejections produced before a handler runs, such as Axum
extractor failures for malformed JSON or request bodies rejected by
`DefaultBodyLimit`, are not yet normalized into this envelope.

Status-code mapping:

| Status | Class | Current mapping |
|--------|-------|-----------------|
| `400 Bad Request` | Validation | Malformed or unsupported request fields, empty required strings, invalid callback body, unsupported operator delivery auth. |
| `403 Forbidden` | Authentication/authorization | Missing, malformed, or invalid bearer token; private agent access; invalid callback capability token; ingress policy rejection. |
| `404 Not Found` | Missing resource/cursor | Unknown public agent, archived public agent, unknown compatibility route, or event cursor outside the replay window. |
| `409 Conflict` | State conflict | Stopped agent, stale or missing current run for abort, duplicate skill install, active workspace detach conflict when surfaced as a conflict. |
| `424 Failed Dependency` | Dependency unavailable | Skill manager is unavailable. |
| `502 Bad Gateway` | Upstream failure | Remote skill installer failed. |
| `504 Gateway Timeout` | Upstream timeout | Remote skill installer timed out. |
| `503 Service Unavailable` | Runtime service unavailable | Runtime service metadata is required but absent. |
| `500 Internal Server Error` | Internal/runtime error | Unexpected runtime, storage, workspace, or handler errors. |

Common route-specific error extensions currently include:

| Field | Used by | Meaning |
|-------|---------|---------|
| `agent_id` | stopped-agent and no-current-run conflicts | Agent related to the rejected operation. |
| `after_seq` / `event_seq` | event page/SSE cursor errors | Cursor sequence that was not available in the replay window. |
| `requested_run_id` / `current_run_id` | current-run abort conflicts | Stale requested run and current active run. |
| `skill_name`, `destination`, `manager`, `package`, `exit_status`, `stdout`, `stderr`, `timeout_seconds` | skill install errors | Skill-manager or remote-installer diagnostics. |

Known stable error `code` values:

| Code | Status | Meaning |
|------|--------|---------|
| `agent_stopped` | `409` | The target agent is stopped and must be started before prompts or wakes. |
| `cursor_not_found` | `404` | Requested event cursor is outside the retained replay window. |
| `stale_run_id` | `409` | Abort request named a run that is no longer current. |
| `no_current_run` | `409` | Abort request found no active run to abort. |
| `skill_already_installed` | `409` | Skill destination already exists. |
| `skill_manager_unavailable` | `424` | Required skill manager executable is unavailable. |
| `remote_skill_install_failed` | `502` | Remote skill installer exited unsuccessfully. |
| `remote_skill_install_timeout` | `504` | Remote skill installer exceeded its timeout. |

`src/client.rs` decodes this envelope compatibly: it requires only `error` for
display and preserves optional `code` and `hint` in `LocalHttpError`. Unknown
extension fields are ignored by the client unless a caller inspects the raw
response directly.

## Endpoint inventory

### Discovery and runtime

| Method | Path | Inputs | Success response | Stability | Notes |
|--------|------|--------|------------------|-----------|-------|
| `GET` | `/` | Auth header when bearer mode is active. | `{ ok, default_agent }` | Candidate stable | Root discovery for the default agent id. |
| `GET` | `/handshake` | Auth header when bearer mode is active. | `{ ok, protocol, auth, capabilities, runtime }` | Candidate stable | Protocol version is currently `holon-control` / `1`. |
| `GET` | `/models` | Auth header when bearer mode is active. | `{ available_models, model_availability }` | Experimental | Response has no `ok` envelope and returns model catalog/availability internals. |
| `GET` | `/control/runtime/readiness` | Control auth. | `RuntimeStatusResponse`-like readiness payload. | Candidate stable | Used by daemon/client readiness checks. |
| `GET` | `/control/runtime/status` | Control auth. | `RuntimeStatusResponse` with activity, startup surface, runtime config surface, and last failure. | Candidate stable | Response can expose runtime config summaries; keep credential fields redacted. |
| `POST` | `/control/runtime/shutdown` | Control auth; body is ignored/empty JSON in client. | `RuntimeShutdownResponse` | Experimental | Lifecycle control; should keep shutdown semantics explicit. |

### Agent read model

| Method | Path | Inputs | Success response | Stability | Notes |
|--------|------|--------|------------------|-----------|-------|
| `GET` | `/agents/list` | Auth header when bearer mode is active. | `AgentListEntry[]` | Candidate stable | Lightweight list for selection/navigation. |
| `GET` | `/agents/:agent_id/status` | Path `agent_id`; auth header when bearer mode is active. | `AgentSummary` | Candidate stable | Main read model for one agent. |
| `GET` | `/agents/:agent_id/state` | Path `agent_id`; auth header when bearer mode is active. | `AgentStateSnapshot` | Experimental | Lightweight bootstrap snapshot; omits operator notifications, duplicate execution details, task details, and full work-item internals. |
| `GET` | `/agents/:agent_id/briefs` | Path `agent_id`; query `limit?`. | `BriefRecord[]` | Candidate stable | Defaults to `20`. |
| `GET` | `/agents/:agent_id/tasks` | Path `agent_id`; query `limit?`. | `TaskRecord[]` | Candidate stable for list; DTO schema still broad | Defaults to `50`; active/recent task listing. |
| `GET` | `/agents/:agent_id/tasks/:task_id` | Path `agent_id`, `task_id`. | `TaskStatusSnapshot` | Candidate stable route; DTO schema still broad | Returns a single task lifecycle snapshot. |
| `GET` | `/agents/:agent_id/tasks/:task_id/output` | Path `agent_id`, `task_id`; query `block?`, `timeout_ms?`. | `TaskOutputResult` | Candidate stable route; DTO schema still broad | Reads bounded task output and can optionally wait for readiness. |
| `GET` | `/agents/:agent_id/timers` | Path `agent_id`; query `limit?`. | `TimerRecord[]` | Candidate stable | Defaults to `50`. |
| `GET` | `/agents/:agent_id/transcript` | Path `agent_id`; query `limit?`. | `TranscriptEntry[]` | Experimental | Transcript data can include provider/tool internals. |
| `GET` | `/agents/:agent_id/worktree-summary` | Path `agent_id`. | `{ agent_id, summary }` | Experimental | Summary shape follows managed worktree internals. |
| `GET` | `/agents/:agent_id/skills` | Path `agent_id`; auth header when bearer mode is active. | `{ ok, agent_id, skills }` | Experimental | Skills list shape follows local skill catalog records. |

Default-agent aliases:

| Method | Path | Alias target | Stability |
|--------|------|--------------|-----------|
| `GET` | `/status` | `/agents/:default/status` | Internal/debug compatibility |
| `GET` | `/briefs` | `/agents/:default/briefs` | Internal/debug compatibility |
| `GET` | `/state` | `/agents/:default/state` | Internal/debug compatibility |
| `GET` | `/transcript` | `/agents/:default/transcript` | Internal/debug compatibility |
| `GET` | `/worktree-summary` | `/agents/:default/worktree-summary` | Internal/debug compatibility |

### Events and streams

| Method | Path | Inputs | Success response | Stability | Notes |
|--------|------|--------|------------------|-----------|-------|
| `GET` | `/agents/:agent_id/events` | Path `agent_id`; query `before_seq?`, `after_seq?`, `limit?`, `order?`, `max_level?`. | `EventsPageResponse` | Candidate stable route and envelope | `limit` defaults to the event window and is clamped. `order` is `asc` or `desc`. `max_level` filters event inclusion only. |
| `GET` | `/agents/:agent_id/events/stream` | Path `agent_id`; query `after_seq?`, `limit?`; `Accept: text/event-stream` recommended. | SSE frames with JSON `StreamEventEnvelope` data. | Candidate stable route and envelope | SSE `id` is `event_seq`; SSE `event` is the raw audit event kind. |

Event page cursors are exclusive: `after_seq` returns records with higher
`event_seq`, `before_seq` returns records with lower `event_seq`, and combining
both returns `after_seq < event_seq < before_seq`. SSE streams start after the
current tail when `after_seq` is omitted, replay from the current replay window
when `after_seq=0`, and return `404 cursor_not_found` before opening the stream
when a non-zero `after_seq` is outside that replay window.

Stable `StreamEventEnvelope` fields:

```json
{
  "id": "event-uuid",
  "event_seq": 42,
  "ts": "2026-05-24T00:00:00Z",
  "agent_id": "main",
  "type": "task_created",
  "provenance": { "authority_class": "operator_instruction", "task_id": "task-..." },
  "payload": {}
}
```

Event payloads are the protocol standard and are included in full. The events
page may filter event inclusion with `max_level=info|verbose|debug`; filtering
does not alter `payload`. The live event stream is raw and does not support
level filtering.

Breaking migration from the removed projection contract:

- delete `projection=operator` / `projection=local_debug` from event page and
  stream requests
- stop reading `StreamEventEnvelope.projection`; all envelopes now include the
  full standard payload
- use `/agents/:id/events?max_level=info` for an operator-density historical
  page and client-side filtering for the raw stream

### Public ingress

| Method | Path | Inputs | Success response | Stability | Notes |
|--------|------|--------|------------------|-----------|-------|
| `POST` | `/enqueue` | `EnqueueRequest`; auth header when bearer mode is active. | `{ ok, agent_id, message_id }` | Candidate stable | Enqueues to the default agent. |
| `POST` | `/agents/:agent_id/enqueue` | Path `agent_id`; `EnqueueRequest`; auth header when bearer mode is active. | `{ ok, agent_id, message_id }` | Candidate stable | Public callers may not set `trust` or `interject` priority. |
| `POST` | `/webhooks/generic/:agent_id` | Path `agent_id`; JSON payload; auth header when bearer mode is active. | `{ ok, agent_id, message_id }` | Internal/debug | Compatibility/debug route that converts the payload into a trusted integration webhook event. Prefer public enqueue or callback capabilities for new integrations. |

`EnqueueRequest` fields:

| Field | Type / values | Required | Notes |
|-------|---------------|----------|-------|
| `kind` | `channel_event`, `webhook_event`, etc. | No | Defaults to `webhook_event`. Runtime-owned kinds such as `system_tick` and `callback_event` are rejected. |
| `priority` | `next`, `normal`, `background`; `interject` only for trusted ingress | No | Defaults to `normal`; public enqueue rejects `interject`. |
| `trust` | `trusted_operator`, `trusted_system`, `trusted_integration`, `untrusted_external` | No | Public enqueue rejects caller-provided trust. |
| `body` | `MessageBody` | No | Used as-is when present. |
| `text` | string | No | Converted to text body when `body` is absent. |
| `json` | JSON value | No | Converted to JSON body when `body` and `text` are absent. |
| `metadata` | JSON value | No | Stored on the message. |
| `correlation_id` / `causation_id` | string | No | Passed through to the message envelope. |
| `origin` | `channel` or `webhook` for public enqueue | No | Public enqueue rejects operator, timer, system, and task origins. |

### Control actions

| Method | Path | Request body | Success response | Stability | Notes |
|--------|------|--------------|------------------|-----------|-------|
| `POST` | `/control/agents/:agent_id/prompt` | `{ text }` | `{ ok, agent_id, message_id }` | Candidate stable | Enqueues a trusted operator prompt with `interject` priority. |
| `POST` | `/control/agents/:agent_id/wake` | `{ reason, source?, correlation_id?, causation_id? }` | `{ ok, agent_id, disposition }` | Candidate stable | Empty `reason` is rejected. Does not start a stopped agent. |
| `POST` | `/control/agents/:agent_id/control` | `{ action, authority_class? }`; `action` is `start` or `stop`. | `{ ok }` | Candidate stable | `authority_class` is currently audit/provenance metadata only. |
| `POST` | `/control/agents/:agent_id/current-run/abort` | `{ run_id?, mode?, authority_class? }` | `{ ok, aborted, agent_id, run_id, mode, admission_context, provided_trust }` | Candidate stable | `mode` defaults to `stop_after_abort`; deprecated alias `pause_after_abort` is accepted. |
| `POST` | `/control/agents/:agent_id/create` | `{ template?, authority_class? }` | `AgentSummary` | Experimental | Path id names the created agent. |
| `POST` | `/control/agents/:agent_id/debug-prompt` | `{ text, authority_class? }` | `{ ok, agent_id, dump }` | Internal/debug | Dumps prompt rendering and should not be a stable automation API. |

### Tasks, work items, and timers

| Method | Path | Request body | Success response | Stability | Notes |
|--------|------|--------------|------------------|-----------|-------|
| `POST` | `/control/agents/:agent_id/tasks` | `CreateCommandTaskRequest` | `TaskRecord` | Candidate stable for creation; DTO schema still broad | `serde(deny_unknown_fields)` rejects legacy fields. |
| `POST` | `/control/agents/:agent_id/tasks/:task_id/input` | `{ text, authority_class? }` | `TaskInputResult` | Candidate stable route; DTO schema still broad | Delivers operator-authority text to an interactive command task or supervised child-agent task. |
| `POST` | `/control/agents/:agent_id/tasks/:task_id/stop` | `{ authority_class? }` | `TaskStopResult` | Candidate stable route; DTO schema still broad | Requests managed-task cancellation. |
| `GET` | `/agents/:agent_id/work-items` | none | `WorkItemRecord[]` | Experimental read model; CLI schema owner | Query parameter: `limit`; used by `holon work-item list`. |
| `GET` | `/agents/:agent_id/work-items/:work_item_id` | none | `WorkItemRecord` | Experimental read model; CLI schema owner | Used by `holon work-item get`; returns 404 when the id is not found for the target agent. |
| `POST` | `/control/agents/:agent_id/work-items` | `{ objective, authority_class? }` | `WorkItemRecord` | Experimental | Creates/enqueues work items. |
| `POST` | `/control/agents/:agent_id/work-items/:work_item_id/pick` | `{ reason?, clear_blocker?, authority_class? }` | `PickWorkItemResponse` | Experimental | Sets the current WorkItem focus and can explicitly clear a resolved blocker when `clear_blocker=true` and `reason` is provided. |
| `PATCH` | `/control/agents/:agent_id/work-items/:work_item_id` | `UpdateWorkItemRequest` | `WorkItemRecord` | Experimental | Mutates objective, plan status, todo list, and legacy blocker/recheck fields. Empty mutations are rejected. |
| `POST` | `/control/agents/:agent_id/work-items/:work_item_id/complete` | `{ authority_class? }` | `WorkItemRecord` | Experimental | Marks an open WorkItem completed; cancel/delete remains out of scope. |
| `GET` | `/agents/:agent_id/timers/:timer_id` | Path `agent_id`, `timer_id`. | `TimerRecord` | Candidate stable route; DTO schema still broad | Returns 404 when the timer id is not found for the target agent. |
| `POST` | `/control/agents/:agent_id/timers` | `{ duration_ms, interval_ms?, summary?, authority_class? }` | `TimerRecord` | Candidate stable | `duration_ms` is required; `interval_ms` makes a repeating timer. |
| `POST` | `/control/agents/:agent_id/timers/:timer_id/cancel` | `{ authority_class? }` | `TimerRecord` | Candidate stable | Idempotent for already-cancelled timers. Missing timers return 404; completed timers return 400 because they already fired. |

`CreateCommandTaskRequest` fields:

| Field | Type | Required | Default / notes |
|-------|------|----------|-----------------|
| `summary` | string | Yes | Task summary. |
| `cmd` | string | Yes | Command line passed to the command task runner. |
| `workdir` | string or null | No | Optional working directory. |
| `shell` | string or null | No | Optional shell. |
| `login` | bool | No | Defaults to `true`. |
| `tty` | bool | No | Defaults to `false`. |
| `yield_time_ms` | integer | No | Defaults to `10000`. |
| `max_output_tokens` | integer or null | No | Bounded output preview budget. |
| `accepts_input` | bool | No | Defaults to `false`. |
| `authority_class` | `AuthorityClass` or null | No | Defaults to operator authority for admitted control-plane requests; recorded in provenance/audit. |

### Workspace and model controls

| Method | Path | Request body | Success response | Stability | Notes |
|--------|------|--------------|------------------|-----------|-------|
| `POST` | `/control/agents/:agent_id/workspace/attach` | `{ path, authority_class? }` | `{ ok, agent_id, workspace_id, workspace_anchor }` | Candidate stable | `path` is converted into a workspace entry. |
| `POST` | `/control/agents/:agent_id/workspace/exit` | `{ authority_class? }` | `{ ok, agent_id }` | Candidate stable | Returns agent to default workspace behavior. |
| `POST` | `/control/agents/:agent_id/workspace/detach` | `{ workspace_id, authority_class? }` | `{ ok, agent_id, workspace_id }` | Candidate stable | `workspace_id` is trimmed before use. |
| `POST` | `/control/agents/:agent_id/model` | `{ model, reasoning_effort?, authority_class? }` | `{ ok, agent_id, model }` | Experimental | `reasoning_effort` must be `low`, `medium`, `high`, or `xhigh`. |
| `POST` | `/control/agents/:agent_id/model/clear` | `{ authority_class? }` | `{ ok, agent_id, model }` | Experimental | Clears the agent-level model override. |

### Operator transport integration

| Method | Path | Request body | Success response | Stability | Notes |
|--------|------|--------------|------------------|-----------|-------|
| `POST` | `/control/agents/:agent_id/operator-bindings` | `OperatorTransportBindingRequest` | `{ ok, agent_id, binding }` | Experimental | `target_agent_id`, when provided, must match the route `agent_id`. |
| `POST` | `/control/agents/:agent_id/operator-ingress` | `OperatorIngressRequest` | `{ ok, agent_id, message_id }` | Experimental | Requires an active binding and matching actor/provider. Enqueues a trusted operator prompt. |

`OperatorTransportBindingRequest` is strict (`deny_unknown_fields`) and
contains `binding_id?`, `transport`, `operator_actor_id`, `target_agent_id?`,
`default_route_id`, `delivery_callback_url`, `delivery_auth`, `capabilities`,
`provider?`, `provider_identity_ref?`, and `metadata?`.

`delivery_auth.kind = "bearer"` requires a non-empty `bearer_token`.
`delivery_auth.kind = "hmac"` is rejected until HMAC signing is implemented.

### Skills

| Method | Path | Request body | Success response | Stability | Notes |
|--------|------|--------------|------------------|-----------|-------|
| `POST` | `/control/agents/:agent_id/skills/install` | `{ kind }` where `kind` is a `SkillInstallKind` tagged union. | `{ ok, agent_id, skill_name }` | Experimental | Install errors can map to conflict, failed dependency, bad gateway, or gateway timeout. |
| `POST` | `/control/agents/:agent_id/skills/uninstall` | `{ name }` | `{ ok, agent_id, skill_name }` | Experimental | Removes a skill by name from the agent home. |

### Callback capability ingress

| Method | Path | Inputs | Success response | Stability | Notes |
|--------|------|--------|------------------|-----------|-------|
| `POST` | `/callbacks/enqueue/:callback_token` | Capability token in path; arbitrary body; `Content-Type` guides body decoding. | `{ ok, ...CallbackDeliveryResult }` | Capability | Token must resolve to an active external trigger with enqueue delivery mode. |
| `POST` | `/callbacks/wake/:callback_token` | Capability token in path; arbitrary body; `Content-Type` guides body decoding. | `{ ok, ...CallbackDeliveryResult }` | Capability | Token must resolve to an active external trigger with wake delivery mode. |

Body decoding rules:

- JSON content types become `MessageBody::Json`.
- `text/*` content types become `MessageBody::Text`.
- Other typed bodies become JSON with `content_type` and `body_base64`.
- Untyped UTF-8 bodies become text; non-UTF-8 bodies become JSON with
  `body_base64`.

## Shared record shapes to stabilize

The following response types are exposed by multiple endpoints and should be
treated as schema surfaces, not incidental Rust structs:

| Shape | Returned by | Key stability concerns |
|-------|-------------|------------------------|
| `AgentSummary` | `/agents/:id/status`, `/agents/:id/state`, agent creation | Identity visibility/ownership/profile fields, status enum, model state, workspace fields. |
| `AgentListEntry` | `/agents/list` | Keep lightweight; avoid reintroducing heavy runtime/model payloads. |
| `TaskRecord` | `/agents/:id/tasks`, task creation, state snapshot, events | Task kind/status enums, detail truncation, recovery metadata, output references. |
| `WorkItemRecord` | work-item creation, state snapshot, events | State, plan status, plan artifact, todo list, blockers/recheck timestamps. |
| `TimerRecord` | `/agents/:id/timers`, `/agents/:id/timers/:timer_id`, timer creation, state snapshot | Repeating timer fields and status enum. |
| `BriefRecord` | `/agents/:id/briefs` | User-facing delivery vs internal traces. |
| `TranscriptEntry` | `/agents/:id/transcript` | Potential provider/tool internals and truncation policy. |
| `StreamEventEnvelope` | events page and SSE stream | Projection/redaction, provenance, payload versioning. |
| `RuntimeStatusResponse` | runtime readiness/status | Startup/runtime config surface and credential redaction. |
| `SkillInstallKind` | skills install | Tagged union variants and local/remote package semantics. |

## Detected contract gaps

1. **OpenAPI route/type metadata is still only partially colocated with
   implementation.** Route coverage and schema snapshots now exist, but the
   generated baseline still depends on a conservative OpenAPI table. Phase 2
   moves operation metadata closer to Axum route/type definitions.
2. **Stable DTO schemas need tightening.** Several task, work-item, timer,
   agent, event, and envelope responses are still represented broadly in the
   OpenAPI baseline and should become first-class typed components where they
   are stable client contracts.
3. **Event level filtering needs more real-world coverage.** `max_level`
   inclusion rules should be validated against real TUI/client sessions before
   they are treated as final.
4. **WorkItem mutation APIs now cover focus/update/complete.** HTTP can
   list/get/create/enqueue, pick focus, update objective/planning/blocker
   fields, and complete work items. Cancel/delete remains intentionally out of
   scope until a distinct lifecycle contract is needed.
5. **Timer lifecycle APIs now cover cancellation.** HTTP can create/list/get
   timers and cancel active timers. Delete/purge remains out of scope.
6. **Deployment guidance still needs hardening.** The HTTP ingress trust/auth
   table is documented, but production-facing guidance should still describe
   when to use bearer mode, local mode, callback capabilities, and dedicated
   operator adapters.
7. **Tool schema is a separate API surface.** Built-in tool input/result schemas
   now have their own checked inventory; this HTTP inventory should link to it
   rather than duplicate it.

## Tracking issues

Milestone 8 baseline issues are complete. Phase 2 follow-up issues are grouped
under the same
[`CLI/API Stability Contracts`](https://github.com/holon-run/holon/milestone/8)
milestone and tracked by
[#1444](https://github.com/holon-run/holon/issues/1444).

| Issue | Scope |
|-------|-------|
| [#1438](https://github.com/holon-run/holon/issues/1438) `api: migrate OpenAPI baseline to aide route/type metadata` | Move OpenAPI operation metadata closer to route and DTO definitions. |
| [#1439](https://github.com/holon-run/holon/issues/1439) `api: tighten OpenAPI DTO schemas for stable read models` | Replace selected generic JSON schemas with typed stable DTO schemas. |
| [#1443](https://github.com/holon-run/holon/issues/1443) `events: define stable operator-facing event payload subset` | Version/document stable event fields. Event payloads are the protocol standard; `max_level` filters event inclusion only. |

## Suggested next work

1. Migrate the OpenAPI baseline toward `aide` route/type metadata
   ([#1438](https://github.com/holon-run/holon/issues/1438)).
2. Tighten DTO schemas for stable read models and control-plane results
   ([#1439](https://github.com/holon-run/holon/issues/1439)).
3. Define the stable operator-facing event payload subset
   ([#1443](https://github.com/holon-run/holon/issues/1443)).
