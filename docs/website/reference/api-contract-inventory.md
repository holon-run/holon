---
title: API contract inventory
summary: First-pass stability inventory for Holon's HTTP control-plane API parameters, responses, and follow-up contract work.
order: 25
---

# API contract inventory

This page is the first-pass inventory for Holon's **HTTP control-plane API**.
It complements the [HTTP control plane](./http-control-plane.md) reference by
recording the route list, request parameters, response shapes, and contract
gaps that need stabilization before scripts and integrations can rely on the
API long term.

- **Last reviewed against:** `holon` v0.14.1, `main` at `77fe575`.
- **Primary source:** `src/http.rs` Axum router and request/response structs.
- **Generated schema:** [`openapi.json`](./openapi.json), produced by
  `holon::openapi::generate_openapi_json()` and checked by
  `cargo test --test openapi_snapshot`.
- **Route/schema drift check:** `tests/snapshots/http_route_inventory.json`,
  produced from the Axum route tree and generated OpenAPI baseline by
  `cargo test --test http_route_snapshot`.
- **Client source:** `src/client.rs` for the subset consumed by the TUI/CLI.
- **Current status:** pre-1.0 baseline. Treat shapes below as observed
  behavior, not a final compatibility promise.

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
| `GET` | `/agents/:agent_id/state` | Path `agent_id`; auth header when bearer mode is active. | `AgentStateSnapshot` | Experimental | Broad bootstrap snapshot; includes agent, session, tasks, timers, work items, waiting intents, external triggers, notifications, workspace, execution. |
| `GET` | `/agents/:agent_id/briefs` | Path `agent_id`; query `limit?`. | `BriefRecord[]` | Candidate stable | Defaults to `20`. |
| `GET` | `/agents/:agent_id/tasks` | Path `agent_id`; query `limit?`. | `TaskRecord[]` | Candidate stable for list; Gap for task detail APIs | Defaults to `50`; only active/recent task listing, no status/output/input/stop route yet. |
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
| `GET` | `/agents/:agent_id/events` | Path `agent_id`; query `before_seq?`, `after_seq?`, `limit?`, `order?`, `projection?`. | `EventsPageResponse` | Candidate stable route; experimental payload | `limit` defaults to the event window and is clamped. `order` is `asc` or `desc`. `projection` is `operator` or `local_debug`. |
| `GET` | `/agents/:agent_id/events/stream` | Path `agent_id`; query `after_seq?`, `limit?`, `projection?`; `Accept: text/event-stream` recommended. | SSE frames with JSON `StreamEventEnvelope` data. | Candidate stable route; experimental payload | SSE `id` is `event_seq`; SSE `event` is the raw audit event kind. |

Observed event envelope:

```json
{
  "id": "event-uuid",
  "event_seq": 42,
  "ts": "2026-05-24T00:00:00Z",
  "agent_id": "main",
  "type": "task_created",
  "projection": { "name": "operator", "raw_payload_included": true, "redactions": [] },
  "provenance": { "trust": "trusted_operator", "task_id": "task-..." },
  "payload": {}
}
```

Gap: the `operator` and `local_debug` projections currently both include the
raw payload. The redaction contract should be defined before this stream is
called stable.

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
| `POST` | `/control/agents/:agent_id/control` | `{ action, trust? }`; `action` is `start` or `stop`. | `{ ok }` | Candidate stable | `trust` is currently audit metadata only. |
| `POST` | `/control/agents/:agent_id/current-run/abort` | `{ run_id?, mode?, trust? }` | `{ ok, aborted, agent_id, run_id, mode, admission_context, provided_trust }` | Candidate stable | `mode` defaults to `stop_after_abort`; deprecated alias `pause_after_abort` is accepted. |
| `POST` | `/control/agents/:agent_id/create` | `{ template?, trust? }` | `AgentSummary` | Experimental | Path id names the created agent. |
| `POST` | `/control/agents/:agent_id/debug-prompt` | `{ text, trust? }` | `{ ok, agent_id, dump }` | Internal/debug | Dumps prompt rendering and should not be a stable automation API. |

### Tasks, work items, and timers

| Method | Path | Request body | Success response | Stability | Notes |
|--------|------|--------------|------------------|-----------|-------|
| `POST` | `/control/agents/:agent_id/tasks` | `CreateCommandTaskRequest` | `TaskRecord` | Candidate stable for creation; Gap for lifecycle management | `serde(deny_unknown_fields)` rejects legacy fields. |
| `POST` | `/control/agents/:agent_id/work-items` | `{ objective, trust? }` | `WorkItemRecord` | Experimental | Only creates/enqueues work items; no HTTP get/update/complete API yet. |
| `POST` | `/control/agents/:agent_id/timers` | `{ duration_ms, interval_ms?, summary?, trust? }` | `TimerRecord` | Candidate stable for creation; Gap for cancellation/list detail | `duration_ms` is required; `interval_ms` makes a repeating timer. |

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
| `trust` | `TrustLevel` or null | No | Defaults to `trusted_operator`; recorded in audit. |

### Workspace and model controls

