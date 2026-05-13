---
title: RFC: Work Item Centered Agent Runtime
date: 2026-05-13
status: draft
handle: rfc-work-item-centered-agent-runtime
---

# RFC: Work Item Centered Agent Runtime

## Summary

This RFC defines the target shape for making `WorkItem` the central durable
thread of long-running agent work.

The direction is:

- an agent owns a durable work queue made of WorkItems;
- one WorkItem is the sticky current focus at a time;
- WorkItems carry the agent-authored objective, plan artifact reference, todo
  snapshot, blocker, process trace, and completion report;
- scheduler activation is derived from WorkItem readiness and external
  reactivation signals;
- context compaction should preserve current WorkItem truth and compress older
  WorkItem history around completed reports and process notes;
- `CompleteWorkItem.result_summary` should become the canonical report for a
  completed WorkItem.

This RFC is intentionally cross-cutting. It does not replace the lower-level
RFCs for WorkItem schema, scheduler decisions, waiting intents, or context
compaction. Instead, it defines how those pieces should compose into one
agent-facing work mode.

## Problem

Holon already has useful pieces:

- `WorkItem` records objective, plan artifact metadata, todo list, blocker, and
  result summary;
- scheduler work-queue ticks can continue current work or surface queued work;
- waiting intents can bind external triggers to a WorkItem;
- transcript, briefs, and working memory retain runtime history.

However, these pieces are still loosely connected. In a long-running scenario an
agent may receive a list of issues, create many WorkItems, gradually plan them,
work through todos, wait on external events, switch to other work, then return
when a wait is triggered.

The current model can approximate that flow, but the runtime does not yet make
it first-class:

- WorkItem is a task record, not yet the primary narrative thread;
- assistant messages are transcript entries, not explicitly modeled as
  WorkItem process traces;
- briefs are turn/session artifacts, not clearly tied to WorkItem reports;
- context compaction is not consistently WorkItem-centered;
- blocked WorkItems with triggered external events are not surfaced as a
  distinct scheduling candidate class;
- current/queued switching relies mostly on prompt discipline;
- task supervision and WorkItem dependencies still risk being conflated.

The result is that the model can lose track of the intended work rhythm:

1. discover work;
2. record draft WorkItems;
3. clarify objectives;
4. edit the WorkItem plan artifact and write todos;
5. execute todo steps;
6. record process notes;
7. wait or switch when blocked;
8. resume when relevant external events arrive;
9. complete with a durable report;
10. select the next WorkItem.

## Goals

- Make WorkItem the durable unit for agent-owned work, not just a side record.
- Store the plan body as an AgentHome file artifact that the agent can read,
  grep, and patch directly.
- Keep `blocked_by` as flexible agent-authored natural language.
- Keep `runnable` as a derived view, not a stored state.
- Preserve sticky current focus unless the agent reaches a safe point or makes
  an explicit switch.
- Let blocked WorkItems receive triggered external-event signals without
  automatic unblocking.
- Make WorkItem process notes and result reports first-class context and
  compaction anchors.
- Keep the tool surface small; prefer improving existing WorkItem tools before
  adding fine-grained todo tools.
- Make scheduler behavior explainable from WorkItem projections.

## Non-goals

- Do not reintroduce a large enum of blocker kinds that the runtime must infer.
- Do not auto-clear `blocked_by` from callback delivery alone.
- Do not make external event matching decide that a WorkItem is complete.
- Do not add single-todo mutation tools as the default todo API.
- Do not allow the scheduler to silently switch `current_work_item_id` merely
  because another WorkItem became available.
- Do not make WorkItem replace raw transcript or audit ledgers.

## Terms

### WorkItem

The agent-owned durable work thread.

It contains:

- a short objective;
- natural-language plan artifact metadata;
- full todo-list snapshot;
- natural-language blocker;
- completion report;
- derived scheduling/readiness views.

### Plan Artifact

The WorkItem-owned plan file under AgentHome.

Suggested path:

```text
agent_home/work-items/<work_item_id>/plan.md
```

The plan body is edited directly by the agent with normal file tools. WorkItem
records and read tools expose only artifact metadata and bounded preview. The
first descriptor version is path-first:

- absolute `path`;
- hash;
- byte size;
- updated timestamp;
- preview text;
- `preview_complete`.

`path` is the agent-facing locator. It works for direct reads, grep, and shell
commands even when the active workspace is a project workspace. Workspace
identity can be added later after AgentHome has a globally unique workspace id.

