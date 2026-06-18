# Projection query and subscription API design

This note designs a server-side projection surface for first-party UI clients.
It complements, but does not replace, the raw event stream described in
`docs/rfcs/event-stream-interface.md`.

The current TUI and web GUI both reduce raw runtime events into display items.
That keeps the raw runtime model explicit, but it makes every client own the
same merge, dedupe, hydration, and visibility policy. A projection API moves the
default UI path to a shared server-side reducer while preserving raw events for
audit, replay, diagnostics, and advanced tools.

## Goals

- Provide a default display surface that web and TUI can consume without
  understanding every runtime event kind.
- Keep raw events as the canonical ledger and debugging surface.
- Provide both query and subscription at the same semantic layer, so clients do
  not have to subscribe to both raw events and projection changes for normal UI.
- Make long-field boundaries explicit: projection items may include bounded
  previews and stable refs; full text/output/details still come from detail
  APIs or artifacts.
- Centralize display policy for visibility, dedupe, suppression, hydration
  state, and item replacement.

## Non-goals

- Do not remove `/agents/:id/events` or raw event streaming.
- Do not make projection items the source of truth for runtime state.
- Do not require every client to use the same visual renderer. The API provides
  semantic display items; web and TUI still render them in their native widgets.
- Do not embed full task output, full tool result JSON, full work-item plans, or
  full message/brief bodies in projection deltas.

## Layering

```text
append-only runtime ledger
        │
        ├── raw event query/stream
        │       - audit/debug/replay/developer tooling
        │       - exact event kind and payload
        │
        └── projection reducer
                │
                ├── projection query
                │       - bootstrap, pagination, refresh
                │
                └── projection subscription
                        - item-level deltas / invalidations
```

Normal UI flow:

```text
GET projection window
SUBSCRIBE projection changes for that scope
hydrate full details only when a user expands or opens an item
```

Debug/event-inspector flow:

```text
GET raw event page
SUBSCRIBE raw events
```

The key design rule is that a default UI subscribes to projection changes, not
to raw events. Raw events remain available for trace/debug modes.

## Projection scopes

Projection is scoped because different UI surfaces need different windows and
different aggregation levels.

Recommended first scopes:

- `agent_timeline`: one agent's operator-facing timeline.
- `work_item_timeline`: one work item's focused timeline, including related
  messages, briefs, waits, tasks, and completion events.
- `task_timeline`: one task's lifecycle and output summary timeline.
- `agent_roster`: compact per-agent activity/status cards.
- `work_queue`: current/open/queued/blocked/completed work-item display.
- `raw_debug_timeline`: optional projection of raw events into debug rows for
  clients that want a readable event inspector without implementing all labels.

The first implementation can start with `agent_timeline` plus `agent_roster`
because those cover the current web GUI session view and the TUI conversation
panel.

## Query API

### Agent timeline

```http
GET /agents/{agent_id}/projection/timeline?cursor={cursor}&limit=100&direction=backward&level=info
```

Response:

```json
{
  "scope": {
    "kind": "agent_timeline",
    "agent_id": "holon-pm"
  },
  "revision": "projrev:agent:holon-pm:12345",
  "source_event_seq": 12345,
  "items": [],
  "page": {
    "start_cursor": "projcur:...",
    "end_cursor": "projcur:...",
    "has_more_before": true,
    "has_more_after": false
  },
  "hydration": {
    "messages": [],
    "briefs": [],
    "tasks": [],
    "work_items": []
  }
}
```

Parameters:

- `cursor`: projection cursor, not raw event id. It identifies an item-window
  boundary.
- `limit`: maximum item count.
- `direction`: `backward` for history pagination, `forward` for catch-up.
- `level`: `info`, `verbose`, `debug`, or `trace`.
- `include_open`: optional boolean for appending active mutable items such as
  in-flight assistant progress or running commands.

### Work item timeline

```http
GET /work-items/{work_item_id}/projection/timeline?cursor={cursor}&limit=100
```

This returns the same item shape, scoped to one work item. The reducer includes
events linked by `work_item_id`, relevant messages/briefs, task/tool events that
ran under that work item, wait/resume events, and completion result refs.

### Projection detail hydration

Projection items carry refs for full details. Full content should be loaded
through existing or dedicated detail APIs:

- messages: `messages:batchGet` or equivalent message detail endpoint;
- briefs: brief detail endpoint by `brief_id`;
- tasks: `TaskStatus`, `TaskOutput`, and task output artifacts;
- tool executions: canonical tool execution evidence by `tool_execution_id`;
- work items: work-item detail endpoint and plan artifact refs.

Projection query may optionally include a bounded `hydration` block for common
above-the-fold data. This is a convenience cache, not a replacement for detail
APIs.

## Subscription API

Projection subscriptions use the same scope and visibility semantics as query.
SSE is sufficient for the first version:

```http
GET /agents/{agent_id}/projection/timeline/stream?after_revision={revision}&level=info
```

Events:

```json
{
  "type": "projection_delta",
  "scope": { "kind": "agent_timeline", "agent_id": "holon-pm" },
  "revision": "projrev:agent:holon-pm:12346",
  "source_event_seq": 12346,
  "ops": [
    { "op": "insert", "after": "item:...", "item": {} },
    { "op": "replace", "item_id": "item:...", "item": {} },
    { "op": "remove", "item_id": "item:..." }
  ]
}
```

Other subscription records:

- `projection_invalidated`: the reducer cannot produce a safe delta from the
  client's `after_revision`; client must refetch the current window.
- `projection_hydration_available`: optional notice that a previously
  placeholder item can now be hydrated.
- `projection_heartbeat`: keepalive, includes latest `source_event_seq`.

The client stores the latest projection `revision`, not just raw event seq.
`source_event_seq` is still included to correlate projection rows with raw event
inspection and to know which ledger events are covered.

## Projection item model

A projection item is stable enough for all first-party UIs but not tied to a
specific visual layout.

```json
{
  "id": "pitem:agent:holon-pm:brief:brief_123",
  "kind": "assistant_result",
  "title": "Result",
  "summary": "已完成并提交文档…",
  "body_preview": "已完成并提交文档…",
  "body_len": 128,
  "status": "completed",
  "visibility": "info",
  "created_at": "2026-06-16T10:00:00Z",
  "updated_at": "2026-06-16T10:00:02Z",
  "refs": {
    "agent_id": "holon-pm",
    "brief_id": "brief_123",
    "message_id": "msg_456",
    "task_id": null,
    "work_item_id": null,
    "tool_execution_id": null,
    "source_event_ids": ["event_1"],
    "source_event_seq": 12345
  },
  "children": [],
  "actions": [
    { "kind": "open_detail", "ref": "brief:brief_123" },
    { "kind": "show_raw_events", "event_seq": 12345 }
  ],
  "debug": {
    "source_event_kinds": ["brief_created"]
  }
}
```

Required fields:

- `id`: deterministic item id. Related raw events update the same item instead
  of creating duplicates.
- `kind`: UI semantic kind, not raw event kind.
- `summary`: bounded text safe for default lists.
- `visibility`: minimum level: `info`, `verbose`, `debug`, or `trace`.
- `refs`: stable ids and raw event correlation.

Recommended item kinds:

- `operator_message`
- `assistant_progress`
- `assistant_result`
- `tool_activity`
- `command_activity`
- `file_change`
- `task_activity`
- `wait_notice`
- `resume_notice`
- `work_item_card`
- `work_item_bookkeeping`
- `agent_status`
- `workspace_activity`
- `system_alert`
- `debug_event`

## Event-to-projection policy

The table below is the normative first-pass policy for currently used event
families. `Default item` is what normal `agent_timeline` should produce.
`Debug handling` describes how the event remains inspectable.

