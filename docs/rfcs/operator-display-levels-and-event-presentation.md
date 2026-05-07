---
title: RFC: Operator Display Levels and Event Presentation
date: 2026-05-07
status: draft
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
- `verbose`: Codex-like activity view
- `debug`: detailed execution view
- `trace`: raw audit/event inspection, reserved for event inspectors such as
  `/events`

Only `info`, `verbose`, and `debug` should be main operator conversation display
modes. `trace` is not a normal conversation mode; it is an inspection surface.

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
- make `verbose` comparable to the current Codex activity view
- make `debug` more detailed than Codex when useful, including full commands and
  full patch diffs

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

It should approximate Codex's main activity presentation:

- assistant text is visible
- command/tool activity is visible as human-readable activity rows
- related lifecycle events are merged into one item when possible
- work item changes are shown as deltas, not as full internal records
- context management and recovery events are shown only when they explain a
  visible delay or continuation

`verbose` should not show raw provider telemetry by default.

### `debug`

`debug` is the detailed operator view.

It should include everything in `verbose`, plus details useful for debugging:

- full shell commands
- full ApplyPatch diffs
- command cwd, duration, and exit status
- tool input/output summaries
- provider model, stop reason, token usage, and round number when available
- work item ids and task ids when helpful

`debug` should still be a curated operator view. It should not dump every raw
runtime event into the conversation.

### `trace`

`trace` is raw event inspection.

It is reserved for surfaces such as `/events`, local debug logs, or future event
inspectors.

`trace` may show raw event kinds, raw payloads, redaction markers, provenance,
sequence ids, and replay metadata.

`trace` is not a main conversation display level.

## Presentation Pipeline

The presentation pipeline should have four stages:

1. raw event ingestion
2. event classification and normalized field extraction
3. event reduction into operator items
4. display-level rendering

The existing `present_operator_event` function roughly covers stage 2. It should
not be treated as the final rendering contract.

Operator surfaces should render reduced operator items, not raw events.

### Event Classification

Each event should be classified into one of these presentation actions:

- `show`: render as its own item
- `hide`: do not render in main conversation
- `merge`: combine with related events into one item
- `attach`: attach as metadata to a nearby item
- `trace_only`: show only in raw event inspectors

The classification may depend on display level.

### Event Reduction

Some event families are only meaningful after reduction.

Examples:

- `assistant_round_recorded` with tool calls should be merged with the subsequent
  tool or command item instead of producing `Assistant requested tools: ...`
- `process_execution_requested` and `tool_executed` should become one command or
  tool lifecycle item
- `task_created`, `task_status_updated`, and `task_result_received` should become
  one task lifecycle item when they refer to the same task
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
- `message_processing_interrupted`
- `turn_started`

Policy:

- `operator_interjection_admitted`: show as operator input in all main levels
- `message_processing_interrupted`: show only when it explains an interruption or
  failure
- all other message plumbing events: hide in main levels; trace only

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

Policy:

- `info`: show only failures or operator-relevant final actions
- `verbose`: merge command/tool start and completion into human-readable activity
  items
- `debug`: show full command/tool details, including full ApplyPatch diff and
  full command text
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

- `info`: show completed work items and work-item-related failures only
- `verbose`: show work item deltas, not full internal records
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

Policy:

- `info`: show task/delegation completion, failures, and child-agent results
- `verbose`: show high-signal lifecycle changes
- `debug`: show ids, workspace mode, task kind, and status transitions
- `trace`: raw payload

Task lifecycle events with the same task id should be reduced into one task
activity item when possible.

### Waiting, Timers, And External Events

Events:

- `waiting_intent_created`
- `waiting_intent_cancelled`
- `stale_waiting_intents_cancelled`
- `callback_delivered`
- `timer_create_requested`
- `timer_created`
- `timer_fired`
- `timer_fire_failed`

Policy:

- `info`: show active waits, fired timers that wake work, callbacks, and failures
- `verbose`: show wait/timer lifecycle when it explains agent posture
- `debug`: show ids, source, deadline, and cancellation reason
- `trace`: raw payload

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
- `verbose`: show visible workspace/worktree switches and cleanup outcomes
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
- `agent_state_changed`
- `state_changed`
- `session_state_changed`

Policy:

- `info`: show created agent and final model changes
- `verbose`: show model switching activity
- `debug`: show requested/resolved model details
- state sync events: main conversation hidden; `/state` or trace only

### Control, Continuation, And Closure

Events:

- `control_request_admitted`
- `control_applied`
- `current_run_interrupted`
- `wake_requested`
- `continuation_trigger_received`
- `continuation_resolved`
- `closure_decided`
- `system_tick_emitted`
- `system_tick_suppressed`
- `runtime_service_shutdown_requested`

Policy:

- `info`: show interrupts and shutdown requests; hide routine control plumbing
- `verbose`: show control results that change visible behavior
- `debug`: show continuation/closure details only when useful for diagnosing
  agent posture
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
- `verbose`: show delivery failures and important routing changes
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

## API And Client Implications

The raw event stream should continue to expose raw events.

Operator-facing clients should not directly render every event in the stream.
They should consume either:

- a shared presentation/reduction library, or
- a server-provided operator projection derived from the same shared rules

The first implementation can live in the TUI, but the rules should remain
runtime-level and reusable.

## Compatibility

Existing event kinds remain valid.

This RFC intentionally changes the meaning of the numeric main-conversation
display aliases.

Current code and older docs use `3`, `4`, and `5` as direct numeric projections
of `OperatorVisibility::{TurnResult, Progress, Trace}`. Under that model,
`/display 5` means "show trace-level items in the main conversation."

This RFC replaces that model for operator-facing conversation surfaces:

- `3` remains the default result-oriented mode, now named `info`
- `4` becomes the Codex-like activity mode, now named `verbose`
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
8. Keep `/events` as the trace/raw event inspector.
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

- `debug` should include full commands and full ApplyPatch diffs
- `debug` should not include full command stdout/stderr or arbitrary large tool
  outputs by default
- large outputs should be summarized in the main conversation and inspected via
  `/events`, task output, or a future expansion/pager surface

Reason:

`debug` is a detailed operator view, not raw trace. Full commands and patches are
usually bounded and decision-relevant. Full process output can be unbounded and
would make the main conversation unusable.

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
