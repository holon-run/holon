---
title: RFC: Operator Display Levels and Event Presentation
date: 2026-05-07
status: implemented
---

# RFC: Operator Display Levels and Event Presentation

## Summary

Holon should separate raw runtime audit events from operator-facing presentation.

The raw event stream remains the canonical runtime record. Operator surfaces
such as the TUI, future web clients, remote operator views, AgentInbox bridges,
and notification adapters should render those events through shared display
levels instead of showing raw event kinds directly.

The proposed display levels are:

- `info`: result-oriented operator view
- `verbose`: compact activity view
- `debug`: detailed execution view
- `trace`: raw audit/event inspection, reserved for event inspectors such as
  `/events`

Only `info`, `verbose`, and `debug` should be main operator conversation display
modes. `trace` is not a normal conversation mode; it is an inspection surface.

Clarification:

- "Compact activity view" describes the presentation density of `verbose`, not
  the event taxonomy and not the raw event kind names.
- TUI and server surfaces should share one event-level contract:
  `info | verbose | debug`.
- Presentation reducers may render the same eligible activity differently at
  different display levels. For example, an ApplyPatch activity can be a compact
  file summary at `verbose` and a bounded diff with diagnostics at `debug`.
- `/events?max_level=...` is an event eligibility filter over raw event
  envelopes. It does not by itself produce compact presentation items.
- Unfiltered raw trace inspection remains a separate surface.

## Problem

Holon currently has a human-facing event presentation layer, but the operator
experience still leaks too much runtime vocabulary.

Examples include:

- `Process started`
- `Tool executed: ExecCommand`
- `Provider round 218: model=model; stop=unknown; tokens unavailable; tools=0`
- `Assistant requested tools: ExecCommand`
- long `Work item Open: ...` rows that repeat internal state rather than explain
  what changed

These rows are technically derived from runtime events, but they are not useful
to most operators.

The deeper issue is that a raw event and an operator-facing item are different
things.

A raw event is an append-only fact. It may be small, internal, redundant, or
only meaningful when combined with neighboring events.

An operator-facing item should answer one of these questions:

- what result did the agent produce?
- what is the agent doing now?
- what changed that the operator should care about?
- what needs operator attention?
- what failed?
- what low-level detail is useful for debugging?

Those questions cannot be answered by rendering each raw event independently.
Some events must be hidden, some must be merged, some must be attached as
metadata, and some must only appear in trace inspection.

This is not only a TUI issue. Any future operator surface will need the same
presentation contract.

## Goals

- define shared display levels for operator-facing surfaces
- keep raw audit events separate from operator-facing presentation
- make `info`, `verbose`, and `debug` usable across TUI and non-TUI surfaces
- reserve `trace` for raw event inspection
- define how common event families should be shown, hidden, merged, or attached
  as metadata
- prevent provider/runtime telemetry from overwhelming the operator conversation
- make `verbose` a compact activity timeline that is useful during normal
  operator supervision
- make `debug` more detailed than `verbose` when useful, including full commands
  and full patch diffs
- keep event eligibility separate from display rendering so one activity can be
  compact at `verbose` and expanded at `debug`

## Non-goals

- do not remove or weaken the raw audit event stream
- do not make `/events` prettier at the expense of losing raw data
- do not require every event to become a visible conversation row
- do not make display level `debug` equivalent to raw `trace`
- do not change provider, tool, or work item runtime semantics in this RFC
- do not define a final visual design for any one UI implementation

## Display Levels

### `info`

`info` is the result-oriented operator view.

It should show only:

- operator input and durable operator-facing responses
- final briefs and completed work
- errors, interruptions, and failures
- waits or approvals that require operator attention
- important configuration or runtime changes that affect the operator's current
  work

It should not show assistant intermediate text. Intermediate assistant text is
progress, not result.

It should not show ordinary tool calls, provider rounds, process starts, command
completion, work item open updates, or internal state changes.

### `verbose`

`verbose` is the normal activity view.

It should show operator-understandable activity rendered at compact density:

- assistant text is visible
- command/tool/file activity is visible as compact human-readable activity rows
- related lifecycle events are merged into one item when possible
- user-relevant child-agent or task results are visible as high-level activity
  rows
- work item changes are shown only when they explain the agent's visible posture
  or operator-facing result