| Event kind/family | Default projection item | Visibility | Merge/dedupe and hydration policy | Debug handling |
| --- | --- | --- | --- | --- |
| `message_enqueued` | `operator_message` for operator-origin messages. | `info` | Use `message_id` as item id. Display hydrated message body or `text_preview`; never require event payload to contain full text. Ignore non-operator messages unless a specific UI scope needs them. | Raw event available; non-operator messages may become `debug_event`. |
| `message_admitted` | None by default. | `debug` | Admission duplicates `message_enqueued` for normal display. Promote only if admission fails or contains a distinct trust/security decision. | `debug_event` with origin/trust metadata. |
| `message_processing_started` | None by default; may update active run indicator. | `debug` | Treat as run correlation and activity reset, not a conversation row. | `debug_event`. |
| `turn_started` / `recovery_turn_started` | None by default; may update active run indicator. | `debug` | Merge with current run state. Do not show alongside operator message. | `debug_event`. |
| `operator_interjection_admitted` | `operator_message` or `system_alert` depending on payload. | `info` | If it represents a visible interjection, show one concise row using preview. Do not duplicate the original message item. | Raw event preserved. |
| `brief_created` | `assistant_result`. | `info` | Use `brief_id` as item id. Display `text_preview`; hydrate full markdown only in detail/expanded view. Replace/suppress matching `assistant_progress`. | Raw event preserved with slim payload. |
| `assistant_round_recorded` / `text_only_round_observed` | `assistant_progress`. | `verbose` | Use bounded preview. If later `brief_created` matches or supersedes it, remove or mark as superseded. Never treat as final result. | `debug_event` at debug level if needed. |
| `provider_round_completed` | None by default. | `debug` | Provider/model telemetry is not an operator timeline item. It may annotate a surrounding assistant/tool activity in debug view. | `debug_event`. |
| provider recovery/lineage events (`provider_failed_needs_recovery`, `checkpoint_recorded`, `turn_local_checkpoint_recorded`, `turn_local_compaction_applied`, `working_memory_updated`) | None by default, except failures may become `system_alert`. | `debug` | Keep normal UI result-oriented. Show only actionable or failed recovery facts at info/verbose. | `debug_event`. |
| successful `tool_executed` | `tool_activity`, `command_activity`, `file_change`, or hidden bookkeeping. | `verbose` | Use `tool_execution_id` as item id. Special-case `ExecCommand`, `ExecCommandBatch`, `ApplyPatch`, image/view tools, and work-item mutation tools. Store refs to full evidence; include only bounded command/result previews. | Full evidence through tool execution detail; raw event as `debug_event`. |
| `tool_execution_failed` | `system_alert` or failed `tool_activity`. | `info` for failures, `debug` details. | Use bounded error detail as failure evidence. Link `tool_execution_id` and related task/output refs. | Raw event plus tool detail. |
| `task_created` | Create/update `task_activity`. | `verbose` | Use `task_id` as item id. If task is command-backed, merge with command tool item when `tool_execution_id`/task handle links exist. | `debug_event`. |
| `task_status_updated` | Replace existing `task_activity`; update active task panels. | `verbose` | Do not append one row per status. Use replace delta to show queued/running/cancelling/completed state. | `debug_event`. |
| `task_result_received` | Complete `task_activity`; maybe append failure `system_alert`. | `verbose`/`info` on failure | Include `output_summary_preview` and output refs. Full output remains in `TaskOutput`/artifact. | `debug_event`. |
| command/child task failures (`command_task_runner_failed`, `command_task_result_enqueue_failed`, `supervised_child_task_recovery_failed`) | `system_alert`. | `info` | These are actionable runtime failures. Link task id and recovery refs. | Raw event preserved. |
| `task_input_delivered` | Update `task_activity` or hide. | `debug`/`verbose` | Show only when useful for interactive task timeline. Do not duplicate operator input text. | `debug_event`. |
| task worktree events (`task_worktree_metadata_recorded`, `task_worktree_cleanup_failed`, `worktree_created_for_task`) | `workspace_activity` or `system_alert` on failure. | `verbose`/`info` on failure | Keep path/branch previews bounded. Link workspace/worktree detail where available. | `debug_event`. |
| timers (`timer_created`, `timer_fired`, `timer_fire_failed`) | Usually none; failure becomes `system_alert`. | `debug`/`info` on failure | Timer events feed wait/resume item state. Do not show every timer layer by default. | `debug_event`. |
| waits (`wait_condition_registered`, `waiting_intent_created`, `wait_conditions_resolved`) | `wait_notice` then `resume_notice`/replace. | `info` | Use wait id/work-item/agent scope as item id. Replace active waiting row when resolved instead of appending every layer. | Raw wait lifecycle visible in debug. |
| continuation/callback events (`callback_delivered`, `continuation_trigger_received`, `continuation_resolved`) | `resume_notice` if it changes user-visible state. | `info`/`verbose` | Merge into the active wait/resume item. Avoid separate callback, trigger, and resolution rows unless debug. | `debug_event`. |
| `work_item_written` | `work_item_card` for create/complete/block/unblock or `work_item_bookkeeping` for minor mutations. | `info` for major transitions, `verbose` for bookkeeping | Use `work_item_id` plus transition as item id. Display `objective_preview`/`result_summary_preview`; full plan/todo/result via detail/brief refs. | Raw lifecycle event preserved. |
| `work_item_picked` / `work_item_focus_released` | Update work queue/current-work panels; optional `work_item_bookkeeping`. | `verbose` | Avoid noisy default rows unless focus change is useful in the timeline scope. | `debug_event`. |
| work-item delegation/binding/plan events (`work_item_delegation_created`, `work_item_delegation_completed`, `work_item_turn_binding_released`, `work_item_plan_artifact_refreshed`) | `work_item_bookkeeping` or hidden. | `verbose`/`debug` | Show delegation completion if it affects user-facing progress. Hide artifact refresh unless requested. | `debug_event`. |
| `agent_state_changed` / `state_changed` / legacy `session_state_changed` | Update agent panels, not timeline. | `debug` | Treat as state-sync input for roster/current status. Do not create conversation rows. `session_state_changed` remains legacy replay fallback. | `debug_event`. |
| `closure_decided` | Update active run status; maybe `system_alert` on failed/interrupted closure. | `verbose`/`info` on abnormal | Normal completion is usually represented by `brief_created`. Avoid duplicate "completed" row. | `debug_event`. |
| control events (`control_request_admitted`, `control_applied`) | `system_alert` or `agent_status` only when visible to operator. | `verbose`/`debug` | Normal control plumbing is hidden; security/trust outcomes can be visible. | `debug_event`. |
| operator delivery/mirror events (`operator_delivery_completed`, `operator_notification_mirror_failed`, `target_operator_boundary`) | Usually none; mirror failure becomes `system_alert`. | `debug`/`info` on failure | Delivery success is not a timeline item. | `debug_event`. |
| workspace/worktree events (`workspace_*`, `worktree_auto_cleanup_failed`, `worktree_created_for_task`) | `workspace_activity` or `system_alert` on failure. | `verbose`/`info` on failure | Use compact names/branches; full metadata by detail refs. | `debug_event`. |
| scheduler/diagnostic/recovery events (`scheduler_decision`, `scheduler_diagnostic`, lineage/retry/context events) | None by default. | `debug` | Suppress from normal timeline. | `debug_event` / raw event inspector. |
| storage/test/bootstrap events (`db_canonical_*`, `live_event`, `test_event`, `legacy_chat_event`) | None by default. | `trace` | Internal only unless explicitly viewing raw traces. | Raw trace only. |

