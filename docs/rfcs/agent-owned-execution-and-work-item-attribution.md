---
title: "RFC: Agent-Owned Execution and WorkItem Attribution"
date: 2026-11-18
status: draft
handle: rfc-agent-owned-execution-and-work-item-attribution
---

# RFC: Agent-Owned Execution and WorkItem Attribution

## Summary

Holon needs two distinct concepts that are currently conflated under "owner":

1. **WorkItem scheduling state (`Runnable | Waiting | Paused | Completed`)**,
   which answers: *"Does this WorkItem want autonomous continuation turns right
   now, or should the scheduler skip it and either run another WorkItem or let
   the agent sleep?"*
2. **Execution and event ownership (Turn, Task, TaskResult, Wait)**, which
   answers: *"Who owns a running turn, and who is entitled to receive a
   completed task result or wake trigger?"*

Today, `WorkItem` (alongside `Conversation` and `AgentLifecycle`) acts as both
a scheduling candidate **and** the exclusive owner of Turns, Tasks,
TaskResults, and Waits. That turns WorkItem's scheduling state into an **input
delivery firewall**: when a WorkItem is `Waiting` on `task2`, `Paused`, or
`Completed`—or when a non-WorkItem turn ends—the runtime refuses to deliver or
drops `task1`'s terminal result.

This RFC decouples **WorkItem scheduling state** from **Turn/Task/Wait
ownership**:

1. **`WorkItem` keeps its scheduling state machine (`Runnable | Waiting |
   Paused | Completed`)** so the scheduler can schedule across multiple
   WorkItems (continuing `Runnable` ones and skipping `Waiting` or `Paused`
   ones).
2. **The Agent is the sole owner of Turns (`ExecutionAttempt`), Tasks,
   `TaskResult` obligations, and `WaitCondition`s.** `work_item_id` and
   `interaction_id` on Turns, Tasks, and Waits are **attribution and
   correlation links**, not delivery gates.
3. **`WorkItemExecutionState::InFlight` is removed.** Only the single agent
   lane is in-flight (`ExecutionAttempt::Open`). A single Turn may create,
   switch, yield, or complete WorkItems without invalidating the Turn's own
   execution fence.
4. **Agent-spawned terminal task results are agent-level wake events.** A
   validated terminal result (`Succeeded`, `Failed`, `Cancelled`) wakes an idle
   or sleeping agent to run a Turn regardless of which state any WorkItem is
   in, without losing or consuming unrelated active waits on `Waiting`
   WorkItems.

## Motivation

### 1. Why WorkItem needs a scheduling state, and why it should not own Turns or Tasks

An agent may maintain several open WorkItems (`W1`, `W2`, `W3`). When `W1` is
waiting on an external event or long-running task (`Waiting`) or blocked on
operator input (`Paused`), the scheduler must know that `W1` is not asking for
autonomous continuation turns so it can schedule `W2` (`Runnable`) or let the
agent sleep if no WorkItem is `Runnable`.

However, making `WorkItem` (or `AgentLifecycle`) the **exclusive owner** of
Turns, Tasks, and Waits goes further than candidate selection—it couples four
unrelated mechanisms to a single WorkItem's current state:

1. **Task-result delivery is gated by WorkItem readiness**: a `TaskResult`
   tagged with `W1` cannot be delivered while `W1` is `Waiting` (on another
   wait), `Paused`, or `Completed`.
2. **Non-WorkItem tasks are gated by a dead anchor turn**: a task started in a
   `Conversation` or `AgentLifecycle` turn is bound to `parent_turn_id`
   (`anchor_turn_live`). Once that turn finishes and its single wait slot is
   empty or replaced, the result has no live owner and is settled as
   `invalid_or_stale`.
3. **Turn lifecycle is locked to a single WorkItem fence**: `AdmitExecution`
   puts `W1` into `InFlight(g, attempt_id)` and requires a matching
   `WorkItemOutcome` at settlement. If the model calls `CompleteWorkItem` or
   switches focus during the turn, the turn's own completion fence can clash
   with the in-turn mutation (e.g., `#3620`).
