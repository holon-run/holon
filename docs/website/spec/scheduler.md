---
title: Scheduler
summary: Current scheduler input, runnable/waiting decisions, WorkItem readiness, and wake/sleep boundaries.
order: 30
---

# Scheduler

This page defines the current contract for Holon's scheduler: what inputs it
consumes, how it derives posture and runnability, and what decisions it emits.
It also documents the additive protocol transition layer that wraps scheduler
decisions in atomic transactions with shadow comparison, semantic decision
plane integration, and a public diagnostic event stream.

> **Last verified:** 2026-07-21 against `src/runtime/scheduler.rs`,
> `src/runtime/scheduler_executor.rs`, `src/runtime/waiting.rs`,
> `src/runtime/closure.rs`, `src/runtime/turn/execution.rs`,
> `src/runtime/runtime_event.rs`, `src/types.rs`.

## Source RFCs

- [Runtime Scheduler Contract](https://github.com/holon-run/holon/blob/main/docs/rfcs/runtime-scheduler-contract.md)
- [Scheduler Wait State And Recoverable Agent Continuation](https://github.com/holon-run/holon/blob/main/docs/rfcs/scheduler-wait-state.md)
- [Waiting Plane And Reactivation](https://github.com/holon-run/holon/blob/main/docs/rfcs/waiting-plane-and-reactivation.md)
- [Continuation Trigger](https://github.com/holon-run/holon/blob/main/docs/rfcs/continuation-trigger.md)
- [Work Item Centered Agent Runtime](https://github.com/holon-run/holon/blob/main/docs/rfcs/work-item-centered-agent-runtime.md)
- [Agent Activation, Settlement, and Dispatch](https://github.com/holon-run/holon/blob/main/docs/rfcs/agent-activation-settlement-and-dispatch.md) — normative target for admission, activation, settlement, and dispatch authority

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
| `Sleep` | Runtime moves the agent to asleep; no immediate action |
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
| `WaitingOperator` | `plan_status=NeedsInput` or operator wait | Agent waits for operator |
| `Blocked` | `blocked_by` set without a more specific wait | Not runnable; check legacy `recheck_at` when present |
| `WaitingTask` | Wait condition on task result | Wake on task terminal |
| `WaitingExternal` | Wait condition on external event | Wake on external trigger |
| `WaitingTimer` | Runtime timer wait | Wake when timer fires |
| `WaitingSystem` | Runtime system-tick wait | Emit system tick |
| `Completed` | `state=Completed` | Excluded from runnable set |

## Wake/sleep boundary

- `Sleep` is an internal scheduler decision after turn closure. The scheduler
  decides whether the agent truly becomes `Asleep` or continues with queued
  work.
- `WaitFor` records explicit wait state and then yields the turn. It is the
  model-facing path for task, external, and operator waits.
- `StayIdle` means the agent is already asleep and the scheduler has nothing
  to do; this is distinct from `Sleep` (the initial transition).
- `EmitSystemTick` injects an internal follow-up message to re-enter the model
  when a runnable WorkItem is found at an idle boundary.
- When `CompleteWorkItem` promotion ends a turn, any remaining runnable
  WorkItem is resumed by the same work-queue `SystemTick` path.
- Wake hints are **liveness signals**: they tell the scheduler to re-evaluate
  but do not themselves carry content for the model.
- Duplicate suppression uses idempotency keys to prevent redundant system
  ticks for the same wake hint or continue-active signal.

## Protocol transition layer

The scheduler decision flow above describes the **legacy** production path.
An additive protocol layer wraps every scheduler boundary in an atomic
`QueueTransitionCommand` transaction that can simultaneously:

1. commit the queue operation (admit, claim, or enqueue);
2. update the agent state projection;
3. persist message evidence, transcript entries, and audit events;
4. record a `SchedulerShadowComparison` between the legacy decision and the
   canonical protocol outcome; and
5. record a `SchedulerSemanticShadowDecision` from the semantic decision
   plane (trusted ingress, provider, response, and policy).

All five effects commit in the same SQLite transaction. If the transaction
fails or the CAS does not match, no partial shadow sample or semantic record
is left behind.

### Rollout modes

The `scheduler_protocol_config` table controls a per-agent rollout state:

| Mode | Behavior |
|------|----------|
| `Legacy` | No protocol persistence; legacy scheduler is sole authority |
| `Shadow` | Legacy scheduler remains authoritative; protocol records comparison and semantic shadow for observability |
| `Authoritative` | Protocol owns admission authority; legacy path is compatibility-only |

Rollout transitions are `Legacy → Shadow → Authoritative` for upgrade and
`Authoritative → Shadow → Legacy` for rollback. The `Authoritative` mode is
currently fail-closed: if production authority is not connected, all
admissions are rejected. This is an MVP gate, not a production cutover.

### Integration points

`QueueTransitionCommand` is committed at every scheduler boundary. Each
boundary records its own shadow comparison between the legacy decision and
the canonical protocol outcome:

| Boundary | Operation | Shadow comparison | Semantic shadow |
|----------|-----------|-------------------|-----------------|
| Message admission (`scheduler_executor::prepare_message`) | `Claim` | Yes | Yes |
| Wait resume (same path, `.or_else`) | `Claim` | Yes | — |
| Settlement recovery (`runtime::commit_queue_settlement`) | `Settle` | Yes | — |
| Delivery disposition (`runtime::commit_queue_settlement`) | `Settle` | Yes (delivery) | — |
| Operator interjection — `AfterProviderRound` | `Admit` | Yes | — |
| Operator interjection — `BeforeToolExecution` | `Admit` | Yes | — |
| Operator interjection — `AfterToolResults` | `Admit` | Yes | — |
| Operator interjection — `BeforeProviderContinuation` | `Admit` | Yes | — |
| Work-queue idle tick (`memory_refresh::emit_system_tick_from_work_queue`) | `Admit` | Yes | — |

The semantic decision plane provides trusted-ingress construction, provider
validation, and response policy. It returns `Ok(None)` when trusted ingress
conditions are not met, preventing observation errors from blocking the run
loop. No provider owns runtime authority; the deterministic resolver and
validator retain all state-transition control. Wait resume shadow comparison
is evaluated within the message admission path via `.or_else`, so the same
`QueueTransitionCommand` transaction records whichever comparison applies.
Settlement recovery and delivery disposition shadow comparisons are recorded
in the same `commit_queue_settlement` transaction.

### Public diagnostic event stream

The scheduler emits a typed `SchedulerDiagnosticAuditEvent` for every
decision that passes through `append_scheduler_decision`. This event carries:

| Field | Content |
|-------|---------|
| `decision` | `SchedulerDecisionKind` variant |
| `reason` | Human-readable decision reason |
| `boundary` | Where the decision was made (e.g. `run_loop`, `after_provider_round`) |
| `message_id` | Optional message that triggered the decision |
| `evidence` | Evidence strings used by the decision |
| `scenario_class` | Optional scenario classification (e.g. `operator_interjection`) |
| `shadow_matched` | Whether legacy and canonical outcomes agreed, when shadow was present |
| `divergence_code` | Optional divergence code if outcomes disagreed |

The event is emitted via `RuntimeEventKind::SchedulerDiagnostic` alongside
the legacy `scheduler_decision` audit event. Both are persisted in the same
transaction as the scheduler decision. The typed event is the public
observability surface; the legacy audit event remains for backward
compatibility.

### Scheduling advisories

`SchedulingAdvisory` is an internal, non-authoritative warning system that
detects potential scheduler state mismatches: idle posture with runnable
work, weak external wait recoverability, unrecoverable blocked WorkItems,
and similar conditions. Advisories are appended as `scheduling_advisory`
audit events with deduplication against recent events.

Advisories are **not** diagnostics in the diagnostic event stream sense.
They are internal hints for debugging and operational awareness; the
deterministic scheduler projection and posture derivation remain the sole
authority for scheduling decisions.

## Known gaps

- `SchedulerDecisionKind` intentionally has more variants than the coarse
  RFC posture labels. The RFC posture is the stable turn-end vocabulary;
  decision variants are concrete runtime actions and duplicate-suppression
  outcomes.
- The `Authoritative` rollout mode is fail-closed and not yet a production
  path; canonical evidence pass-through and full cutover validation remain
  future work (Phase 5h).