- context management and recovery events are shown only when they explain a
  visible delay or continuation

`verbose` should not show runtime bookkeeping by default. Low-level callback
delivery, timers, scheduler decisions, continuation resolution, routine
work-item writes, provider telemetry, transport delivery bookkeeping, and state
sync events belong in `debug` or `trace` unless they directly explain an
operator-visible state.

External wake and resume reasons are not routine bookkeeping when they answer
"why did the agent start working again?". A callback or timer may therefore
produce two different presentation contributions:

- a low-level delivery/lifecycle item for `debug`
- a reduced operator item for `verbose` or `info`, such as "External event
  received: PR review submitted" or "Timer fired; resumed waiting task"

### `debug`

`debug` is the detailed operator view.

It should include everything in `verbose`, plus curated details useful for
debugging:

- full or bounded shell command output details
- full or bounded ApplyPatch diffs
- command cwd, duration, and exit status
- tool input/output summaries
- provider model, stop reason, token usage, and round number when available
- work item ids and task ids when helpful
- debug-only runtime event families such as callbacks, timers, scheduler,
  continuation, context, memory, delivery, and lifecycle bookkeeping

`debug` should still be a curated operator view. It should not dump every raw
runtime event into the conversation. Raw payload inspection belongs to `trace`.

### `trace`

`trace` is raw event inspection.

It is reserved for surfaces such as `/events`, local debug logs, or future event
inspectors.

`trace` may show raw event kinds, raw payloads, redaction markers, provenance,
sequence ids, and replay metadata.

`trace` is not a main conversation display level.

## Presentation Pipeline

The display contract has two layers.

Layer 1 classifies raw events for eligibility:

```text
raw event -> EventLevel(info | verbose | debug)
```

Layer 2 reduces eligible events into operator activities and renders those
activities for the requested display level:

```text
eligible events -> PresentationItem
PresentationItem + display level -> compact | expanded | diagnostic rendering
```

The presentation pipeline should therefore have four stages:

1. raw event ingestion
2. event-level classification and normalized field extraction
3. event reduction into operator items
4. display-level rendering

The existing `present_operator_event` function roughly covers stage 2. It should
not be treated as the final rendering contract.

Operator surfaces should render reduced operator items, not raw events.

Server and TUI code should not define competing level semantics. A server event
filter, a server-provided operator projection, and the TUI presentation reducer
should all use the same `info | verbose | debug` event-level classification.
Only the presentation layer decides how much detail to show for a reduced item.

### Event Classification

Each event should first be classified into one event level:

- `info`: result, durable operator-facing response, required attention, or
  operator-relevant failure
- `verbose`: agent work activity that an operator can understand without runtime
  internals
- `debug`: runtime, diagnostic, telemetry, or bookkeeping detail

After level classification, each event should be assigned one of these
presentation actions:

- `show`: render as its own item
- `hide`: do not render in main conversation
- `merge`: combine with related events into one item
- `attach`: attach as metadata to a nearby item
- `trace_only`: show only in raw event inspectors

Event level controls eligibility. Presentation action controls how an eligible
event contributes to operator-facing items. An eligible event may still be
hidden in a specific presentation when it only exists to enrich a neighboring
item.

### Initial Event Level Map

This table assigns the initial event level for current event families. The
level is the raw event eligibility level before presentation reduction. The
later Event Family Policy section still decides whether an eligible event is
shown, hidden, merged, attached, or left to trace inspection.