4. **Single wait slot per owner**: "at most one unresolved wait per owner"
   causes a second `WaitFor` on the same owner to cancel the first wait.

### 2. Concrete failure: lost or stranded task wakes across turns

Consider a standard flow:

1. **Turn 1**: The agent starts `task1` and calls `WaitFor(task1)`.
2. **Turn 2**: The agent is woken by another trigger (e.g., an operator prompt
   or timer), and at the end of Turn 2 either replies without waiting or calls
   `WaitFor(task2)`.
3. **`task1` finishes** (succeeded, failed, or cancelled) and emits its
   `TaskResult`.

Under the current owner-gated rules:

- Calling `WaitFor(task2)` on the same owner cancels `wait1`.
- When `task1` finishes, no active wait matches `task1`. Scheduler claim
  reduces the queue entry to `ReduceOnly`.
- If `task1` belonged to `AgentLifecycle`, Turn 1 is terminal and no exact
  waiter exists: the recovery scanner converges `task1`'s result to
  `invalid_or_stale` and **never invokes the LLM**.
- If `task1` belonged to `W1` and `W1` is now `Waiting` on `task2`, `W1` is
  "not runnable" and has an "unresolved wait": `task1`'s result is held behind
  `task2` indefinitely.

When `TaskResult` ownership belongs to the **Agent** while `Waiting` remains a
scheduling state of the **WorkItem**, this conflict disappears: `task1`
finishing is an agent-level event that wakes the agent for a Turn; after that
Turn ends, if `W1`'s wait on `task2` was not satisfied or changed, `W1` simply
remains `Waiting(wait2)`.

## Updated Authority Matrix

