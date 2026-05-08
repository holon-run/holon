---
title: RFC: Runtime Scheduler Contract
date: 2026-05-08
status: draft
---

# RFC: Runtime Scheduler Contract

## Summary

Holon's scheduler should become an explicit runtime contract instead of an
emergent behavior spread across queue handling, task reducers, work-item
reactivation, waiting intents, turn execution, and context compaction.

The direction is:

- scheduler decisions are derived from typed runtime facts;
- `AgentState.status` is a projection, not an authority by itself;
- task, work-item, waiting, wake, and compaction transitions have explicit
  boundaries;
- every scheduler-sensitive transition is testable through pure reducers and
  replayable ledger fixtures.

This RFC does not propose a large rewrite as the first step. It defines the
contract Holon should converge toward and the tests that should guard the
existing implementation while it is refactored.

## Problem

Holon is headless, event-driven, and long-lived. Scheduler bugs therefore have
a disproportionate impact on user experience:

- the agent may appear stuck while useful work is available;
- the agent may keep re-entering itself with duplicate work-queue ticks;
- completed tasks may continue to block progress;
- work items may drift after compaction;
- restarts may replay work in surprising ways;
- the TUI and API may disagree about what the agent is waiting on.

Recent task-completion fixes exposed the underlying design issue: scheduling
state is currently written from several places. Queue dequeue, message
processing, task transitions, command-task runners, work-item tools, system
ticks, control actions, and shutdown paths can all mutate pieces of the same
runtime posture.

That makes local fixes possible, but it makes global correctness hard to
reason about.

## Goals

- define one scheduler vocabulary for runtime inputs, decisions, and
  projections;
- make wake/sleep/wait/task/work-item behavior explainable from runtime facts;
- make scheduler behavior reproducible from append-only ledgers;
- prevent terminal tasks, completed work items, stale waits, or repeated ticks
  from corrupting the active runtime posture;
- keep context compaction from becoming an implicit scheduler mechanism;
- preserve existing public behavior while giving future refactors a clear
  target.

## Non-goals

- do not replace the provider turn loop in this RFC;
- do not redefine the WorkItem schema beyond scheduler-relevant boundaries;
- do not make the model the authority for closure or completion;
- do not introduce a UI-first plan mode;
- do not require remote provider compaction to become part of Holon's local
  scheduler state;
- do not remove append-only runtime ledgers.

## Terms

### Message

A queued ingress unit. Messages include operator prompts, task results, timer
ticks, system ticks, callback events, channel events, and internal follow-ups.

Messages are inputs to scheduling. They are not themselves proof that a model
turn should run.

### Turn

One conversational execution pass with a provider. A turn may contain multiple
provider/tool rounds.

Turns own:

- provider request construction;
- assistant round recording;
- tool execution inside the turn;
- turn-local compaction and checkpoints;
- a terminal record.

Turns do not own:

- high-level work identity;
- background task truth;
- long-lived wait truth.

### Task

A concrete operational execution unit such as a command task or supervised
child-agent task.

Tasks own:

- operational lifecycle;
- output retrieval;
- terminal result delivery;
- cancellation and restart recovery.

Tasks do not own high-level objective state.

### WorkItem

The goal-oriented unit of ongoing work.

Work items own:

- objective;
- durable plan;
- todo list;
- work-level blocker;
- completion summary.

Work items are scheduler inputs, but they should not be hidden scheduler state.

### Waiting Intent

A durable future-condition record, usually anchored to a work item.

Waiting intents explain why progress depends on a timer, callback, external
change, or operator input.

### Wake Hint

A liveness signal that tells the runtime to reconsider scheduling. A wake hint
does not automatically become rich model-visible content.

### Closure

The derived outcome of current runtime facts:

- completed;
- continuable;
- failed;
- waiting.

Closure is evidence-driven and remains separate from runtime posture.

### Runtime Posture

The runtime's execution stance:

- booting;
- awake idle;
- awake running;
- awaiting task;
- asleep;
- paused;
- stopped.

