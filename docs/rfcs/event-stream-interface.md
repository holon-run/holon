# Event Stream Interface Design

## Summary

This document proposes a native Holon event stream for first-party clients such
as the built-in TUI.

The native stream should expose Holon's raw runtime events rather than a
separate server-side UI projection. First-party clients are expected to:

1. bootstrap from one aggregated snapshot endpoint
2. subscribe to the native raw event stream
3. maintain their own local materialized view from those events
4. reconnect from the last seen cursor
5. reset from snapshot when replay is no longer available

This keeps Holon explicit about its runtime model. The server publishes the
runtime event feed. The TUI adapts itself to that feed rather than requiring
Holon to maintain a second TUI-specific event contract.

## Problem

The current TUI experience depends on polling multiple endpoints and then
reconciling the results client-side. That has several costs:

- visible latency between runtime changes and UI updates
- duplicated client logic across status, transcript, and task polling
- no single replayable event surface for long-running daemon sessions
- difficult future integration with richer clients and protocol adapters
- pressure to shape the runtime surface around today's TUI layout

Holon should provide one native streaming surface that reflects daemon/runtime
state directly, while allowing clients to build projections appropriate to
their own UI.

## Goals

- provide one canonical raw event stream for Holon first-party clients
- reduce multi-endpoint polling in the TUI
- support long-running daemon sessions
- support reconnect with replay from a recent cursor
- support both HTTP and unix socket transports symmetrically
- keep the design aligned with current Holon runtime concepts
- leave room for future projection into protocols such as AG-UI and ACP

## Non-goals

- do not require `WorkItem` or `WorkPlan` to exist in the first version
- do not make AG-UI the first internal/public stream shape
- do not define a second server-side TUI-specific event family
- do not replace all existing snapshot endpoints immediately
- do not build a general-purpose durable event sourcing system
- do not require WebSocket for the first version

## Design Overview

The first version should use:

- one aggregated bootstrap snapshot endpoint
- one native SSE event stream
- one unified event envelope
- one recent-event replay buffer keyed by cursor
- one client-side projection model in the TUI

The intended client model is:

1. fetch current snapshot
2. initialize local projection from that snapshot
3. open event stream from the returned cursor
4. apply subsequent raw events incrementally
5. on disconnect, reconnect from the latest cursor
6. if the cursor is too old, refetch snapshot and resume from there

## Raw Event Principle

The native stream should expose Holon's raw runtime events.

That means:

- the `type` field is the runtime's original event kind
- the `payload` field is the original event payload
- the stream is not rewritten into TUI-friendly semantic categories on the
  server
- first-party clients build their own local projection from the raw feed

This matters because Holon is still evolving. A raw stream keeps the transport
stable while allowing the TUI, future web clients, and protocol adapters to
experiment with different projections.

## Transport

### Snapshot

Use a normal JSON endpoint:

`GET /agents/:id/state`

This returns the current aggregated state required to bootstrap a client-side
projection.

### Stream

Use Server-Sent Events:

`GET /agents/:id/events?since=<cursor>`

with:

- `Content-Type: text/event-stream`
- standard SSE `id:` field for cursor replay
- optional `Last-Event-ID` support

SSE is the preferred first transport because Holon's immediate problem is a
server-to-client event feed, not a bidirectional control channel.

### Transport Parity

The native stream must be available over both:

- HTTP
- unix socket HTTP

These transports are peers. First-party clients must not assume HTTP is
enabled. A local-only deployment may expose only the unix socket.

That means the client implementation must support:

- long-lived SSE over normal HTTP
- long-lived SSE over unix socket HTTP
- the same replay and reconnect behavior on both transports

## Why SSE First

SSE is the right first step because:

- Holon already has HTTP control surfaces
- TUI/web/CLI clients mainly need server-to-client updates
- SSE is simpler to debug and proxy than WebSocket
- reconnect and replay semantics map naturally onto SSE event ids
- unix socket and TCP can both carry the same HTTP/SSE contract

WebSocket can be added later if there is a strong need for a fully interactive
streaming control plane. It should not block the first version.

## Snapshot Endpoint

### Endpoint

`GET /agents/:id/state`

### Purpose

The snapshot endpoint is the bootstrap surface. It should give the client a
coherent current view without requiring the client to merge multiple polling
requests.