Unknown event kinds should not become noisy `info` rows. The reducer should
default them to `debug_event` with `visibility=debug` and a compact summary.
This keeps development inspectability without leaking internal vocabulary into
the normal timeline.

## Reducer semantics

### Deterministic item ids

Projection item ids should be derived from stable runtime ids:

- operator message: `message:{message_id}`;
- assistant result: `brief:{brief_id}`;
- tool execution: `tool:{tool_execution_id}`;
- task: `task:{task_id}`;
- work item transition: `work-item:{work_item_id}:{transition}:{event_seq}`;
- active wait: `wait:{wait_id}` or `wait:{agent_id}:{work_item_id}:{kind}`;
- raw debug fallback: `event:{event_id}`.

Stable ids allow subscriptions to use `replace` instead of append-only noise.

### Replacement and suppression

The reducer should explicitly support:

- `insert`: a new semantic item appears;
- `replace`: a mutable item changes state, such as running command completion;
- `remove`: a progress/debug item is superseded by a final result;
- `annotate`: optional child evidence is attached to an existing item.

Important suppression rules:

- `assistant_progress` is removed or marked superseded when a matching
  `assistant_result` arrives.
- successful work-item mutation tools are hidden when the lifecycle event
  already produced the visible item.
- task status updates replace one task item, not append a row per status.
- wait/timer/callback/continuation layers merge into one wait/resume item.
- normal closure completion is hidden when a final brief exists.

### Visibility

Projection visibility is the minimum level at which an item appears:

- `info`: operator messages, final briefs, failures, waits needing attention,
  major work-item changes;
- `verbose`: tools, commands, file changes, task activity, work-item
  bookkeeping, workspace activity;