Posture is a projection of scheduler facts plus control state.

## Scheduler Inputs

The scheduler should consume typed inputs. Current code may still receive these
inputs through existing modules, but scheduler-sensitive mutations should be
mapped into this vocabulary.

### Message Inputs

- `MessageQueued`
- `MessageDequeued`
- `MessageProcessed`
- `MessageInterrupted`
- `MessageDropped`

Required fields:

- `message_id`
- `message_kind`
- `priority`
- `origin`
- `trust`
- `work_item_id`
- `task_id`
- `correlation_id`
- `causation_id`

### Turn Inputs

- `TurnStarted`
- `ProviderRoundCompleted`
- `AssistantRoundRecorded`
- `ToolRoundCompleted`
- `TurnTerminal`
- `TurnInterrupted`
- `TurnBaselineOverBudget`

Required fields:

- `turn_index`
- `run_id`
- `message_id`
- `terminal_kind`
- `last_assistant_message`
- `checkpoint`
- token and timing diagnostics when available

### Task Inputs

- `TaskCreated`
- `TaskRunning`
- `TaskCancelling`
- `TaskCompleted`
- `TaskFailed`
- `TaskCancelled`
- `TaskInterrupted`
- `TaskResultQueued`
- `TaskResultDelivered`

Required fields:

- `task_id`
- `task_kind`
- `task_status`
- `wait_policy`
- `work_item_id` when known
- `recovery`
- `summary`
- terminal detail when available

### WorkItem Inputs

- `WorkItemCreated`
- `WorkItemPicked`
- `WorkItemUpdated`
- `WorkItemBlocked`
- `WorkItemUnblocked`
- `WorkItemCompleted`
- `WorkItemDelegated`
- `WorkItemDelegationCompleted`

Required fields:

- `work_item_id`
- `state`
- `readiness`
- `objective`
- `plan_status`
- `blocked_by`
- generation or updated-at marker

### Waiting Inputs

- `WaitingIntentCreated`
- `WaitingIntentTriggered`
- `WaitingIntentCancelled`
- `TimerCreated`
- `TimerFired`
- `TimerCompleted`
- `WakeHintSubmitted`
- `WakeHintCoalesced`
- `WakeHintIgnored`

Required fields:

- `waiting_intent_id`
- `scope`
- `work_item_id`
- `source`
- `resource`
- `delivery_mode`
- `trigger_count`

### Control Inputs

- `PauseRequested`
- `ResumeRequested`
- `StopRequested`
- `ShutdownRequested`
- `RuntimeRestarted`

Control inputs are authoritative for posture. They should not erase task,
work-item, or waiting facts.

## Scheduler State

The scheduler state should be a derived projection over durable facts, not a
second independent source of truth.

Recommended shape:

```rust
struct SchedulerState {
    control_posture: ControlPosture,
    queue: QueueProjection,
    active_run: Option<RunProjection>,
    active_tasks: Vec<TaskProjection>,
    active_work_item: Option<WorkItemProjection>,
    queued_work_items: Vec<WorkItemProjection>,
    waiting_intents: Vec<WaitingIntentProjection>,
    timers: Vec<TimerProjection>,
    pending_wake_hint: Option<WakeHintProjection>,
    last_turn_terminal: Option<TurnTerminalProjection>,
    runtime_error: Option<RuntimeErrorProjection>,
}
```

The important point is not this exact struct. The important point is that the
scheduler decision can be derived from one explicit projection.

## Scheduler Decisions

The scheduler should produce one explicit decision at each boundary:

- `StartModelTurn`
- `ReduceMessageOnly`
- `EmitSystemTick`
- `WaitForTask`
- `WaitForExternalChange`
- `WaitForTimer`
- `WaitForOperator`
- `Sleep`
- `StayIdle`
- `Stop`
- `Noop`

Decisions should include evidence:

- which facts caused the decision;
- which message or work item is involved;
- whether the decision is model-visible;
- whether it is a liveness-only decision;
- whether any input was ignored as mismatched.