The snapshot should be complete enough to initialize the current TUI
projection. It should not assume the client can recover missing panels by
replaying arbitrary historical events.

### Requirements

- aggregated and self-consistent
- includes a replay cursor
- includes the current state for every panel the first-party TUI needs at
  bootstrap time
- stable enough that reconnect reset is straightforward

## Event Stream Endpoint

### Endpoint

`GET /agents/:id/events?since=<cursor>`

### Purpose

The event endpoint provides the incremental updates after snapshot bootstrap.

The stream is per-agent. It should emit Holon's raw events relevant to the
requested agent.

### Replay Behavior

- if `since` is present and still available in the recent-event buffer, replay
  events after that cursor
- if `since` is missing, stream only new events after connection establishment
- if `since` is too old and no longer replayable, the server should explicitly
  require a snapshot refresh

Replay failure must be explicit and deterministic. A client must know when it
has to discard its local projection and rebuild from `/state`.

## Event Envelope

Every stream event should use one canonical envelope.

### Suggested Envelope

```json
{
  "id": "evt_12346",
  "seq": 12,
  "ts": "2026-04-18T14:00:00Z",
  "agent_id": "default",
  "type": "task_status_updated",
  "payload": {}
}
```

### Fields

- `id`
  - stable event cursor used for replay and SSE `id:`
- `seq`
  - stream ordering hint
  - clients must not depend on this field for replay correctness
- `ts`
  - event timestamp
- `agent_id`
  - agent identity
- `type`
  - raw runtime event kind
- `payload`
  - raw event payload

### SSE Projection

The envelope should be projected to SSE as:

```text
id: evt_12346
event: task_status_updated
data: {"id":"evt_12346","seq":12,"ts":"2026-04-18T14:00:00Z","agent_id":"default","type":"task_status_updated","payload":{}}
```

## Raw Event Families

The native stream should surface the runtime's existing raw event kinds. It is
not necessary to define a second semantic event taxonomy for the TUI.

Important first-party event families currently include:

### Message ingress and queue

- `message_admitted`
- `message_enqueued`
- `message_processing_started`
- `continuation_trigger_received`
- `continuation_resolved`

### Session and lifecycle

- `agent_state_changed`
- `session_state_changed`
- `closure_decided`
- `control_applied`
- `runtime_service_shutdown_requested`

### Turn and model execution

- `provider_round_completed`
- `text_only_round_observed`
- `max_output_tokens_recovery`
- `turn_terminal`
- `runtime_error`

### Tasks and timers

- `task_created`
- `task_status_updated`
- `task_result_received`
- `task_requeued_after_restart`
- `timer_created`
- `timer_fired`
- `timer_fire_failed`

### Briefs and operator-visible outcomes

- `brief_created`

### Work items and planning

- `work_item_written`
- `work_item_turn_end_committed`
- `work_item_turn_end_commit_skipped`
- `work_plan_snapshot_written`

### Workspace and worktree state

- `workspace_attached`
- `workspace_entered`
- `workspace_exited`
- `worktree_entered`
- `worktree_exited`
- `worktree_created_for_task`
- `task_worktree_metadata_recorded`
- `worktree_retained_for_review`
- `worktree_auto_cleaned_up`
- `worktree_auto_cleanup_failed`
- `task_worktree_cleanup_already_removed`
- `task_worktree_cleanup_retained`
- `task_worktree_cleanup_failed`
- `task_worktree_branch_cleanup_retained`

### Waiting and callback flow

- `waiting_intent_created`
- `waiting_intent_cancelled`
- `callback_delivered`

### Optional/secondary runtime detail

- `tool_executed`
- `tool_execution_failed`
- `skill_activated`
- `system_tick_emitted`

The TUI does not need to render every raw event equally. It should classify
them into views such as:

- primary operator-facing timeline
- state panels
- raw event inspector/debug view

That classification belongs in the client projection layer, not in the stream
transport.

## Event Payload Strategy

The stream payload should remain raw and explicit.

That means:

- if a runtime event already contains a full current snapshot of some record,
  keep that full snapshot
- if a runtime event is primarily diagnostic, keep it diagnostic
- do not force all raw events into a patch protocol
- do not force all raw events into a user-facing message model

The goal is to preserve runtime clarity while still allowing the client to
derive richer projections.

## Server Buffer and Replay