| Event family | Events | Initial level |
| --- | --- | --- |
| Operator-facing briefs and notifications | `operator_notification_requested`, `brief_created` | `info` |
| Operator input | `operator_interjection_admitted` | `info` |
| Message aborts and failures | `message_processing_aborted` when it explains an abort or failure | `info` |
| Message queue and admission plumbing | `message_enqueued`, `message_admitted`, `message_processing_started`, `turn_started` | `trace_only` |
| Assistant text progress | `assistant_round_recorded`, `text_only_round_observed` with assistant text | `verbose` |
| Tool-only assistant rounds | `assistant_round_recorded` with only tool calls | `verbose`, but normally merged into the related tool item |
| Provider telemetry | `provider_round_completed` | `debug` |
| Command and tool lifecycle | `process_execution_requested`, `tool_executed`, `tool_execution_failed`, `truncated_mutation_tool_call_rejected` | `verbose`; failures may promote to `info` when operator-relevant |
| User-relevant work item changes | `work_item_written`, `work_item_picked`, `work_item_enqueue_requested`, `missing_current_work_item_before_wait` when they change visible posture, complete work, or explain a failure/wait | `verbose`; created/completed lifecycle cards, failures, and operator waits may promote to `info` |
| Routine work item bookkeeping | `work_item_turn_end_committed`, `work_item_turn_end_commit_skipped`, stale-reminder and wait-intent cleanup events, routine `work_item_written` writes | `verbose` as visually distinct bookkeeping rows; raw payload details remain `debug`/trace |
| Delegated work and tasks | `work_item_delegation_created`, `work_item_delegation_completed`, `task_created`, `task_status_updated`, `task_result_received`, `task_child_spawned`, `task_input_delivered` | `verbose`; final child results/failures may promote to `info` |
| Task supervision diagnostics | `task_create_requested`, `supervised_child_task_monitor_reattached`, `supervised_child_task_recovery_failed`, `command_task_runner_failed`, `command_task_running_persisted`, `command_task_result_enqueue_failed` | `debug`; failures may promote to `info` |
| Waits that define visible posture | `waiting_intent_created`, `waiting_intent_cancelled` when the wait is operator-visible | `verbose`; waits requiring operator input are `info` |
| External wake and resume reasons | `callback_delivered`, `timer_fired`, `continuation_trigger_received` when they explain why the agent resumed or why an external wait completed | `verbose`; promote to `info` when the wake requires attention or materially changes visible posture |
| Callback, timer, stale-wait lifecycle | low-level `callback_delivered`, `timer_create_requested`, `timer_created`, `timer_fired`, `timer_fire_failed`, `stale_waiting_intents_cancelled` details | `debug`; failures may promote to `info` |
| Workspace and worktree activity | `workspace_*`, `worktree_*`, `task_worktree_*` events when they are part of user-understandable work activity | `verbose`; retained worktrees and cleanup failures may promote to `info` |
| Workspace and worktree requests or routine metadata | request-only or metadata-only workspace/worktree events | `debug` or `trace_only` |
| Skill install/uninstall | `skill_installed`, `skill_uninstalled` | `info` |
| Skill activation | `skill_activated` | `verbose` when it explains behavior; otherwise `debug` |
| Agent and model configuration | `agent_created`, model override set/clear events | `info` when user-visible; otherwise `verbose` |
| State sync | `agent_state_changed`, `state_changed`, `session_state_changed` | `trace_only` unless needed for diagnostics |
| Control results | `current_run_aborted`, `runtime_service_shutdown_requested`, user-visible `control_applied` | `info` |
| Control, wake, continuation, closure, scheduler, and tick bookkeeping | `control_request_admitted`, routine `control_applied`, `wake_requested`, `continuation_trigger_received`, `continuation_resolved`, `closure_decided`, `system_tick_emitted`, `system_tick_suppressed` | `debug` or `trace_only` |
| Context, memory, and recovery | context budget, compaction, checkpoint, episode memory, working memory, and recovery events | `debug`; hard limits/failures may promote to `info` |
| Operator delivery bookkeeping | `operator_delivery_submitted`, `operator_delivery_completed`, `operator_transport_binding_upserted` | `debug` or `trace_only` |
| Operator delivery failures | `operator_notification_mirror_failed` and actionable delivery failures | `info` |
| Unknown or new raw event kinds | any event not classified above | `trace_only` until deliberately classified |

### Event Reduction

Some event families are only meaningful after reduction.

Examples:

- `assistant_round_recorded` with tool calls should be merged with the subsequent
  tool or command item instead of producing `Assistant requested tools: ...`
- `process_execution_requested` and `tool_executed` should become one command or
  tool lifecycle item; `tool_executed` should carry only bounded metadata and a
  `tool_execution_id` for full output lookup
- `task_created`, `task_status_updated`, and `task_result_received` should become
  one task lifecycle item when they refer to the same task; full task output
  should be fetched from task APIs, not audit payloads
- repeated `work_item_written` events should become work item deltas
- `provider_round_completed` should usually attach telemetry to the nearest
  assistant/tool cycle rather than render as a standalone row

