---
title: Work items
summary: Current WorkItem lifecycle, focus, readiness, planning, blocking, and completion contract.
order: 20
---

# Work items

This page defines the current contract for WorkItem runtime behavior:
lifecycle, focus, readiness, planning, blocking, and completion semantics.

> **Last verified:** 2026-05-23 against `src/types.rs` `WorkItemRecord`,
> `WorkItemState`, `WorkItemPlanStatus`, `WorkItemReadiness`,
> `WorkItemSchedulingState`, and the tool implementations in
> `src/tool/tools/{create,update,pick,list,get,complete}_work_item.rs`.

## Source RFCs

- [Work Item Runtime Model](https://github.com/holon-run/holon/blob/main/docs/rfcs/work-item-runtime-model.md)
- [Work Item Centered Agent Runtime](https://github.com/holon-run/holon/blob/main/docs/rfcs/work-item-centered-agent-runtime.md)
- [Objective, Delta, and Acceptance Boundary](https://github.com/holon-run/holon/blob/main/docs/rfcs/objective-delta-and-acceptance-boundary.md)
- [Long-Lived Context Memory](https://github.com/holon-run/holon/blob/main/docs/rfcs/long-lived-context-memory.md)
- [Turn-Local Context Compaction](https://github.com/holon-run/holon/blob/main/docs/rfcs/turn-local-context-compaction.md)

## Core model

A WorkItem is a durable objective record owned by an agent. It tracks:

| Field | Purpose |
|-------|---------|
| `objective` | Short human-readable target (required) |
| `state` | `Open` or `Completed` |
| `plan_status` | `draft`, `ready`, or `needs_input` |
| `plan_artifact` | Path to the durable plan.md artifact in agent home |
| `todo_list` | Progress checklist snapshot |
| `blocked_by` | Human-readable blocker description |
| `recheck_at` | Fallback deadline for blocked re-evaluation |
| `result_summary` | Completion summary (optional) |

The Rust enum `WorkItemPlanStatus` uses PascalCase variants (`Draft`, `Ready`,
`NeedsInput`), but all tool input/output uses snake_case (`draft`, `ready`,
`needs_input`).

## Lifecycle states

```text
                    CreateWorkItem
                          │
                          ▼
                    ┌──────────┐
                    │   Open   │
                    └────┬─────┘
                         │
            ┌────────────┼────────────┐
            ▼            ▼            ▼
       plan_status    plan_status   plan_status
        = Draft       = Ready       = NeedsInput
            │            │               │
            └────────────┼───────────────┘
                         │
                    CompleteWorkItem
                         │
                         ▼
                    ┌───────────┐
                    │ Completed │
                    └───────────┘
```

**Key contract:**

- `state` is the hard lifecycle boundary: `Open` or `Completed`.
- `plan_status` is the planning/coordination posture: whether the plan is
  still being drafted, ready for execution, or waiting for operator input.
- `plan_status=NeedsInput` makes the WorkItem **non-runnable** and means the
  scheduler must wait for operator input.
- `blocked_by` is a human-readable string; when set, the WorkItem is
  non-runnable. A `recheck_after` millisecond fallback deadline can be set
  alongside the blocker via `UpdateWorkItem.recheck_after`. The stored field
  is `recheck_at` (an absolute timestamp).

## Readiness and scheduling

WorkItem readiness is derived from `state`, `plan_status`, `blocked_by`, and
active wait state:

| `WorkItemSchedulingState` | Condition |
|---------------------------|-----------|
| `Runnable` | Open, plan not `NeedsInput`, no blocker, no active wait |
| `WaitingOperator` | `plan_status=NeedsInput` |
| `WaitingTask` | Active wait on a task result |
| `WaitingExternal` | Active wait on an external event |
| `WaitingTimer` | Active wait on a timer |
| `WaitingSystem` | Active wait on a system tick |
| `Blocked` | `blocked_by` is set |
| `Completed` | `state=Completed` |

`WorkItemReadiness` is the reduced view used by scheduler and user display:

| `WorkItemReadiness` | Maps from |
|---------------------|-----------|
| `Runnable` | `WorkItemSchedulingState::Runnable` |
| `WaitingForOperator` | `WorkItemSchedulingState::WaitingOperator` |
| `Blocked` | All other non-completed scheduling states |
| `Completed` | `WorkItemSchedulingState::Completed` |

**Key contract:**

- Readiness is derived, not stored. `is_runnable()` and
  `is_waiting_for_operator()` are computed from current state.
- `plan_status=NeedsInput` is the only explicit "waiting for operator" signal.
- Blocked WorkItems with `recheck_at` carry a fallback deadline; the scheduler
  may re-evaluate them after that time.
- Focus (current/queued) is separate from readiness. A blocked WorkItem can
  still be the current focus for inspection.

## Focus and current work

An agent has at most one **current WorkItem** (`current_work_item_id`). The
current WorkItem is the focus for the current turn:

- `PickWorkItem` sets the current focus from an existing open WorkItem.
- The scheduler may auto-pick a runnable WorkItem when the agent wakes.
- Current focus persists across turns until explicitly changed or completed.
- Only runnable WorkItems are eligible for scheduler auto-resume.

## Tool surface

| Tool | Purpose |
|------|---------|
| `CreateWorkItem` | Create a new open WorkItem with objective, optional plan seed, and todo_list |
| `UpdateWorkItem` | Mutate objective, plan_status, todo_list, blocked_by, recheck_after |
| `PickWorkItem` | Set current focus to an existing open WorkItem |
| `GetWorkItem` | Read a single WorkItem with plan preview |
| `ListWorkItems` | Query with filters: all, open, completed, current, queued, blocked, waiting_for_operator, runnable |
| `CompleteWorkItem` | Mark complete; the next assistant-text block in the same round is promoted as the completion report |

**Key contract:**

- `CreateWorkItem` should only be used for genuinely separate objectives with
  independent lifecycles. Use `UpdateWorkItem` to refine the current WorkItem
  instead of creating a new one for the same task.
- `UpdateWorkItem.todo_list` replaces the full checklist snapshot; it is not
  an append operation.
- `UpdateWorkItem.blocked_by` accepts `null` to clear the blocker.
- `CompleteWorkItem` promotion: the operator-facing completion report must be
  written as assistant text **in the same round**. After the tool succeeds,
  the runtime promotes that text as the canonical completion report.
- Plan body changes go through direct file edits to `plan_artifact.path`, not
  through `UpdateWorkItem`.

## Plan artifact

Each WorkItem has an optional `plan_artifact` pointing to a `plan.md` file in
the agent's home directory (`work-items/<id>/plan.md`). The plan artifact:

- Is **not** stored inline in the WorkItem record.
- Is read by `GetWorkItem` (bounded preview) and editable via
  `ApplyPatch`/file tools.
- Contains the durable prose plan; `todo_list` is the progress checklist.

## Known gaps

- `WorkItemSchedulingState` and `WorkItemReadiness` overlap; `WorkItemReadiness`
  collapses five waiting states into "Blocked", which loses scheduling
  granularity at the display layer.
- Plan artifact path resolution depends on agent home workspace; cross-agent
  WorkItem reads may need explicit workspace routing.