The preview is a cache and may be truncated. The file is the source of truth.

### Current WorkItem

The WorkItem currently in focus for the top-level agent.

Current focus is sticky. New ingress, queued work, or external events should not
silently replace it.

### Process Trace

The raw transcript and tool/event history that happened while a WorkItem was
current.

Assistant messages are part of this trace. They are useful for audit and recent
context, but they should not all become permanent high-priority memory.

### Process Note

A compressed or selected durable note derived from the WorkItem process trace.

A process note records useful progress, decisions, blockers, or findings for
future resumption.

### Result Report

The final WorkItem completion report. The canonical source should be
`CompleteWorkItem.result_summary`.

### Readiness

A derived view over WorkItem state:

- `completed` when lifecycle state is completed;
- `blocked` when `blocked_by` is present;
- `waiting_for_operator` when `plan_status = needs_input`;
- `runnable` otherwise, for open WorkItems.

`runnable` should not be stored as a mutable state.

### Triggered Wait

A WorkItem-scoped waiting intent that received an external event.

A triggered wait means: something changed and the agent should re-evaluate the
WorkItem. It does not mean the blocker is resolved.

## Target Work Mode

The intended loop is:

1. Operator and agent discuss work.
2. The agent records discovered units of work as draft WorkItems.
3. Through conversation or inspection, the agent refines objective, edits the
   plan artifact, and updates the todo list.
4. The agent works through todos, updating the WorkItem after meaningful
   progress.
5. If blocked, the agent records `blocked_by` and, when useful, creates a
   WorkItem-scoped waiting intent.
6. The agent can switch to another runnable WorkItem while the blocked one
   remains tracked.
7. When an external event triggers a waiting intent, the runtime surfaces that
   WorkItem as a candidate for review.
8. The agent explicitly decides whether to clear the blocker, edit the plan
   artifact, continue waiting, or complete the WorkItem.
9. Completion writes a result report and releases current focus.
10. Scheduler surfaces the next runnable candidate.

## WorkItem State And Readiness

The current minimal state shape should remain small:

- `state = open | completed`;
- `plan_status = draft | ready | needs_input`;
- `blocked_by: Option<String>`;
- `todo_list` as a full snapshot;
- `plan_artifact` metadata with bounded preview.

The runtime should derive richer views instead of requiring the agent to set
many status enums.

Suggested derived projection:

```rust
struct WorkItemReadinessProjection {
    lifecycle: WorkItemLifecycleView,
    readiness: WorkItemReadiness,
    focus: WorkItemFocusView,
    has_active_waits: bool,
    has_triggered_waits: bool,
    current_todo: Option<TodoItem>,
}
```

The exact type name is not normative, but the projection should be stable enough
for scheduler tests, `/state`, `/tasks`, TUI, and prompt context to agree.

## Blockers

`blocked_by` should remain a natural-language field authored by the agent.

Runtime interpretation must stay narrow:

- `blocked_by.is_some()` means the WorkItem is not runnable;
- `blocked_by.is_none()` means the WorkItem may be runnable if other derived
  conditions allow it.

The runtime must not parse blocker text into kinds such as `waiting_for_ci`,
`waiting_for_review`, or `waiting_for_task`.

If more machine-readable linkage is needed, it should live beside the blocker,
not inside it. WorkItem-scoped waiting intents are the current mechanism for
that linkage.

## Waiting And External Events

A WorkItem can have active waiting intents. Waiting intent records may carry:

- WorkItem id;
- source;
- resource;
- condition;
- trigger count;
- last triggered time;
- delivery mode.

When a callback or external event arrives:

- preserve provenance;
- update waiting intent trigger metadata;
- wake or re-enter the model according to delivery mode;
- surface the WorkItem as triggered in projections;
- do not automatically clear `blocked_by`;
- do not automatically complete the WorkItem;
- let the agent explicitly call `UpdateWorkItem` or `CompleteWorkItem` after it
  evaluates the event.

This keeps runtime automation honest. External systems can say that something
changed; the agent remains responsible for deciding whether the objective can
advance.

## Scheduler Semantics

Scheduler behavior should be WorkItem-centered but not WorkItem-mutating.

The default order should be:

1. If the current WorkItem is runnable, continue it.
2. If the current WorkItem is non-runnable, release or ignore it as active work
   according to the focus-release rules below.
