---
title: Work items guide
summary: Durable objective tracking with work items, plans, todo lists, and lifecycle management.
order: 45
---

# Work Items Guide

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
- **completed** — explicitly finished
- **current** — the agent's active focus
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
- **plan status** — `draft`, `ready`, or `needs_input`
- **todo list** — checklist of meaningful progress steps
- **blocked by** — specific blocker when progress cannot continue
- **recheck deadline** — fallback time for reconsidering a blocked item

Treat the work item as coordination state, not a scratchpad.

## Core Operations

Typical work-item operations:

- `CreateWorkItem` — create a new tracked objective
- `PickWorkItem` — make an open item the current focus
- `UpdateWorkItem` — refine objective, plan status, blocker, or todo list
- `ListWorkItems` — inspect current, open, blocked, or completed work
- `GetWorkItem` — inspect one work item in detail
- `CompleteWorkItem` — mark the objective done

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

`Sleep` only rests the agent. It does not mark the current work item as blocked,
waiting, or non-runnable. If no immediate progress is possible, call `WaitFor`
instead of plain `Sleep`:

- use `wake=operator_input` when operator input is required
- use `wake=task_result` with `resource=<task_id>` when waiting on a task
- use `wake=external` with `resource=<external object>` when waiting on an
  outside system such as a PR, CI run, URL, or durable inbox source
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

## Relationship to Tasks

Work items and tasks are different:

| Surface | Purpose |
|---------|---------|
| Work item | Tracks the objective and progress |
| Task | Represents running execution such as a command or child agent |

A single work item may create multiple tasks over time. Tasks are execution;
work items are intent and progress.

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
- [Quick Examples](/guides/quick-examples.md) — Common command patterns
- [Multi-Agent Collaboration](/guides/multi-agent.md) — Delegated work and task
  supervision