### Display-Level Rendering

The final renderer should receive the display level.

A single reduced item may render differently by level.

For example, a command item may render as:

`verbose`:

```text
Command finished: cargo test provider_contract_error_fallback (42s)
```

`debug`:

```text
Command finished
cwd: /repo/holon
exit: 0
duration: 42.1s
command:
cargo test provider_contract_error_fallback --all-targets
```

Another example is a file mutation item:

`verbose`:

```text
Modified 2 files: src/a.rs, src/b.rs (+12 -4)
```

`debug`:

```text
ApplyPatch succeeded
files:
- src/a.rs (+8 -3)
- src/b.rs (+4 -1)
diff:
<bounded diff>
diagnostics: none
```

## Event Family Policy

This section defines the initial policy for common event families. It is a
presentation contract, not a storage contract.

### Operator Attention And Briefs

Events:

- `operator_notification_requested`
- `brief_created`

Policy:

- `info`: show
- `verbose`: show
- `debug`: show with route, boundary, work item, or delivery metadata when useful
- `trace`: raw payload

Rationale:

These events are explicitly operator-facing.

### Operator Input And Messages

Events:

- `operator_interjection_admitted`
- `message_enqueued`
- `message_admitted`
- `message_processing_started`
- `message_processing_aborted`
- `turn_started`

Policy:

- `operator_interjection_admitted`: show as operator input in all main levels
- `message_processing_aborted`: show only when it explains an abort or
  failure
- all other message plumbing events: hide in main levels; trace only; message
  lifecycle payloads carry ids and provenance, not full message bodies

Rationale:

The operator should see their own message, not the queue machinery around it.

### Assistant Rounds

Events:

- `assistant_round_recorded`
- `text_only_round_observed`

Policy:

- `info`: hide
- `verbose`: show only assistant text; hide tool-only and empty rounds
- `debug`: show assistant text; tool-only rounds may be shown only if they cannot
  be merged with a tool item
- `trace`: raw payload

Rationale:

Intermediate assistant text is progress. It is not part of the result-oriented
`info` level.

Tool-only assistant rounds are better expressed as the actual tool or command
activity.

### Provider Rounds

Events:

- `provider_round_completed`

Policy:

- `info`: hide
- `verbose`: hide by default
- `debug`: attach valid telemetry to the relevant assistant/tool item; render as
  a standalone item only when it explains an error, retry, recovery, or unusual
  latency
- `trace`: raw payload

A provider round with missing model, missing stop reason, and unavailable token
usage should not render in the main conversation.

Rationale:

Provider rounds are telemetry, not operator progress.

### Tool And Command Execution

Events:

- `process_execution_requested`
- `tool_executed`
- `tool_execution_failed`
- `truncated_mutation_tool_call_rejected`

Payload policy:

- `tool_executed` carries command/tool preview, status, duration, summary, and
  `tool_execution_id`; full tool input/output belongs in `tool_executions`

Policy:

- `info`: show only failures or operator-relevant final actions
- `verbose`: merge command/tool start and completion into human-readable activity
  items with compact command, status, and short output summaries
- `debug`: show full or bounded command/tool details, including cwd, duration,
  output artifact references, ApplyPatch diagnostics, and bounded diffs
- `trace`: raw payload

Examples:

`verbose`:

```text
Running command: gh issue view 968 ...
Command finished: gh issue view 968 ... (1s)
```

`debug`:

```text
Tool: ExecCommand
cwd: /repo/holon
exit: 0
duration: 1.2s
command:
gh issue view 968 --repo holon-run/holon --json title,milestone,comments
```

Rationale:

The operator cares about what the agent did, not that a generic tool event was
emitted.

### Work Items

Events:

- `work_item_written`
- `work_item_picked`
- `work_item_enqueue_requested`
- `work_item_turn_end_committed`
- `work_item_turn_end_commit_skipped`
- `work_item_stale_reminder_injected`
- `work_item_stale_reminder_skipped`
- `work_item_waiting_intents_cancelled`
- `missing_current_work_item_before_wait`

Policy:

- `info`: show WorkItem tracking started, completed work items,
  WorkItem-scoped waits, and WorkItem-related failures. Operator-input waits
  are action-required; external/task waits are result-level posture changes.