## Decision Priority

Scheduler decisions should use a fixed priority order.

1. `stopped` or shutdown means `Stop`.
2. `paused` means no model turn starts.
3. queued interrupt operator input may interrupt a running turn.
4. a queued model-visible message may start a turn when no turn is running.
5. terminal blocking task result may resume the model-visible wait it satisfies.
6. active blocking tasks mean `WaitForTask`.
7. active waiting intents mean `WaitForExternalChange`.
8. active timers mean `WaitForTimer`.
9. current runnable work item means `EmitSystemTick(continue_active)` unless an
   idempotency key has already been emitted for the same generation.
10. queued runnable work item means `EmitSystemTick(queued_available)` unless an
    idempotency key has already been emitted for the same generation.
11. pending wake hint means `EmitSystemTick(wake_hint)`.
12. no runnable work and no pending inputs means `Sleep` or `StayIdle`,
    depending on host mode.

This order intentionally keeps blocking facts above work-queue self-reactivate
ticks.

## Model Visibility

Not every scheduler input should run a provider turn.

Model-visible inputs:

- operator prompt;
- contentful external event;
- timer tick with contentful resume text;
- runtime-owned internal follow-up intended for the model;
- terminal blocking task result.

Liveness-only inputs:

- wake hint without contentful body;
- non-terminal task status;
- duplicate work-queue tick;
- control-plane state updates;
- task result that does not satisfy the current wait and does not require model
  re-entry.

The continuation-trigger contract remains the source for the waiting matrix.
This RFC adds the scheduler-level requirement that mismatched triggers must be
recorded and must not silently satisfy an unrelated wait.

## Task Contract

Task transitions must be monotonic:

```text
queued -> running -> cancelling -> terminal
queued -> terminal
running -> terminal
```

Terminal statuses are:

- completed;
- failed;
- cancelled;
- interrupted.

Invariants:

- terminal tasks must not remain in the active task set;
- terminal task records outrank stale non-terminal task messages;
- task result delivery must not reopen a terminal task;
- a non-terminal task status is not model-visible by itself;
- blocking task truth is derived from latest task records, not from stale
  active id lists alone;
- task completion should be persisted before the corresponding model-visible
  task result is queued.

Recommended implementation boundary:

- all task lifecycle writes should go through one `TaskTransition` reducer;
- command tasks, child-agent tasks, and worktree child tasks should not each
  hand-roll `append_task + active_task_ids + status` updates.

## WorkItem Contract

WorkItem scheduling should use readiness:

- completed work is not runnable;
- blocked work is not runnable;
- `needs_input` is waiting for operator input;
- open, unblocked work is runnable.

Invariants:

- completed work items cannot become current;
- queued work items must not replace current work implicitly;
- a work-queue `queued_available` tick must not mutate current work;
- completing a work item clears it from current focus;
- completing a work item cancels work-item-scoped waiting intents;
- work-item completion should be blocked only by relevant blocking tasks once
  task-to-work-item association is available.

`current_work_item_id` and `current_turn_work_item_id` should remain distinct:

- current work item is the durable active focus;
- current turn binding is a scoped association for one real model turn.

Turn-end work-item commits should only run for a real turn boundary. Reducing a
non-model-visible message should not accidentally rewrite a work item's blocker
state.

## Work Queue Tick Contract

Work queue ticks are runtime-generated messages used to make runnable work
model-visible.

They must be idempotent.

Recommended idempotency keys:

```text
work_queue:continue_active:<work_item_id>:<work_item_generation>
work_queue:queued_available:<work_item_id>:<work_item_generation>
wake_hint:<waiting_intent_id_or_source>:<trigger_generation>
```

The current heuristic of scanning recent messages, briefs, tool executions, and
events is useful as a guardrail, but the scheduler contract should move toward
explicit idempotency keys.

Invariants:

- a work-queue tick is emitted only when the runtime is idle enough to process
  it;