3. If there are triggered blocked WorkItems, surface them as review candidates.
4. If there are queued runnable WorkItems, surface them as work candidates.
5. If no candidates exist, remain idle or sleep according to runtime posture.

The scheduler should not silently set `current_work_item_id` to a queued item.
It may emit a system tick that asks the model to pick or review work.

### Current Focus Release

The runtime should release current focus when the current WorkItem becomes
non-runnable through an explicit WorkItem mutation:

- `CompleteWorkItem` completes it;
- `UpdateWorkItem(blocked_by = Some(...))` marks it blocked;
- `UpdateWorkItem(plan_status = needs_input)` marks it waiting for operator or
  external clarification.

This avoids a separate `YieldWorkItem` tool for the common case where the agent
already expressed why the current item cannot continue.

If the current WorkItem remains runnable but the agent wants to switch, it
should use `PickWorkItem` with an explicit reason once that tool shape supports
it. This is an agent-directed focus override, not a scheduler guess.

### Candidate Classes

Scheduler and context projections should distinguish candidate classes:

- `current_runnable`;
- `triggered_blocked`;
- `queued_runnable`;
- `waiting_for_operator`;
- `blocked`;
- `completed_recent`.

The exact names can change, but the distinction matters. A triggered blocked
WorkItem is not the same as a runnable queued WorkItem.

### Preemption

External events should not preempt current runnable work by default.

If a WorkItem is current and runnable, an external event for another WorkItem
should be surfaced as a candidate. The agent can finish the current step,
update the current WorkItem, then choose whether to switch.

Operator interjection or explicitly urgent control input may still preempt
according to the operator/control-plane contracts.

## Tool Surface

The preferred tool surface remains small.

### Keep

- `CreateWorkItem`
- `PickWorkItem`
- `UpdateWorkItem`
- `CompleteWorkItem`
- `GetWorkItem`
- `ListWorkItems`

### Avoid For Now

- `AddTodo`
- `UpdateTodo`
- `CompleteTodo`
- `ReorderTodo`
- `NextWorkItem`
- `YieldWorkItem`

Todo list mutation should remain full-snapshot replacement through
`UpdateWorkItem`. This matches the model-friendly pattern where the agent
rewrites its current complete checklist after meaningful progress.

Plan body mutation should not use `UpdateWorkItem` and should not introduce a
separate `UpdateWorkItemPlan` tool in phase one. The plan body is a normal
AgentHome file artifact; the agent should read, grep, and patch that file
directly. `UpdateWorkItem` remains responsible for explicit state changes such
as `plan_status`, `todo_list`, and `blocked_by`.

WorkItem read tools should not expose full plan bodies. `include_plan` is
deprecated; `GetWorkItem` and `ListWorkItems` should always return the plan
artifact descriptor, bounded preview, and `preview_complete` marker instead.

`NextWorkItem` is not needed initially because queued runnable work can already
be surfaced by scheduler ticks after current focus is released.

`YieldWorkItem` is not needed initially because the common yield cases are
already expressed by `UpdateWorkItem(blocked_by=...)`,
`UpdateWorkItem(plan_status=needs_input)`, or `CompleteWorkItem`.

### PickWorkItem

`PickWorkItem` should remain the explicit target selection tool. It should not
be deprecated.

Future refinements may add:

- `reason` when switching away from a runnable current WorkItem;
- warnings or validation when picking a blocked WorkItem;
- optional force semantics for inspection-only picks.

## Todo List Semantics

`todo_list` should remain a full snapshot.

The runtime should not require stable todo ids or single-item mutation tools in
this phase. Those add schema and tool-surface complexity without solving the
core runtime problem.

The runtime should instead improve projections and reminders:

- derive `current_todo` as first `in_progress`, otherwise first `pending`;
- show active todos in context and reminders;
- remind the agent when many provider rounds pass without WorkItem mutation;
- remind when todo state appears stale, without mutating it automatically;
- warn or ask for reason if completing a WorkItem with unfinished active todos.

## Assistant Messages, Process Notes, And Reports

Assistant messages during a WorkItem should be treated as raw process trace.

They should usually carry or inherit `work_item_id` from current focus so that
transcript, events, and future compaction can group them by WorkItem.

Durable process notes should be derived from process trace and explicit
WorkItem mutations. They should capture:

- decisions;
- completed meaningful steps;
- blockers discovered;
- external resources created;
- important tool outputs;
- scope changes.

