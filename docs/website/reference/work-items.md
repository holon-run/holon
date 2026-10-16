---
title: Work items
summary: Work item fields, statuses, lifecycle operations, and the CLI and HTTP surfaces that act on them.
order: 38
---

# Work items

> **Mental model & specification:** To understand why work items exist and how they fit into Holon's design, see [Runtime model concepts](/concepts/runtime-model.md). For internal state machine transitions and scheduler contracts, see [Work items spec](/spec/work-items.md).

Work items are Holon's durable unit of tracked work. Use them when an objective
needs its own lifecycle, progress tracking, or cross-turn continuity.

## When to Use a Work Item

Create or update a work item when the task:

- spans multiple turns
- needs resumable progress
- waits on external state such as CI, callbacks, or operator input
- has clear acceptance criteria that should be tracked explicitly

Avoid creating work items for:

- casual questions
- one-shot explanations
- short inspections
- lightweight current-turn tasks that can finish immediately

## Work Item Lifecycle

A work item can be:

- **open** — still active
- **completing** — completion has been requested but its required report is not yet bound
- **completed** — explicitly finished
- **current** — the agent's active focus
- **queued** — an open item available for scheduling outside the current focus
- **yielded** — parked behind a WorkItem continuation
- **blocked** — waiting on a specific blocker
- **waiting for operator** — needs clarification or approval
- **runnable** — ready for execution

The key distinction is:

- **lifecycle** describes whether the objective is open or completed
- **focus** describes whether the item is current
- **readiness** describes whether the scheduler should resume it

## What a Work Item Stores

Each work item can contain:

- **objective** — short statement of the goal
- **plan artifact** — durable markdown file describing the intended approach
- **plan artifact status** — artifact resolution state: `available`, `missing`, `unreadable`, or `not_recorded`
- **plan status** — `draft`, `ready`, or `needs_input`
- **todo list** — checklist of meaningful progress steps
- **blocked by** — specific blocker when progress cannot continue
- **recheck deadline** — fallback time for reconsidering a blocked item

Treat the work item as coordination state, not a scratchpad.

### Record fields

The persistent `WorkItemRecord` contains these fields. Read-model projections
may add focus, readiness, scheduling state, and plan-artifact resolution status;
those are not interchangeable with the stored lifecycle `state`.

| Field | Contract |
|---|---|
| `id`, `agent_id`, `workspace_id` | Item identity, owning agent, and workspace |
| `revision` | Version used to reject stale state transitions |
| `objective` | Tracked goal |
| `state` | `open`, `completing`, or `completed` |
| `plan_status` | `draft`, `ready`, or `needs_input` |
| `plan_artifact` | Optional plan descriptor with owner/workspace identity, relative and absolute paths, hash, byte count, timestamp, preview, and preview completeness |
| `todo_list` | Entries containing `text` and `state`: `pending`, `in_progress`, or `completed` |
| `work_refs` | Evidence references with kind, reference, reason, status, timestamp, and optional title/source/metadata |
| `blocked_by` | Current blocker, if any |
| `blocked_by_wait_id` | Wait that established the blocker; absent for independent or older blockers |
| `recheck_at`, `recheck_consumed_at` | Fallback recheck deadline and its consumption timestamp |
| `result_brief_id`, `result_summary` | Canonical completion report linkage and summary |
| `completion_intent` | Completion source/revision, required report state, and report linkage |
| `created_at`, `updated_at`, `turn_id` | Record timestamps and optional associated turn |

`plan_artifact_status=missing` or `unreadable` does not make the item itself
unqueryable. Read the actual plan path returned by the runtime; do not invent a
path from the item ID.

## Core Operations

Typical work-item operations:

- `CreateWorkItem` — create a new tracked objective
- `PickWorkItem` — make an open item the current focus
- `UpdateWorkItem` — refine objective, plan status, or todo list
- `ListWorkItems` — inspect current, open, blocked, or completed work
- `GetWorkItem` — inspect one work item in detail
- `CompleteWorkItem` — mark the objective done
- `WaitFor` — record an explicit wait without changing the task's ownership

Edit the plan file for plan-body changes. Use `WaitFor` for agent waits, not
blocker fields on `UpdateWorkItem`. Operator HTTP PATCH has separate
`blocked_by`/`recheck_after` controls documented in the
[HTTP reference](http-control-plane.md); those are not agent-tool arguments.

`PickWorkItem` can yield the current execution through a continuation to another
item. `clear_blocker=true` requires evidence that the blocker is resolved and a
reason. Do not use it to bypass a yielded item's return path.

`CompleteWorkItem` requires acceptance evidence and a non-empty assistant
completion report. Bound completion settles that execution; detached completion
preserves the current execution when allowed. An item active in another execution
cannot be completed by a different execution or control request. The operator
HTTP completion route requires `report_text` and persists the canonical report.

## Example Workflow

1. Inspect whether the objective already has an open work item
2. Create one only if the objective has its own lifecycle
3. Edit the durable plan artifact once the acceptance boundary is clear
4. Update the todo list after material progress
5. Record blockers explicitly instead of silently broadening scope
6. Complete the item only after acceptance evidence exists

## Plan Status

Use plan status intentionally:

- **`draft`** — the objective exists, but the approach is still forming
- **`ready`** — the plan is stable enough to execute
- **`needs_input`** — the next step depends on operator input

This matters because the runtime can distinguish active runnable work from work
that should pause.

## Scheduler Readiness Model

Work item readiness is scheduler input. An open runnable work item is eligible
for scheduler resume or a system tick, while blocked or waiting items should
pause until their unblock condition changes.

Ending a turn only rests the agent. It does not mark the current work item as
blocked, waiting, or non-runnable. If no immediate progress is possible, call
`WaitFor`:

- use `wake=operator_input` when operator input is required
- use `wake=task_result` with `resource=<task_id>` when waiting on a task
- use `wake=external` with `resource=<external object>` when waiting on an
  outside system such as a PR, CI run, URL, or durable inbox source
- use `wake=timer` with the real `timer_id` returned by the runtime
- use `wake=system` for the next non-work-queue system wake
- use an external trigger when an external system can actively wake the agent

Keep work items runnable only when the next scheduler resume can make useful
progress. This avoids loops where an agent sleeps while leaving an open item
eligible for repeated system ticks.

## Todo List Best Practices

Good todo items are:

- outcome-focused
- durable across turns
- updated after real progress

Avoid:

- logging every tiny shell command
- using the todo list as temporary notes
- leaving stale checklists after the plan changes

## Blocking and Waiting

When a work item cannot proceed:

- call `WaitFor` with a concrete `reason`
- use `wake=operator_input` if operator clarification is required
- use `wake=task_result` or `wake=external` for concrete task or outside waits
- attach external waiting mechanisms only when the work is truly cross-turn

This keeps the scheduler and future turns aligned with reality.

Every `WaitFor` call selects `delivery=final` or `silent`. A yielding final-delivery
path needs a non-empty operator-facing report. Follow the returned
`continuation`: `continue_turn` means continue useful work now,
`yield_and_wait` means wait for the wake, and `yield_and_reenter` means yield for
the queued result. Already-consumed results do not register duplicate waits.

The execution-bound WorkItem owns the wait when one exists; a conflicting
explicit `work_item_id` is ignored and disclosed. Otherwise the runtime resolves
the waiter from an explicit owned open item, the turn-bound item, current focus,
or agent lifecycle. A valid matching wake may resume inspection even when an
independent business blocker remains. It does not clear that blocker or
automatically make ordinary WorkItem work runnable.

See [Events and timers](events-and-timers.md) for callback and timer contracts.

## Relationship to Tasks

Work items and tasks are different:

| Surface | Purpose |
|---------|---------|
| Work item | Tracks the objective and progress |
| Task | Represents running execution such as a command or child agent |

A single work item may create multiple tasks over time. Tasks are execution;
work items are intent and progress.

A task retains the WorkItem owner captured at creation. Picking another item,
creating a new one, or waiting from another item never migrates that task.
`WaitFor(task_result)` can wait on a task owned by any WorkItem of the same agent;
tasks belonging to other agents are rejected.

### Delegated task snapshots

`InvokeAgent(new_subagent)` returns a result-bearing supervised child handle.
`InvokeAgent(existing_agent)` returns a handle satisfied by the first later
durable message from that peer; this is not a business-completion guarantee.
Multiple waits may observe the same peer reply.

Child task status/output snapshots can report cumulative `token_usage`:

```json
{
  "total": {
    "input_tokens": 12450,
    "output_tokens": 3840,
    "total_tokens": 16290
  },
  "total_model_rounds": 5,
  "last_turn": {
    "input_tokens": 2100,
    "output_tokens": 720,
    "total_tokens": 2820
  }
}
```

| Field | Meaning |
|---|---|
| `total` | Cumulative token counts across child turns |
| `total_model_rounds` | Model round-trips made by the child |
| `last_turn` | Token counts for the latest turn, when available |

Task status also provides supported follow-up information. Command-task input
requires input support; supervised child follow-up uses
`child_supervision.followup_target`. A terminal handle cannot be restarted with
`TaskInput`.

Children are retained for reuse after invocation completion. The supervising
parent owns cleanup and must inspect the child's current incarnation and
blockers before deletion. Parent `GetAgent` summaries expose up to 16
`subagent_cleanup` entries. Active execution, unresolved descendants, and
dirty/shared worktrees can block deletion. Open WorkItems or completed tasks
alone do not prevent cleanup once the parent has decided their records are no
longer needed. Optional `reminders_enabled` reminders and
`idle_retirement_enabled` runtime retirement do not delete durable child identity.

## Relationship to Agents

Agents can switch focus between work items, but they should usually maintain one
current tracked objective at a time. If the objective changes meaningfully,
update or switch the work item before doing high-commitment work.

## Common Mistakes

- creating a new work item for every small sub-question
- failing to update the plan after the scope changes
- keeping an item open without recording the blocker
- completing the item without verification evidence

## See Also

- [Runtime Model](/concepts/runtime-model.md) — Work items in the runtime
  lifecycle
- [Run a long-lived task](../guides/run-long-lived-task.md) — Follow one objective through acceptance
- [Multi-Agent Collaboration](/guides/delegate-work.md) — Delegated work and task
  supervision