- `verbose`: show WorkItem lifecycle/activity and visually distinct
  bookkeeping rows, including routine writes, picks, focus release, turn-end
  commits/skips, stale reminders, and wait cleanup
- `debug`: show state, ids, objective, result summary, and todo deltas
- `trace`: raw payload

Do not render long `Work item Open: ...` rows in the main conversation.

Rationale:

A work item record is internal state. The operator needs a change summary.

### Delegated Work And Tasks

Events:

- `work_item_delegation_created`
- `work_item_delegation_completed`
- `task_created`
- `task_status_updated`
- `task_result_received`
- `task_child_spawned`
- `task_input_delivered`
- `task_create_requested`
- `supervised_child_task_monitor_reattached`
- `supervised_child_task_recovery_failed`
- `command_task_runner_failed`
- `command_task_running_persisted`
- `command_task_result_enqueue_failed`

Payload policy:

- task lifecycle events carry task id, status, summary, and small terminal
  metadata; full detail and command output belong in task storage/output APIs

Policy:

- `info`: show task/delegation completion, failures, and child-agent results
- `verbose`: show high-signal lifecycle changes and child-agent activity
  summaries, not every status transition
- `debug`: show ids, workspace mode, task kind, and status transitions
- `trace`: raw payload

Task lifecycle events with the same task id should be reduced into one task
activity item when possible.

### Waiting, Timers, External Events, And Resume Reasons

Events:

- `waiting_intent_created`
- `waiting_intent_cancelled`
- `stale_waiting_intents_cancelled`
- `callback_delivered`
- `timer_create_requested`
- `timer_created`
- `timer_fired`
- `timer_fire_failed`
- `continuation_trigger_received`

Policy:

- `info`: show active waits requiring operator attention, external waits that
  define the agent's current posture, external wakes that require attention, and
  failures
- `verbose`: show waits only when they explain visible posture or continuation;
  show external wake or timer resume reasons when they explain why the agent
  started working again; hide routine callback delivery, timer firing, and
  stale-wait cleanup when they do not produce a user-relevant continuation
- `debug`: show ids, source, deadline, and cancellation reason
- `trace`: raw payload

Low-level callback delivery, timer firing, and wait-intent cleanup are runtime
lifecycle events. They should not appear in compact activity as raw plumbing
rows. However, the presentation reducer should emit a compact operator item when
the same event explains a visible continuation.

Examples:

`verbose`:

```text
External event received: PR review submitted; resuming agent
Timer fired: resumed scheduled follow-up
```

`debug`:

```text
callback_delivered
source: github
subscription: pr-review
wait_id: wait_...
callback_id: cb_...
matched_continuation: true
```

Rationale:

The operator should not need transport details, but they do need a short reason
when an agent resumes without a new operator message.

### Workspace And Worktree

Events:

- `workspace_attach_requested`
- `workspace_attached`
- `workspace_entered`
- `workspace_exit_requested`
- `workspace_exited`
- `workspace_detach_requested`
- `workspace_detached`
- `workspace_used`
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

Policy:

- `info`: show retained worktrees, cleanup failures, and operator-relevant
  workspace changes
- `verbose`: show workspace/worktree changes only when they are part of
  user-understandable work activity, such as creating an isolated coding
  worktree or retaining a worktree for review
- `debug`: show paths, branches, roots, and task ids
- `trace`: raw payload

Request events are usually trace-only unless they fail or explain a later
operator-visible state.

### Skills

Events:

- `skill_activated`
- `skill_installed`
- `skill_uninstalled`

Policy:

- `info`: show install/uninstall results; hide activation unless user-visible
- `verbose`: show `Loaded skill: <name>` when activation explains behavior
- `debug`: show scope, path, and load reason
- `trace`: raw payload

### Agent And Model Configuration

Events:

- `agent_created`
- `agent_model_override_requested`
- `agent_model_override_set`
- `agent_model_override_clear_requested`
- `agent_model_override_cleared`
- `agent_state_changed` (lightweight state sync)
- `state_changed`
- `session_state_changed` (legacy replay only)

Payload policy:

- model override events carry model refs and pending/active status only; full
  model state should be read from state snapshots

Policy:

- `info`: show created agent and final model changes
- `verbose`: show model switching activity only when user-visible
- `debug`: show requested/resolved model details
- state sync events: main conversation hidden; `/state` or trace only