| Method | Path | Request body | Success response | Stability | Notes |
|--------|------|--------------|------------------|-----------|-------|
| `POST` | `/control/agents/:agent_id/workspace/attach` | `{ path, trust? }` | `{ ok, agent_id, workspace_id, workspace_anchor }` | Candidate stable | `path` is converted into a workspace entry. |
| `POST` | `/control/agents/:agent_id/workspace/exit` | `{ trust? }` | `{ ok, agent_id }` | Candidate stable | Returns agent to default workspace behavior. |
| `POST` | `/control/agents/:agent_id/workspace/detach` | `{ workspace_id, trust? }` | `{ ok, agent_id, workspace_id }` | Candidate stable | `workspace_id` is trimmed before use. |
| `POST` | `/control/agents/:agent_id/model` | `{ model, reasoning_effort?, trust? }` | `{ ok, agent_id, model }` | Experimental | `reasoning_effort` must be `low`, `medium`, `high`, or `xhigh`. |
| `POST` | `/control/agents/:agent_id/model/clear` | `{ trust? }` | `{ ok, agent_id, model }` | Experimental | Clears the agent-level model override. |

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
| `TimerRecord` | `/agents/:id/timers`, timer creation, state snapshot | Repeating timer fields and status enum. |
| `BriefRecord` | `/agents/:id/briefs` | User-facing delivery vs internal traces. |
| `TranscriptEntry` | `/agents/:id/transcript` | Potential provider/tool internals and truncation policy. |
| `StreamEventEnvelope` | events page and SSE stream | Projection/redaction, provenance, payload versioning. |
| `RuntimeStatusResponse` | runtime readiness/status | Startup/runtime config surface and credential redaction. |
| `SkillInstallKind` | skills install | Tagged union variants and local/remote package semantics. |

## Detected contract gaps

1. **No generated route/schema inventory.** The current route list is hand
   extracted from `src/http.rs`. Add a test or generator that snapshots method,
   path, handler, request type, query type, and response contract.
2. **Event projection is not yet a redaction contract.** `operator` and
   `local_debug` currently both include raw payloads. This should be fixed or
   explicitly documented before event streams become stable.
3. **Task lifecycle APIs are incomplete.** HTTP can create and list tasks, but
   lacks task status/output/input/stop routes that correspond to runtime tool
   operations.
4. **WorkItem APIs are incomplete.** HTTP can create/enqueue work items and
   include them in state snapshots, but lacks list/get/update/pick/complete
   routes.
5. **Timer lifecycle APIs are incomplete.** HTTP can create and list timers, but
   lacks cancellation or detail routes.
6. **Deployment guidance still needs hardening.** The HTTP ingress trust/auth
   table is documented, but production-facing guidance should still describe
   when to use bearer mode, local mode, callback capabilities, and dedicated
   operator adapters.
7. **Tool schema is a separate API surface.** Built-in tool input/result schemas
   are not covered by this HTTP inventory and need their own stability
   inventory.

## Tracking issues

These follow-up issues are grouped under the
[`CLI/API Stability Contracts`](https://github.com/holon-run/holon/milestone/8)
milestone.

| Issue | Scope |
|-------|-------|
| [#1396](https://github.com/holon-run/holon/issues/1396) `api: add HTTP route and schema snapshot tests` | Generate or snapshot the route/schema inventory so contract drift is visible in CI. |
| [#1397](https://github.com/holon-run/holon/issues/1397) `api: define shared HTTP error envelope and status-code contract` | Normalize or document error JSON and HTTP status-code mapping. |
| [#1398](https://github.com/holon-run/holon/issues/1398) `api: define success envelope policy for control-plane responses` | Decide which route classes return `{ ok, ... }` envelopes versus direct records. |
| [#1399](https://github.com/holon-run/holon/issues/1399) `api: stabilize event replay and SSE projection contract` | Stabilize cursor behavior, SSE fields, projection, and redaction. |
| [#1400](https://github.com/holon-run/holon/issues/1400) `api: add task, WorkItem, and timer lifecycle endpoints` | Add or explicitly defer task, work-item, and timer detail/control endpoints. |
| [#1401](https://github.com/holon-run/holon/issues/1401) `api: document HTTP ingress trust and auth boundaries` | Publish a trust/auth table for public enqueue, webhooks, operator bindings, and callbacks. |
| [#1402](https://github.com/holon-run/holon/issues/1402) `api: inventory and version model-facing tool schemas` | Produce the separate inventory for built-in model-facing tool schemas. |

## Suggested next work

1. Add HTTP route/schema snapshot tests ([#1396](https://github.com/holon-run/holon/issues/1396)).
2. Define and test the common error envelope ([#1397](https://github.com/holon-run/holon/issues/1397)).
3. Decide per-route success envelope policy ([#1398](https://github.com/holon-run/holon/issues/1398)).
4. Stabilize event replay/SSE projection and redaction ([#1399](https://github.com/holon-run/holon/issues/1399)).
5. Add or explicitly defer task, work-item, and timer lifecycle endpoints ([#1400](https://github.com/holon-run/holon/issues/1400)).
6. Document HTTP ingress trust/auth boundaries ([#1401](https://github.com/holon-run/holon/issues/1401)).
7. Produce a separate tool-schema inventory for model-facing APIs ([#1402](https://github.com/holon-run/holon/issues/1402)).