- a model-visible continuation suppresses immediate duplicate
  `continue_active`;
- a wake hint has priority over work-queue ticks when both are pending;
- duplicate suppression records evidence, not just silence.

## Waiting Contract

Waiting belongs to the waiting plane.

Invariants:

- `awaiting_operator_input` is satisfied by operator input;
- `awaiting_task_result` is satisfied by a terminal blocking task result;
- `awaiting_timer` is satisfied by a timer fire;
- `awaiting_external_change` is satisfied by a contentful external event or a
  wake hint tied to an active waiting condition;
- mismatched triggers are liveness-only unless explicitly allowed to override;
- switching the active work item cancels stale work-item-scoped waits;
- agent-scoped waits are not cancelled merely because active work switches.

## Queue And Restart Contract

Queued messages are durable scheduler inputs.

Restart behavior should be explicit:

- `Queued` messages replay;
- `Dequeued` messages replay only if the previous run did not reach a terminal
  boundary;
- `Processed`, `Interrupted`, `Dropped`, and `Interjected` messages do not
  replay as normal queued messages.

The current replay behavior should be characterized by tests before it changes.

Open question:

- should a `Dequeued` message replay after a provider/tool side effect but
  before `Processed`, or should Holon persist a run checkpoint that prevents
  duplicate side effects?

Until that is resolved, tools that can cause side effects should remain
auditable and preferably idempotent or recoverable.

## Context And Compaction Contract

Compaction must not become an implicit scheduler authority.

### Cross-Turn Context Compaction

Cross-turn compaction keeps prompt history bounded across messages.

It may update:

- compacted message count;
- working-memory compression epoch;
- context summary when working memory is not active.

It must not decide:

- whether a task is active;
- whether a work item is complete;
- whether the runtime should wake.

### Turn-Local Compaction

Turn-local compaction keeps one provider/tool turn within the prompt budget.

It may create:

- deterministic round recaps;
- checkpoint prompts;
- checkpoint terminal records;
- baseline-over-budget terminal records.

It must not replace:

- WorkItem plan;
- task records;
- waiting intents;
- closure derivation.

### Provider Context Management

Provider context management is a provider-window optimization.

It must not become:

- Holon's semantic memory;
- scheduler state;
- a replacement for WorkItem plan or local checkpoint evidence.

## Closure Relationship

The scheduler should consume closure decisions, but closure should also be
derived from scheduler facts.

Expected precedence:

1. runtime error;
2. operator-input wait;
3. active blocking tasks;
4. active waiting intents;
5. active timers;
6. active turn in progress;
7. failed terminal turn;
8. runnable work signal;
9. completed terminal turn;
10. no waiting condition.

The result-closure RFC remains the semantic contract. This scheduler RFC
defines how facts enter that closure derivation and how closure affects the
next decision.

## Event And Ledger Requirements

Every scheduler decision should be explainable from durable records.

Required ledger classes:

- messages;
- queue entries;
- events;
- transcript;
- tasks;
- work items;
- waiting intents;
- timers;
- tool executions;
- briefs.

Recommended new event:

```json
{
  "kind": "scheduler_decision",
  "data": {
    "decision": "EmitSystemTick",
    "reason": "continue_active",
    "model_visible": false,
    "work_item_id": "work_...",
    "message_id": null,
    "evidence": [
      "runtime_idle",
      "current_work_item_runnable",
      "no_duplicate_tick_for_generation"
    ]
  }
}
```

The event should be generated from the scheduler projection, not hand-written
separately in each feature path.

## Test Strategy

The test plan should have four layers.

### Pure Reducer Tests

These should not boot a runtime.

Coverage:

- continuation matrix;
- closure priority;
- task status monotonicity;
- WorkItem readiness;
- work-queue tick idempotency;
- pause/stop gating;
- restart replay classification.

### Runtime Boundary Tests

These should run focused runtime instances with stub providers.

Coverage:

- queue -> dequeued -> processed transitions;
- provider turn started only when allowed;
- non-model-visible task status does not start a turn;
- terminal blocking task result resumes the expected wait;
- paused runtime persists task terminal state but does not start a provider
  turn;
- system tick generation does not duplicate across idle loops.

### Ledger Replay Tests

These should rebuild `SchedulerState` from saved ledger fixtures and assert the
same final projection.

Coverage:

- terminal task persisted before task result enqueue;
- Dequeued message replay after restart;
- pending wake hint recovery;
- current work item with queued follow-up;
- blocked work item with active waiting intent;
- turn-local baseline-over-budget terminal record.

### Scenario Tests

These should cover end-to-end flows that combine mechanisms.

Required scenarios:

- task result rejoin after cross-turn compaction preserves current work truth;
- queued work item notification after compaction does not focus the queued
  item;
- wake hint after compaction preserves provenance;
- turn-local checkpoint followed by WorkItem update invalidates the checkpoint
  anchor;
- completing one work item is not blocked by an unrelated task once task
  association exists;
- interrupt operator prompt during a long provider/tool turn preserves
  side-effect evidence and queue status.

## Incremental Implementation Plan

### Phase 1: Contract And Characterization

- land this RFC;
- add a `SchedulerProjection` read-only builder from current storage and agent
  state;
- emit `scheduler_decision` diagnostics without changing behavior;
- add pure invariant tests for terminal tasks, pause/stop gating, work-item
  readiness, and duplicate work-queue ticks.

### Phase 2: Task Transition Unification

- introduce `TaskTransition`;
- move command task and child task terminal persistence through the same path;
- enforce terminal active-task cleanup in one place;
- add restart and out-of-order task-message tests.

### Phase 3: Work Queue Idempotency

- add explicit work-queue tick idempotency keys;
- replace broad recent-ledger scans with generation-aware checks;
- keep old scans temporarily as diagnostics.

### Phase 4: Turn Binding Cleanup

- separate durable current work from scoped turn binding;
- make turn-end WorkItem commit require a real model turn terminal;
- prevent non-model-visible reductions from mutating WorkItem blockers.

### Phase 5: Replay Harness

- add fixture-based scheduler replay tests;
- convert future "agent seems stuck" reports into ledger fixtures before
  patching;
- use replay mismatches to guide further reducer extraction.

## Invariants Checklist

- stopped runtime does not process messages;
- paused runtime does not start model turns;
- current run id exists only while a run is active;
- terminal tasks are absent from active task ids;
- stale non-terminal task updates do not reopen terminal tasks;
- blocking task closure is based on latest non-terminal blocking tasks;
- completed work items are never runnable;
- queued work items do not become current without an explicit pick;
- `needs_input` work items wait for operator input;
- duplicate work-queue ticks are suppressed with durable evidence;
- wake hints are liveness signals unless made contentful by trigger policy;
- mismatched continuation triggers do not satisfy unrelated waiting reasons;
- turn-end WorkItem commits run only for real turn terminals;
- compaction does not rewrite scheduler truth;
- ledger replay can explain the final scheduler projection.

## Open Questions

- Should `AgentState.status` eventually be fully derived, or should it remain a
  cached projection with strict consistency checks?
- What is the durable generation marker for WorkItem idempotency: `updated_at`,
  append sequence, or an explicit revision?
- How should Holon prevent duplicate side effects when a `Dequeued` message
  replays after a crash?
- Should task-to-work-item association become a first-class `TaskRecord` field
  or remain in detail metadata?
- Should scheduler replay fixtures live under `tests/fixtures/scheduler/` or
  be generated from real `.holon/ledger` directories?

## Related RFCs

- [Result Closure](./result-closure.md)
- [Continuation Trigger](./continuation-trigger.md)
- [Work Item Runtime Model](./work-item-runtime-model.md)
- [Waiting Plane And Reactivation](./waiting-plane-and-reactivation.md)
- [Turn-Local Context Compaction](./turn-local-context-compaction.md)
- [OpenAI Remote Compaction Boundary](./openai-remote-compaction.md)
