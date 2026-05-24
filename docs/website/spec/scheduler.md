---
title: Scheduler
summary: Current scheduler input, runnable/waiting decisions, WorkItem readiness, and wake/sleep boundaries.
order: 30
---

# Scheduler

This page defines the current contract for Holon's scheduler: what inputs it
consumes, how it derives posture and runnability, and what decisions it emits.

> **Last verified:** 2026-05-23 against `src/runtime/scheduler.rs`,
> `src/runtime/scheduler_executor.rs`, `src/runtime/waiting.rs`,
> `src/runtime/closure.rs`.

## Source RFCs

- [Runtime Scheduler Contract](https://github.com/holon-run/holon/blob/main/docs/rfcs/runtime-scheduler-contract.md)
- [Scheduler Wait State And Recoverable Agent Continuation](https://github.com/holon-run/holon/blob/main/docs/rfcs/scheduler-wait-state.md)
- [Waiting Plane And Reactivation](https://github.com/holon-run/holon/blob/main/docs/rfcs/waiting-plane-and-reactivation.md)
- [Continuation Trigger](https://github.com/holon-run/holon/blob/main/docs/rfcs/continuation-trigger.md)
- [Work Item Centered Agent Runtime](https://github.com/holon-run/holon/blob/main/docs/rfcs/work-item-centered-agent-runtime.md)

## Core model

The scheduler is the runtime component that answers: given the current agent
state, what should happen next?

It consumes a `SchedulerProjection` — a snapshot assembled from:

| Input | Source |
|-------|--------|
| Agent status | `AgentState.status` |
| Queue depth | `AgentState.pending` |
| Active tasks | `TaskRecord`s with non-terminal status |
| Current WorkItem | `current_work_item_id` → `WorkItemRecord` |
| Runnable WorkItems | Open WorkItems with `is_runnable()=true` |
| Wait conditions | Active `WaitConditionRecord`s |
| Waiting intents | Active `WaitingIntentRecord`s |
| Wake hints | `PendingWakeHint` |
| Turn state | `turn_in_progress`, `last_turn_terminal` |
| Runtime errors | `runtime_error_active()` |

The projection is a **read-only snapshot**; the scheduler never mutates
durable state directly. Decisions are emitted and handed to the executor.

## Scheduler inputs (`SchedulerInput`)

| Input variant | Trigger |
|---------------|---------|
| `Message` | A new message arrived in the agent's queue |
| `IdleSignal::WakeHint` | A pending wake hint was received |
| `IdleSignal::ContinueActive` | A WorkItem was runnable at the last closure |
| `IdleSignal::QueuedAvailable` | A queued message is ready for processing |
| `Idle` | Periodic idle boundary check |

## Scheduler decisions (`SchedulerDecisionKind`)

| Decision | Meaning |
|----------|---------|
| `StartModelTurn` | Start a new model turn with context assembly |
| `ReduceMessageOnly` | Reduce a message without starting a full model turn |
| `EmitSystemTick` | Emit a runtime-owned follow-up message (system tick) |
| `WaitForTask` | Block until a non-terminal task completes |
| `WaitForExternalChange` | Block until an external event arrives |
| `WaitForTimer` | Block until a timer fires |
| `WaitForOperator` | Block until operator input arrives |
| `Sleep` | Agent sleeps; no immediate action, wait for wake signal |
| `StayIdle` | Agent is already asleep; no action |
| `Stop` | Agent is stopped; no scheduling possible |
| `Noop` | No action (duplicate suppressed, turn in progress) |

Each decision carries metadata: `reason`, `model_reentry`, `liveness_only`,
`work_item_id`, `task_id`, and `evidence`.

## Decision flow

```text
                    SchedulerInput
                         │
                         ▼
              ┌─────────────────────┐
              │ Status == Stopped?  │──Yes──► Stop
              └─────────┬───────────┘
                        │ No
                        ▼
              ┌─────────────────────┐
              │ Turn in progress?   │──Yes──► Noop
              └─────────┬───────────┘
                        │ No
                        ▼
         ┌──────────────────────────┐
         │ Queue has pending input? │──Yes──► StartModelTurn
         └──────────────┬───────────┘        (or ReduceMessageOnly)
                        │ No
                        ▼
         ┌──────────────────────────┐
         │ Runnable WorkItem?       │──Yes──► EmitSystemTick
         └──────────────┬───────────┘        (ContinueActive)
                        │ No
                        ▼
         ┌──────────────────────────┐
         │ Active wait condition?   │──Yes──► WaitFor{Task,
         └──────────────┬───────────┘         External,Timer,Operator}
                        │ No
                        ▼
                      Sleep
```

## WorkItem scheduling states

WorkItems flow through scheduling states that the scheduler consumes:

| State | Meaning | Scheduler action |
|-------|---------|-----------------|
| `Runnable` | Ready for processing | May be auto-picked as current |
| `WaitingOperator` | `plan_status=NeedsInput` | Agent waits for operator |
| `Blocked` | `blocked_by` set | Not runnable; check `recheck_at` |
| `WaitingTask` | Wait condition on task result | Wake on task terminal |
| `WaitingExternal` | Wait condition on external event | Wake on external trigger |
| `Completed` | `state=Completed` | Excluded from runnable set |

## Wake/sleep boundary

- `Sleep` is a scheduler decision (not a tool). The model calls `Sleep` to
  signal turn-end; the scheduler then decides whether the agent truly becomes
  `Asleep` or continues with queued work.
- `StayIdle` means the agent is already asleep and the scheduler has nothing
  to do; this is distinct from `Sleep` (the initial transition).
- `EmitSystemTick` injects an internal follow-up message to re-enter the model
  when a runnable WorkItem is found at an idle boundary.
- Wake hints are **liveness signals**: they tell the scheduler to re-evaluate
  but do not themselves carry content for the model.
- Duplicate suppression uses idempotency keys to prevent redundant system
  ticks for the same wake hint or continue-active signal.

## Known gaps

- `WaitingTimer` and `WaitingSystem` variants exist in both
  `WorkItemSchedulingState` and scheduler decisions (`wait_decision_for_projection`)
  without RFC coverage. The RFC defines `WaitingOperator, WaitingTask,
  WaitingExternal, Blocked` but not these timer/system variants. See
  [issue #1380](https://github.com/holon-run/holon/issues/1380).
- `idle_boundary_decision` (`scheduler.rs:903`) gates on
  `AgentStatus::Asleep` before inspecting work facts, which can strand
  runnable work behind an `Asleep` status check. The RFC says scheduling
  should derive from WorkItem/wait facts, not lifecycle status labels. See
  [issue #1380](https://github.com/holon-run/holon/issues/1380).
- `SchedulerDecisionKind` has 11 variants (`StartModelTurn, ReduceMessageOnly,
  EmitSystemTick, WaitForTask, …`) while the RFC suggests ~6 high-level
  posture outcomes. The added granularity is useful but the RFC vocabulary is
  too coarse to describe current behavior. See
  [issue #1380](https://github.com/holon-run/holon/issues/1380).
- The scheduler does not yet expose a public diagnostic event stream for
  observability; diagnostic events exist but are audit-only.