This matrix updates
[Scheduler–WorkItem Unified Execution Protocol](./scheduler-work-item-unified-execution-protocol.md#authority-matrix):

| Question | Sole Authority | Attribution or Projection Only |
| --- | --- | --- |
| Is the agent currently executing a turn? | one open `ExecutionAttempt` (agent-owned) | UI `executing` badge on focused WorkItem |
| Does WorkItem `W` want autonomous continuation turns? | `WorkItemExecutionState` (`Runnable \| Waiting \| Paused \| Terminal`) | plan, todo, focus |
| Which `TaskResult`s owe model delivery? | `TaskResultSettlement` ledger (agent-owned) | `work_item_id` / `parent_turn_id` attribution |
| What explicit conditions are being waited on? | `WaitCondition` records (agent-owned, optional `work_item_id` link) | closure summary |
| Which WorkItem is currently in focus? | durable agent focus + continuation stack | `TurnAttribution.work_item_id` |
| Which WorkItem or Conversation does a Turn belong to for context/history? | `TurnAttribution` (`work_item_id`, `interaction_id`) | scheduling eligibility |
| May the host or control plane admit execution? | host phase + agent control posture (`Running \| Paused \| Stopped`) | WorkItem states |

## Core Model

### 1. Two Scheduling Drivers: Event-Driven vs Work-Driven

The scheduler activates the agent lane (`AdmitExecution`) from two kinds of
sources:

1. **Event-Driven Sources (Agent-Scoped)**
   - Queued operator input or interjection
   - Triggered `WaitCondition` (timer, external trigger, subagent reply, task)
   - Deliverable `TaskResult` (`RuntimeEventReentry`: any unsettled terminal
     result of a task created by this agent)
   - Internal/system follow-up

   Event-driven sources wake the agent whenever the agent lane is free and the
   agent control posture allows execution, **regardless of whether any
   WorkItem is `Runnable`, `Waiting`, `Paused`, or `Completed`**.

2. **Work-Driven Sources (WorkItem-Scoped)**
   - A `WorkItem` whose `WorkItemExecutionState` is `Runnable(g)`.

   Work-driven continuation runs when no higher-priority event-driven source is
   claiming the lane and an eligible `Runnable` WorkItem exists (preferring
   the focused WorkItem, or surfacing runnable candidates per focus rules).

### 2. WorkItem Execution State Machine (Without `InFlight`)

`WorkItemExecutionState` governs **work-driven scheduling eligibility only**:

```text
WorkItemExecutionState =
  | Runnable(g)
  | Waiting(g, wait_ids)
  | Paused(g, reason)
  | Terminal(g, completion)
```

Key changes from the current state machine:

- **`InFlight(g, attempt_id)` is removed.** The open `ExecutionAttempt` on the
  agent is the sole authority for whether a turn is running. During a turn, the
  agent is free to create, update, switch (`PickWorkItem`), yield, or complete
  any WorkItem without fighting an `InFlight` ownership lock on the turn.
- **`Waiting(g, wait_ids)` means "do not schedule work-driven continuation for
  this WorkItem until one of `wait_ids` triggers (or the model/operator
  explicitly resumes it)."** It does **not** block the agent from running
  event-driven turns (such as delivering an earlier `task1` result or replying
  to an operator prompt).
- **When an event-driven turn runs while `W1` is `Waiting(g, wait2)`**:
  - If the turn does not modify `W1` or resolve `wait2`, `W1` stays in
    `Waiting(g, wait2)` when the turn settles.
  - If the turn satisfies `wait2` (or the model updates/resumes `W1` or
    registers a new wait for `W1`), `W1` transitions to `Runnable(g + 1)` or
    `Waiting(g + 1, new_wait_ids)` as part of `SettleExecution`.
- **`WorkItemRecord.blocked_by` is a descriptive note only.** It is natural-language
  context surfaced in prompts and UI projections to explain why a WorkItem is
  blocked; it does not act as a scheduler gate or tool lock.

### 3. Agent-Owned Turns and Unified Settlement

Every model turn is owned by the agent:

```text
ExecutionAttempt {
  attempt_id: AttemptId,
  agent_id: AgentId,
  source: ExecutionSource,
  attribution: TurnAttribution,
  state: Open | Settled | Interrupted | ProtocolViolation,
  ...
}

TurnAttribution {
  work_item_id: Option<WorkItemId>,
  interaction_id: Option<InteractionId>,
}
```

- `TurnAttribution` records the `work_item_id` (from the triggering source or
  current focus) and/or `interaction_id` for context projection and history.
- `SettleExecution` commits:
  1. the attempt's terminal outcome (`Continue | Idle | Wait | Pause | Failed |
     Interrupted`);
  2. any WorkItem state transitions produced by the turn (`Runnable`,
     `Waiting`, `Paused`, `Terminal`);
  3. any `WaitCondition` registrations or resolutions; and
  4. `TaskResultSettlement` consumption for results delivered in this turn.

Because the Turn is agent-owned, `ConversationOutcome` and `WorkItemOutcome` no
longer need to be separate incompatible worlds: a turn triggered by an operator
message or a task result can update or complete a WorkItem, and a turn
triggered by a WorkItem can reply or wait without a protocol violation.

### 4. Agent-Owned Tasks, Waits, and TaskResult Delivery

1. **TaskResult ownership is Agent-scoped.**
   Every terminal outcome (`Succeeded`, `Failed`, `Cancelled`, `Interrupted`)
   of an agent-spawned task creates a `TaskResultSettlement` record owned by
   `agent_id`, with `work_item_id` and `parent_turn_id` kept as attribution.
   Completing a WorkItem (`W1 -> Completed`) does **not** discard or settle
   unconsumed results of tasks attributed to `W1`; those results are still
   delivered to the agent with `W1 (Completed)` attribution so background
   failures or side effects are never silently swallowed.
2. **Terminal task results have independent wake authority (`RuntimeEventReentry`).**
   - `Succeeded`, `Failed`, `Interrupted`, timeout, and operator-initiated
     `Cancelled` results are event-driven activation sources for `agent_id`,
     even if `WaitFor(task1)` was never called or was later superseded by
     `WaitFor(task2)`.
   - When a task is cancelled by the model itself via `StopTask` in a later
     turn (and no active `WaitCondition` still references that `task_id`), the
     resulting `Cancelled` record does **not** wake an idle or sleeping agent on
     its own; instead, it piggybacks on the next turn admitted for `agent_id`.
3. **Wait replacement without blinding earlier tasks.**
   - `WaitCondition`s belong to `agent_id` with optional `work_item_id`
     correlation (linking the wait to the `Waiting` state of that WorkItem).
   - Calling `WaitFor(cond2)` replaces the prior unresolved `WaitCondition` for
     the same scope (`work_item_id` or agent-level) so each scope has one clear
     explicit waiting condition.
   - Replacing `wait1` with `wait2` does not blind the agent to `task1`: when
     `task1` finishes, `task1`'s `TaskResult` wakes the agent via
     `RuntimeEventReentry` and enters the LLM. After that turn finishes, if
     `wait2` is still active, `W1` remains `Waiting` on `wait2`.
4. **Piggybacking (Batching up to 8).**
   Whenever `AdmitExecution` starts a turn for `agent_id` for any reason, it
   binds up to `LIVE_TASK_RESULT_CANDIDATE_LIMIT` (8) deliverable pending
   `TaskResult`s for `agent_id` in the same transaction (ordered by triggered
   wait match first, then completion time), including each result's
   `work_item_id` attribution in the rendered prompt.

### 5. Single Decision Function (`next_step`) for TaskResult

Because `TaskResult` delivery is no longer gated by WorkItem execution state or
`anchor_turn_live`, `#4103`'s pure decision function depends only on the result
and agent-level facts:

```rust
pub enum DeliveryMode {
    /// May start an event-driven turn on its own and attaches to any turn.
    WakeAndAttach,
    /// Attaches when the next turn starts, without waking an idle/sleeping agent
    /// on its own (used for model-initiated StopTask with no active wait).
    AttachOnly,
}

pub enum Step {
    Deliver { mode: DeliveryMode },
    Hold { reason: HoldReason, until: Vec<Trigger> },
    Settle { disposition: TaskResultSettlementDisposition },
    Park { diagnostic: Diagnostic },
}

pub enum Trigger {
    AttemptSettled,
    AgentControlChanged,
    ExactWaiterRegistered,
    ReplyQueueEntryTerminal,
    BackoffDeadline(DateTime<Utc>),
    OperatorRearm,
}

fn next_step(
    result: &ResultFacts,
    agent: &AgentFacts,
    now: DateTime<Utc>,
) -> Step
```

Decision table:

| Result & Agent Facts | `Step` |
| --- | --- |
| Result dependency or durable task relation is invalid | `Settle(InvalidOrStale)` |
| Reply message was already consumed by another turn | `Settle(ReplyConsumedElsewhere)` |
| Agent control status is `Stopped` or `Paused` | `Hold(until: [AgentControlChanged])` |
| Agent has an open `ExecutionAttempt` | `Hold(until: [AttemptSettled])` |
| Reply queue entry is still queued for first delivery | `Hold(until: [ReplyQueueEntryTerminal])` |
| Reply queue entry was dropped and no exact waiter exists | `Hold(until: [ExactWaiterRegistered, OperatorRearm])` |
| Result is within recovery backoff window | `Hold(until: [BackoffDeadline(deadline)])` |
| Recovery budget is exhausted | `Park` |
| Model-initiated `StopTask` cancellation with no matching active wait | `Deliver { mode: AttachOnly }` |
| Otherwise (agent lane is free, regardless of WorkItem states) | `Deliver { mode: WakeAndAttach }` |

All WorkItem-owner and anchor-turn rejection branches (`owner_closed`,
`owner_missing`, `owner_not_runnable`, `owner_has_unresolved_wait`,
`claimed_by_other_waiter`, and terminal `anchor_turn_live` convergence) are
removed from `TaskResult` scheduling.

## Relationship to Existing RFCs and Issues

- **Amends [Scheduler–WorkItem Unified Execution Protocol](./scheduler-work-item-unified-execution-protocol.md)**:
  keeps `WorkItemExecutionState` (`Runnable | Waiting | Paused | Terminal`) for
  work-driven scheduling, removes `InFlight` from `WorkItemExecutionState`, and
  moves Turn/Task/Wait ownership to the Agent.
- **Amends [Activation and Turn Owner Identity](./activation-turn-owner-identity.md)**:
  replaces mutually exclusive `WorkItem | Conversation | AgentLifecycle`
  execution owners with a single `Agent` execution owner plus
  `TurnAttribution { work_item_id, interaction_id }`.
- **Preserves [WorkItem Scheduling Read Model](./work-item-scheduling-read-model.md)**:
  `Executing` on a WorkItem continues to be a read-model projection derived
  from the open `ExecutionAttempt` and turn attribution/focus.
- **Incorporates [Wake Authority And Resume Authorization](./wake-authority.md)**:
  makes `RuntimeEventReentry` for agent-spawned terminal task results part of
  canonical admission.
- **Directly simplifies `#4103` ([Agent loop 与输入处理的简化方案](../rfc-implementation-notes/agent-loop-and-input-simplification.md)) and resolves `#3620`**.

## Design Decisions

1. **Model-initiated `StopTask` piggybacks; all other terminal outcomes wake.**
   When the model itself calls `StopTask(task1)` in a subsequent turn, it
   already knows it requested cancellation; if no active wait references
   `task1`, the resulting `Cancelled` record uses `DeliveryMode::AttachOnly`
   (piggybacking on the next turn without waking a sleeping or idle agent on
   its own). `Succeeded`, `Failed`, `Interrupted`, timeout, and operator
   cancellations use `DeliveryMode::WakeAndAttach`.
2. **Results for a `Completed` WorkItem are still delivered.**
   Completing `W1` before `task1` finishes does not settle `task1`'s result as
   `owner_closed`. The result is delivered to the agent with attribution
   `W1 (Completed)` so background errors or side effects remain visible.
3. **`WaitFor` replaces the prior wait for the same scope.**
   Calling `WaitFor(cond2)` replaces the prior unresolved `WaitCondition` on
   the same scope (`work_item_id` or agent-level), keeping a single explicit
   wait target per scope while relying on `RuntimeEventReentry` (§4) so any
   earlier agent-spawned task reaching a terminal state still wakes the agent.
4. **`WorkItem.blocked_by` is purely descriptive.**
   `blocked_by` is a natural-language explanation surfaced in context and UI; it
   does not act as a scheduler gate or tool execution lock.

## Incremental Migration Plan

Because `WorkItemExecutionState` (`Runnable | Waiting | Paused | Terminal`) is
preserved for work-driven scheduling, the migration is much smaller and can be
done in three focused phases:

1. **Phase 1: Decouple `TaskResult` delivery from WorkItem / Anchor-Turn owner
   gates (unblocks `#4103` Steps C–D).**
   - Treat unsettled terminal `TaskResult`s as agent-owned event-driven wake
     sources (`RuntimeEventReentry`) that can wake an idle or waiting agent
     without consuming unrelated active `WaitCondition`s.
   - Remove `anchor_turn_live` convergence and `owner_closed` /
     `owner_not_runnable` / `owner_has_unresolved_wait` gates from recovery and
     admission; allow cross-WorkItem piggybacking up to 8 results per turn.
   - Land `#4103` Steps C–D (`next_step` + closed `Trigger` set) on this
     9-row decision table.
2. **Phase 2: Make `ExecutionAttempt` Agent-owned and remove
   `WorkItemExecutionState::InFlight` (resolves `#3620`).**
   - Move `work_item_id` and `interaction_id` into `TurnAttribution` on
     `ExecutionAttempt` and `TurnRecord`.
   - Remove `InFlight` from `WorkItemExecutionState`; compute `Executing` in
     the read model from the open `ExecutionAttempt`.
   - Allow in-turn `CompleteWorkItem` / `PickWorkItem` / `WaitFor` to settle
     cleanly through a unified `TurnOutcome` + WorkItem state transition.
3. **Phase 3: Clean up legacy owner-scoped wait/settlement invariants.**
   - Remove obsolete per-owner recovery wake keys and redundant owner-matching
     fences in `scheduler_executor.rs` and `task_result_settlement`.