### Control, Continuation, And Closure

Events:

- `control_request_admitted`
- `control_applied`
- `current_run_aborted`
- `wake_requested`
- `continuation_trigger_received`
- `continuation_resolved`
- `closure_decided`
- `system_tick_emitted`
- `system_tick_suppressed`
- `runtime_service_shutdown_requested`

Policy:

- `info`: show aborts and shutdown requests; hide routine control plumbing
- `verbose`: show control results only when they change visible behavior; hide
  ordinary wake, continuation, closure, scheduler, and tick bookkeeping
- `debug`: show continuation/closure/scheduler details when useful for
  diagnosing agent posture
- `trace`: raw payload

Routine system ticks and continuation bookkeeping should not appear in main
conversation display modes.

### Context, Memory, And Recovery

Events:

- `debug_prompt_requested`
- `turn_context_built`
- `turn_context_length_exceeded`
- `turn_local_baseline_over_budget`
- `turn_local_compaction_applied`
- `turn_local_checkpoint_requested`
- `turn_local_checkpoint_recorded`
- `turn_local_checkpoint_resume_requested`
- `episode_memory_finalized`
- `working_memory_updated`
- `recovery_cleared_missing_worktree_session`
- `max_output_tokens_recovery`

Policy:

- `info`: show only failures or hard limits
- `verbose`: show compaction/recovery only when it explains a visible pause or
  continuation
- `debug`: show token/context/recovery details
- `trace`: raw payload

### Operator Delivery

Events:

- `operator_delivery_submitted`
- `operator_delivery_completed`
- `operator_notification_mirror_failed`
- `operator_transport_binding_upserted`

Policy:

- `info`: show mirror failures if operator-visible
- `verbose`: show delivery failures and important routing changes only when the
  operator can act on them or when they explain missing/duplicated output
- `debug`: show delivery target, status, and binding metadata
- `trace`: raw payload

## Trace-Only Defaults

These events should be trace-only by default:

- queue and message plumbing events
- request/admitted events that are followed by clearer result events
- state sync events
- system tick events
- closure/continuation bookkeeping
- delivery bookkeeping
- internal memory finalization events

A trace-only event may still be promoted to `debug` if it carries an error or is
needed to explain a visible state transition.

## Level Contract Matrix

This table summarizes the intended operator contract:

| Surface item | `info` | `verbose` | `debug` |
| --- | --- | --- | --- |
| Operator message | show | show | show |
| Final brief or completion | show | show | show |
| Required operator attention | show | show | show with ids/details |
| Assistant progress text | hide | show compact | show with context |
| Plan or plan update | hide unless final/result | show compact | show with full relevant detail |
| Command execution | hide unless failure/relevant result | show command/status/short preview | show cwd/duration/output refs/longer preview |
| File mutation | hide unless failure/relevant result | show changed-file summary | show bounded diff and diagnostics |
| File read/search | hide | show compact summary when useful | show paths/query/details |
| Child agent/task result | show when final or failed | show high-level activity | show ids/status transitions |
| Work item bookkeeping | hide except completion/failure/wait | hide routine writes; show meaningful posture changes | show ids/state/todo deltas |
| Provider round telemetry | hide | hide | show/attach telemetry |
| Callback/timer/wake | hide except current posture/failure/attention | show external wake or resume reason when it explains continuation | show lifecycle details |
| Scheduler/continuation/closure | hide | hide | show diagnostic lifecycle |
| Context/memory/compaction | hide except hard limits/failures | show only user-relevant recovery | show diagnostic details |
| Raw unknown event | hide | hide | show only if curated; otherwise trace |

## API And Client Implications

The raw event stream should continue to expose raw events.

Operator-facing clients should not directly render every event in the stream.
They should consume either:

- a shared presentation/reduction library, or
- a server-provided operator projection derived from the same shared rules

The first implementation can live in the TUI, but the rules should remain
runtime-level and reusable.

The HTTP event surface should distinguish raw-event eligibility from
presentation rendering:

- `/events` returns raw event envelopes for inspection.
- `/events?max_level=info|verbose|debug` returns raw event envelopes whose
  shared event level is eligible for the requested maximum level.