The first version should use a recent-event replay buffer rather than durable
historical replay.

### Suggested behavior

- keep a bounded recent replay window
- support replay from a recent cursor
- if the cursor falls outside the retained window, require snapshot refresh

This is enough for:

- short disconnects
- TUI reconnect
- daemon clients that want near-real-time continuity

It is not intended to be a durable historical event log.

## Client Projection Model

The first-party TUI should maintain a local materialized view derived from:

- `/state` bootstrap snapshot
- subsequent raw events from `/events`

The projection should be responsible for:

- ordering and deduplication by cursor
- maintaining panel state
- deciding which raw events are primary operator-facing output
- deciding which raw events are debug/noise
- resetting itself when replay is no longer possible

This keeps the server transport simple and allows the TUI layout to evolve
without changing the native stream contract.

## Client Bootstrap Flow

The intended first-party client flow is:

1. call `GET /agents/:id/state`
2. initialize local projection from snapshot
3. extract `cursor`
4. connect to `GET /agents/:id/events?since=<cursor>`
5. apply raw events incrementally
6. on disconnect, reconnect from the last seen cursor
7. if replay fails, discard local projection, refetch snapshot, and continue

This should become the standard integration path for the TUI.

## Relationship to Existing Endpoints

The first version should not immediately remove existing polling endpoints.

Recommended rollout:

1. add `/state`
2. add `/events`
3. add first-party client support for HTTP and unix socket SSE
4. migrate the TUI to use snapshot plus stream projection
5. observe whether older polling endpoints still need to exist for compatibility

This keeps migration risk low.

## Status Surface Versus Bootstrap Surface

The native stream rollout introduces an intentional overlap between:

- agent-facing status endpoints such as `GET /agents/:id/status`
- projection bootstrap endpoints such as `GET /agents/:id/state`

That overlap should stay explicit rather than accidental.

### Phase 1 split

Phase 1 should treat these surfaces differently:

- `/status`
  - the concise agent-facing summary surface
  - intended for operator inspection, scripts, and future generic
    agent-inspection clients
  - should remain centered on one `AgentSummary`
- `/state`
  - the first-party projection bootstrap surface
  - intended for the TUI and later first-party projection clients that need a
    coherent local materialization starting point
  - may include additional slices that are not appropriate for the normal
    agent-facing summary contract

### Duplication rule

Phase 1 should allow one intentional duplication boundary:

- `/state.agent` should stay status-compatible with `/status`

That means:

- identity, lifecycle, closure, model, workspace attachment, and similar
  agent-summary fields may appear in both places
- this duplication is acceptable because first-party clients need one complete
  bootstrap payload after replay loss
- the duplication should be bounded to the embedded `agent` summary instead of
  letting `/state` become an unstructured copy of every existing endpoint

### Compatibility rule

Phase 1 should use different compatibility expectations:

- `/status`
  - stronger compatibility expectations for operator-facing and generic
    agent-facing inspection
- `/state`
  - allowed to evolve for first-party projection bootstrap completeness
  - should preserve `/state.agent` compatibility with `/status`
  - should not be treated yet as the long-term universal rich snapshot API for
    third-party clients

If Holon later needs a broader third-party rich snapshot contract, that should
be designed intentionally instead of silently promoting `/state` into that
role.

## TUI Integration Notes

The first-party TUI should consume the native raw stream directly.

That means:

- TUI uses snapshot bootstrap
- TUI subscribes to the native SSE stream over either HTTP or unix socket
- TUI maintains a local projection derived from raw events
- TUI panels and layout are allowed to evolve around the runtime stream
- TUI should keep a raw event inspector so internal/runtime events remain
  visible instead of being hidden by projection heuristics

The TUI should not depend on AG-UI in the first phase. AG-UI, if added later,
should be a projection built on top of the same native event model.

### Chat-first operator surface

Even after migrating to the native event stream, the built-in TUI should remain
chat-first.

That means:

- the primary surface is still the operator conversation with the agent
- prompt entry stays continuously available in the main layout
- the TUI should not replace the main conversation area with a permanent raw
  event timeline
- the TUI should not reserve a large fixed event pane below the conversation if
  that would push the composer farther away from the operator's focus

The event-stream migration changes the data source and projection model, not
the core operator interaction goal.

### Projection density

The TUI should derive at least three different projections from the same raw
event feed:

- durable conversation items
- ephemeral activity/progress items
- inspectable raw events

These projections have different retention and presentation rules.

### Durable conversation projection

The main conversation surface should keep only operator-relevant items that
need to remain visible as part of the ongoing discussion.

Typical durable items include:

- operator messages
- `brief_created`
- key coordination/system cards such as:
  - work item creation or meaningful status changes
  - entering or leaving an explicit waiting state
  - callback delivery that resumes meaningful work
  - runtime errors
  - turn terminal outcome

The main conversation projection should not become a generic rendering of every
runtime event.

### Ephemeral activity projection

Many runtime events are useful while a turn is active but should not stay in
the durable conversation history.

Typical ephemeral items include:

- `provider_round_completed`
- `text_only_round_observed`
- `tool_executed`
- `tool_execution_failed`
- lightweight task/workspace/worktree progress that is only relevant while the
  turn is in flight

These should be shown as transient activity or progress UI near the main
conversation, not as a permanent secondary history view.

When a new durable `brief_created` arrives, or when the turn reaches a terminal
state, the TUI may clear or collapse the corresponding ephemeral activity for
that turn into a short summary.

### Raw event inspector

The TUI should still provide an explicit raw event inspector for debugging and
runtime comprehension, but this should be an on-demand inspection surface
rather than the default main view.

This keeps the operator workflow focused on:

- conversation
- current runtime state
- lightweight in-flight activity

while still preserving access to the underlying raw facts when needed.

## Forward Compatibility

This design intentionally leaves room for future expansion.

When `WorkItem` and `WorkPlan` become more central, clients can present richer
views by consuming the raw event families already emitted for those records.

That should not require changing:

- transport
- cursor model
- bootstrap model
- base event envelope

Similarly, AG-UI and ACP adapters can later project from the native event model
without making this first version depend on either protocol.

## Open Questions

- exact `/state` bootstrap completeness for the first projection-based TUI
- replay window sizing and retention policy
- whether replay failure should be signaled only as HTTP status or also as a
  terminal SSE event before close
- whether the TUI should persist any local projection state between launches
- whether some low-value raw events should become opt-in stream filters later

## Draft `/state` Schema

This section proposes a more concrete projection bootstrap schema for
`GET /agents/:id/state`.

The goal is not to freeze every field immediately. The goal is to define one
aggregated shape that is coherent enough for a projection-driven TUI to
bootstrap from a single request.

### Top-level Shape

```json
{
  "agent": {},
  "session": {},
  "tasks": [],
  "transcript_tail": [],
  "briefs_tail": [],
  "timers": [],
  "work_items": [],
  "work_plan": null,
  "waiting_intents": [],
  "external_triggers": [],
  "workspace": null,
  "cursor": "evt_12345"
}
```

### Top-level Fields

- `agent`
  - stable agent-facing identity and lifecycle summary
  - should remain status-compatible with `GET /agents/:id/status`
- `session`
  - current daemon/runtime session state relevant to projection bootstrap
- `tasks`
  - current task snapshots relevant to the TUI
- `transcript_tail`
  - recent transcript entries
- `briefs_tail`
  - recent operator-facing briefs
- `timers`
  - current timer records when relevant
- `work_items`
  - current work item records when relevant
- `work_plan`
  - current work plan snapshot when present
- `waiting_intents`
  - current waiting intent state
- `external_triggers`
  - current callback delivery state
- `workspace`
  - current workspace/worktree summary needed by the TUI
- `cursor`
  - replay cursor for the subsequent event stream

The server does not need to expose all possible historical records here. It
does need to expose enough current state that a client can rebuild its local
projection after replay loss.

The server also does not need to claim that every field here belongs to the
general agent-facing status contract. In phase 1, `/state` is the bootstrap
aggregate, while `/status` remains the concise agent-facing summary surface.

## Implementation Work Breakdown

The expected rollout should be split into separate work items:

1. add first-party client support for HTTP and unix socket SSE
2. extend `/state` so projection bootstrap is complete enough for the TUI
3. add a TUI projection/reducer that consumes raw events incrementally
4. migrate the TUI runtime loop from polling to snapshot plus stream
5. redesign TUI panels/layout/content around the raw runtime stream

These should be tracked as separate issues so transport, bootstrap contract,
projection logic, and UI redesign can move with clear dependencies.