- `debug`: raw runtime internals, state sync, provider telemetry, scheduler and
  diagnostic events;
- `trace`: exact raw event pages.

The server may return higher-visibility items when the client asks for
`level=debug`, but it should not change the meaning of `kind`.

## Web GUI update model

The web GUI should move its default session timeline from:

```text
event pages + live raw event stream
  -> session-reducer.ts
  -> timeline items
```

to:

```text
projection query + projection subscription
  -> projection item store
  -> React rendering
```

Recommended web state:

- `projectionItemsById`: normalized item store.
- `projectionOrderByScope`: ordered item ids per timeline scope.
- `projectionRevisionByScope`: latest applied revision.
- `hydrationCache`: message/brief/task/tool/work-item details keyed by refs.
- `rawEventsBySeq`: retained only for debug/event inspector, not the default
  timeline reducer.

Web bootstrap:

1. Query `agent_roster` and the selected `agent_timeline`.
2. Render projection items immediately using previews.
3. Hydrate visible message/brief details in batch.
4. Subscribe to the same projection scopes with `after_revision`.
5. Apply `insert`/`replace`/`remove`; on `projection_invalidated`, refetch the
   current window.

Web rendering changes:

- `TimelineItem` renders server `kind` directly.
- Existing special cases for `message_enqueued`, `brief_created`,
  `tool_executed`, work-item tools, and task events move behind the server
  projection reducer.
- The raw event stream remains available behind a debug tab or "show raw
  events" action from a projection item.
- Roster `lastBrief` should come from `agent_roster` projection or brief detail,
  not from assuming `brief_created.payload.text` exists.

## TUI update model

The TUI should keep its renderer and keyboard/display-level behavior, but stop
needing to classify every raw event for the default conversation view.

TUI bootstrap:

1. Query `agent_timeline` at the configured display level.
2. Convert projection items into `ConversationCell`s.
3. Query `work_queue`/`agent_roster` projection scopes for side panels.
4. Subscribe to projection changes for active scopes.

TUI subscription handling:

- `insert`: append/insert a rendered cell.
- `replace`: update the existing cell in place when possible.
- `remove`: hide superseded progress cells.
- `projection_invalidated`: refetch the visible window and preserve scroll
  position by item id where possible.

TUI raw event usage after migration:

- debug/trace display level can still open a raw event inspector;
- state-sync fallback can remain during migration, but the normal conversation
  should render projection items;
- existing `src/presentation.rs` reducer can become the first server-side
  projection reducer implementation, then TUI can consume the API instead of
  linking equivalent logic directly.

## Migration plan

1. Define the shared projection item schema and scope/revision envelope.
2. Implement server-side `agent_timeline` query by extracting the existing TUI
   presentation reducer policy into a reusable runtime projection module.
3. Add projection subscription that consumes raw event append notifications and
   emits item deltas for `agent_timeline`.
4. Move web GUI default timeline to projection query/subscription while keeping
   raw event pages for debug.
5. Move TUI conversation panel to projection query/subscription, preserving its
   renderer and display-level controls.
6. Add `work_queue` and `agent_roster` projections so side panels no longer
   infer state from raw event fallthrough.
7. Keep raw event stream tests and add projection golden tests:
   - operator message hydration;
   - brief preview/full hydration;
   - assistant progress superseded by brief;
   - command start/result replacement;
   - task lifecycle replacement;
   - work-item mutation tool suppression;
   - wait/timer/callback merge;
   - unknown event becomes debug-only.

## Open design choices

- Whether projection query should be backed by a persisted projection table or
  computed from recent events plus current state. The first version can compute
  on demand; persistence becomes useful once pagination across long history is
  required.
- Whether `projection_delta` should support fine-grained child operations or
  only whole-item replacement. Whole-item replacement is simpler and likely
  enough for the first version.
- Whether raw event `max_level` filtering should share the same visibility enum
  as projection items. The concepts are aligned, but replay authorization and
  display visibility must stay separate.

## Summary

Projection query/subscription is worthwhile if it is treated as the default UI
view layer, not as a replacement for raw events. The event ledger remains the
fact layer. The projection reducer becomes the shared product layer that turns
events, state, and detail refs into stable display items. Web and TUI then
subscribe to one semantic surface for normal UI and use raw events only for
debug, trace, and advanced inspection.