- `/events?max_level=verbose` does not mean "return compact presentation
  rows"; it means "return raw envelopes whose event level is no more detailed
  than `verbose`."
- Compact density is produced by a presentation reducer, either client-side in
  the TUI or through a future server-provided presentation endpoint.
- Raw trace/audit inspection remains available by omitting `max_level` or by
  using a dedicated trace/debug inspection surface.

This preserves one level contract across server and TUI while allowing the TUI
to render the same event group differently at `verbose` and `debug`.

## Compatibility

Existing event kinds remain valid.

This RFC intentionally changes the meaning of the numeric main-conversation
display aliases.

Current code and older docs use `3`, `4`, and `5` as direct numeric projections
of `OperatorVisibility::{TurnResult, Progress, Trace}`. Under that model,
`/display 5` means "show trace-level items in the main conversation."

This RFC replaces that model for operator-facing conversation surfaces:

- `3` remains the default result-oriented mode, now named `info`
- `4` becomes the compact activity mode, now named `verbose`
- `5` becomes a curated detailed mode, now named `debug`
- raw trace inspection moves out of the main conversation and into `/events` or
  equivalent trace inspectors

The old `5=trace` behavior should not remain a main-conversation display mode.
Operators who need raw event kinds, raw payloads, replay provenance, or
redaction diagnostics should use `/events` rather than `/display 5`.

Historical replay should be rendered through the new presentation rules where
possible. If a historical event lacks fields required for a polished item, the
renderer should prefer hiding it over showing a low-value fallback such as
`stop=unknown` or `model=model`.

Raw event inspection remains available through trace surfaces.

## Implementation Plan

1. Introduce display mode names: `info`, `verbose`, `debug`, and reserve `trace`
   for raw inspectors.
2. Migrate numeric aliases to the named modes: `3=info`, `4=verbose`,
   `5=debug`.
3. Split event classification from final rendering.
4. Add a reduced operator item model for command, tool, task, work item,
   assistant message, notice, error, and model-call metadata.
5. Merge command/tool lifecycle events into one item.
6. Hide provider rounds by default and attach useful provider telemetry to nearby
   items in `debug`.
7. Render work item updates as deltas.
8. Keep `/events` as the trace/raw event inspector, with `max_level` acting only
   as raw-envelope eligibility filtering rather than presentation rendering.
9. Add snapshot tests for representative event sequences at each display mode.

## Initial Decisions

### Projection Location

Decision:

- the first implementation should reduce events client-side
- the reduction rules should live in reusable code rather than inside one TUI
  render function
- the daemon should continue exposing the raw event stream as the canonical
  runtime surface

Reason:

The event stream contract already treats raw runtime events as the source of
truth. Introducing a daemon-owned operator projection too early would create a
second server-side event contract before the reduction model is proven.

Future clients may still share the same reduction library, and the daemon may
later expose an optional operator projection endpoint if remote clients need a
lower-bandwidth or lower-state surface.

### Debug Output Size

Decision:

- `debug` should include full commands and bounded ApplyPatch diffs
- `debug` should not include full command stdout/stderr or arbitrary large tool
  outputs by default
- large outputs should be summarized in the main conversation and inspected via
  `/events`, task output, or a future expansion/pager surface

Reason:

`debug` is a detailed operator view, not raw trace. Full commands are usually
bounded and decision-relevant. Patch diffs and process output can be large, so
the main conversation should render bounded details with an inspection path for
the full artifact when needed.

### Remote Operator Preference

Decision:

- display mode should be treated as an operator subscription/view preference
- remote operator surfaces should use the same `info`, `verbose`, `debug`, and
  `trace` vocabulary
- `trace` should still require an explicit inspection surface or permission
  boundary

Reason:

The display levels are not TUI-specific. AgentInbox bridges, web clients, remote
TUI sessions, notification adapters, and future operator dashboards need a
shared way to request result-only, activity, or debug views.

### Reduction State

Decision:

- the first implementation should recompute reduced items from snapshots and
  recent events
- reduction state should not be persisted as a new durable store in the first
  version
- persistence can be added later only for reductions that must remain stable
  across replay-window boundaries

Reason:

Most reductions are local presentation conveniences. Persisting them too early
would create another state plane. Recomputing from recent events keeps the
runtime simpler while the presentation model stabilizes.