The completion report should be `CompleteWorkItem.result_summary`. This report
should be the canonical short answer for what the WorkItem accomplished. Other
briefs may cite or render it, but should not compete with it as the durable
source of truth.

## Context And Compaction

Context projection should be WorkItem-centered.

For the current WorkItem, keep high-priority context:

- objective;
- plan artifact descriptor and bounded preview;
- active todo list;
- blocker;
- active or triggered waits;
- recent process trace;
- durable process notes.

For queued WorkItems, keep compact candidate summaries:

- objective;
- readiness;
- plan artifact preview;
- current/next todo;
- blocker or triggered wait summary.

For completed WorkItems, keep:

- result report;
- important references;
- maybe a short process note summary if relevant to current work.

Compaction should not turn raw assistant chatter into permanent memory by
default. It should preserve selected process notes and completed reports.

## Briefs And Operator Output

The operator-visible final brief for a WorkItem should align with
`CompleteWorkItem.result_summary`.

During execution, assistant messages can remain conversational process updates.
The runtime should not require every assistant message to become a durable
report.

When a WorkItem completes, the runtime can generate or surface a completion
brief from the WorkItem result summary.

## Relationship To Other RFCs

This RFC composes lower-level contracts:

- `work-item-runtime-model.md` owns WorkItem schema and tool contracts;
- `runtime-scheduler-contract.md` owns scheduler decision vocabulary and test
  fixtures;
- `waiting-plane-and-reactivation.md` owns waiting tools and reactivation
  semantics;
- `long-lived-context-memory.md` and `turn-local-context-compaction.md` own
  memory and context projection mechanics;
- `agent-control-plane-model.md` owns child-agent supervision and public/private
  agent boundaries.

Implementation PRs should update the lower-level RFCs when they make a concrete
schema or decision change.

## Implementation Plan

A plausible sequence is:

1. Add WorkItem-centered projection fields for candidate classes and current
   todo derivation.
2. Surface triggered WorkItem-scoped waiting intents as candidates without
   clearing blockers.
3. Update scheduler tick text and state/TUI projections to distinguish current,
   queued, blocked, triggered, and waiting-for-operator work.
4. Make WorkItem focus release explicit when current work becomes completed,
   blocked, or needs input.
5. Tighten `PickWorkItem` semantics and optionally add a reason field for
   switching away from runnable current work.
6. Bind assistant rounds, briefs, and relevant events to current WorkItem where
   provenance is trusted.
7. Make `CompleteWorkItem.result_summary` the canonical WorkItem completion
   report in briefs and compaction.
8. Add WorkItem-centered compaction policy for current, queued, blocked,
   triggered, and completed WorkItems.

## Testing Strategy

Add tests at several layers:

- pure WorkItem readiness and candidate projection tests;
- scheduler fixture tests for current runnable, current blocked, queued
  runnable, triggered blocked, and no-work cases;
- callback/waiting-intent tests proving triggered events do not auto-clear
  blockers;
- WorkItem focus tests proving blocked/needs-input/completed current work
  releases focus;
- context snapshot tests proving current WorkItem and active todos survive
  compaction;
- transcript/brief tests proving WorkItem result summaries are surfaced as
  completion reports;
- TUI/state projection tests for candidate classes.

## Decisions

### PickWorkItem Reason

`PickWorkItem` should require a `reason` only when switching away from a
runnable current WorkItem.

The reason should not be stored on either WorkItem. It is metadata for a focus
transition and belongs on the `work_item_picked` event, or a future dedicated
focus-transition ledger if one becomes necessary.

Suggested event payload fields:

```json
{
  "agent_id": "...",
  "previous_work_item_id": "...",
  "current_work_item_id": "...",
  "reason": "higher priority operator request",
  "previous_readiness": "runnable",
  "current_readiness": "blocked",
  "switch_kind": "explicit_focus_override"
}
```

Reason should not be written into `blocked_by`, the plan artifact,
`result_summary`, or agent state.

### Picking Blocked WorkItems

Picking a blocked WorkItem should be allowed for review or inspection. It does
not make the WorkItem runnable.

If the agent decides the blocker is resolved, it must explicitly clear
`blocked_by` through `UpdateWorkItem` before treating the WorkItem as runnable.

### Focus Release Timing

Current focus should be released immediately inside the WorkItem mutation that
makes the current WorkItem non-runnable:

- `CompleteWorkItem` completes it;
- `UpdateWorkItem(blocked_by = Some(...))` blocks it;
- `UpdateWorkItem(plan_status = needs_input)` marks it waiting for input.

Waiting until turn end would make scheduler projection and tool results lag
behind the agent's explicit state change.

Clearing `blocked_by` should not automatically make that WorkItem current
again. The agent must call `PickWorkItem` if it wants to resume it.

### Process Notes

Do not introduce a standalone `process_note` ledger in phase one.

Process notes should initially be a projection/compaction product derived from:

- assistant rounds and transcript;
- tool executions;
- WorkItem mutation history;
- briefs;
- `CompleteWorkItem.result_summary`.

Add a dedicated `WorkItemProcessNoteRecord` only if durable edited notes need to
be referenced, synced, displayed, or retained independently of their sources.

### CompleteWorkItem With Unfinished Todos

`CompleteWorkItem` should succeed when active todos remain unfinished, but the
tool receipt and event should include a warning with unfinished todo counts.

Todo list is agent working memory, not a strict project-management database.
Hard rejection would create friction and force unnecessary bookkeeping. The
runtime should still encourage agents to update todos before completion.

### WaitingIntent Trigger State

Do not add a terminal-like `triggered` status to `WaitingIntent` in this phase.

Keep persisted status as:

- `active`
- `cancelled`

Treat triggered state as derived from:

- `trigger_count > 0`
- `last_triggered_at != None`

This allows a wait to stay active across repeated external events, such as
multiple review comments, CI updates, or inbox entries.

### Context Budget For Candidate WorkItems

Model request context should be layered:

- current WorkItem: full objective, plan artifact descriptor and preview, active
  todos, blocker, active or triggered waits, recent process trace, and durable
  process notes;
- triggered candidates: compact top candidates;
- queued runnable candidates: compact top candidates;
- blocked candidates: count plus compact top candidates when relevant;
- recently completed WorkItems: count or a small number of result summaries.

The runtime should not include the full plan body for any WorkItem by default.
It should include the plan artifact descriptor plus preview. Agents can read or
grep the AgentHome plan file directly when they need the full plan.

### Candidate Ranking

Candidate ranking should be simple, stable, and explainable.

Phase-one ordering:

- triggered candidates: `last_triggered_at desc`, then `updated_at desc`;
- queued runnable candidates: `updated_at asc`, then `created_at asc`;
- blocked candidates: `updated_at desc`;
- recently completed candidates: `updated_at desc`.

Default prompt limits:

- triggered candidates: top 3;
- queued runnable candidates: top 5;
- blocked candidates: top 3;
- recently completed candidates: top 3.

When prompt budget is tight, preserve candidate classes in this priority order:

1. current WorkItem;
2. triggered candidates;
3. queued runnable candidates;
4. blocked counts or compact summaries;
5. recently completed summaries.

This favors recent external reactivation, fairness for queued runnable work, and
compact background awareness for blocked or completed work.

### PickWorkItem Missing Reason Compatibility

During phase one, missing `PickWorkItem.reason` should be a warning, not a hard
validation error.

If the agent switches away from a runnable current WorkItem without a reason:

- `PickWorkItem` should still succeed;
- the tool receipt should include a warning;
- the `work_item_picked` event should include `reason_required = true` and
  `reason_missing = true`.

This keeps compatibility with current agent behavior while making poor focus
switching visible. A later phase may make the reason mandatory if warning-only
behavior is not enough.

### CompleteWorkItem Warning Receipt

When `CompleteWorkItem` succeeds with unfinished todos, the canonical result
payload should include structured warnings.

Suggested result shape:

```json
{
  "work_item": { "...": "..." },
  "warnings": [
    {
      "kind": "unfinished_todos",
      "message": "Work item completed with unfinished todo items.",
      "pending_count": 2,
      "in_progress_count": 1,
      "sample": [
        { "text": "run regression tests", "state": "pending" },
        { "text": "update docs", "state": "in_progress" }
      ]
    }
  ]
}
```

The corresponding completion event should include aggregate fields such as:

```json
{
  "work_item_id": "...",
  "completed_with_unfinished_todos": true,
  "unfinished_todo_count": 3,
  "pending_todo_count": 2,
  "in_progress_todo_count": 1
}
```

If there are no unfinished todos, `warnings` should be empty or omitted according
to the shared tool-result envelope convention.
