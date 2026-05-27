---
title: RFC: Agent Lifecycle Control Posture
date: 2026-05-12
status: draft
handle: rfc-agent-lifecycle-control-posture
---

# RFC: Agent Lifecycle Control Posture

## Summary

Holon should make agent lifecycle control a small, explicit contract separate
from the runtime scheduler contract.

The scheduler answers: given a runnable agent and durable runtime facts, what is
the next action?

Lifecycle control answers: is this agent allowed to run at all, and what
runtime-owned resources must be retained or released when that answer changes?

This RFC proposes converging the operator-facing lifecycle surface from
`pause/resume/stop` toward `start/stop`:

- `Stop` is a hard lifecycle boundary that prevents autonomous processing,
  aborts the current run, releases runtime-owned execution resources, and marks
  runtime-owned active work as no longer live.
- `Start` hands the agent back to the scheduler. It does not directly run the
  model; it lets the scheduler projection derive whether the agent should be
  idle, awaiting tasks, or start processing queued input.
- `Pause` and `Resume` should be removed or deprecated rather than kept as a
  third lifecycle state with ambiguous background-task and workspace behavior.

## Problem

`Pause`, `Resume`, and `Stop` currently overlap in ways that make runtime
behavior hard to reason about:

- `Pause` can mean "do not start new model turns", "abort the current run",
  "let background tasks finish", or "freeze everything".
- If background tasks continue while paused, task-result messages can enter the
  queue and create subtle questions about whether durable reductions, system
  ticks, or model reentry are allowed.
- `Resume` from `Paused` and `Resume` from `Stopped` are different operations:
  one resumes a retained runtime, while the other recreates or reactivates
  runtime ownership.
- `Stop` needs clear effects on current provider turns, managed tasks,
  workspace occupancy, timers, and queued messages.
- Several modules still mutate `AgentState.status` directly. This makes it hard
  to tell whether a posture change came from lifecycle control or scheduler
  projection.

These ambiguities leak into the scheduler work. The scheduler RFC should not
carry all lifecycle semantics; it should consume a clear runnable/stopped
boundary.

## Goals

- define the lifecycle-control vocabulary for agent runtime ownership;
- reduce lifecycle control to `Start` and `Stop` unless a future RFC proves a
  distinct pause state is necessary;
- specify how `Stop` affects current runs, queued messages, active tasks,
  workspace occupancy, timers, and wake hints;
- specify how `Start` hands control back to the scheduler without directly
  starting a model turn;
- make lifecycle posture writes flow through scheduler-owned posture helpers or
  `SchedulerDecisionExecutor` entrypoints;
- keep external side effects, such as workspace occupancy release, outside the
  scheduler decision core while making their required outcomes explicit.

## Non-goals

- do not redefine task tool semantics such as `TaskStop`;
- do not make the scheduler execute external host or workspace side effects;
- do not make `Start` a synthetic user message;
- do not replay interrupted tasks automatically;
- do not remove append-only ledger records for stopped agents;
- do not define a UI-specific lifecycle surface. CLI, TUI, HTTP, and daemon APIs
  should project the same runtime contract.

## Terms

### Lifecycle Control

An operator- or host-owned request that changes whether the agent runtime is
allowed to perform autonomous work.

### Scheduler Posture

The derived operational state of a runnable agent, such as `AwakeIdle`,
`AwakeRunning`, or `Asleep`.

### Runtime Ownership

The runtime's active responsibility for provider turns, managed task handles,
workspace occupancy, timers, and autonomous wakeups for one agent.

## Proposed Lifecycle Surface

### Actions

The lifecycle control surface should converge to:

```rust
enum ControlAction {
    Start,
    Stop,
}
```

`Pause` and `Resume` should be treated as deprecated request shapes during a
migration window. They canonicalize to `Stop` and `Start` respectively and
should not remain first-class runtime states.

### Status Set

The target `AgentStatus` set is:

```rust
enum AgentStatus {
    Booting,
    AwakeIdle,
    AwakeRunning,
    AwaitingTask,
    Asleep,
    Stopped,
}
```

`AwaitingTask` is retained as a transitional lifecycle label because the
current runtime, TUI, daemon, and waiting paths project blocking task waits
through it. It may be folded into `AwakeIdle` plus task-wait scheduling posture
in a later migration, but this RFC treats it as current implementation contract.

`Paused` should remain readable only for legacy persisted state during the
migration window. New lifecycle control should not generate `Paused`; it should
persist `Stopped` for non-runnable lifecycle control.

`Stopped` is the only lifecycle-control gate. Other statuses are scheduler
posture derived from runtime facts.

## Action Semantics

### Start

`Start` means: hand the agent back to scheduler ownership.

It should:

- be accepted only for `Stopped` or bootstrapping agents;
- clear stale stopped-only lifecycle metadata if any exists;
- not create an operator prompt, system tick, or task result;
- not directly start a model turn;
- not replay interrupted tasks;
- wake the runtime loop so the scheduler can inspect current facts;
- derive the next status from scheduler projection:
  - queued message or wake hint available -> runnable `AwakeIdle` before the
    next run-loop decision;
  - no runnable facts -> `AwakeIdle` or `Asleep`, depending on sleep policy.

Active task records are diagnostic and task-result reduction facts, not
lifecycle posture gates. A running task does not by itself move the scheduler
into a waiting status.

`Start` may restore runtime-owned in-process handles only when those handles are
known to still be valid. It must not invent handles for interrupted tasks.

### Stop

`Stop` means: release runtime ownership and prevent autonomous processing.

It should:

- abort the current provider/tool run if one is active;
- stop dequeuing or processing queued messages;
- leave queued messages durable for a future `Start`;
- release active workspace occupancy;
- clear `current_run_id`;
- clear `sleeping_until` and cancel session-owned sleep wakeups;
- clear pending autonomous wake hints that only exist to keep the stopped agent
  alive;
- cancel runtime-owned cancellable task handles;
- mark active runtime-owned tasks that cannot be proven live as
  `TaskStatus::Interrupted` with restart/stop evidence;
- not delete task, work-item, message, or event ledger history;
- persist `AgentStatus::Stopped`;
- emit clear lifecycle and scheduler-posture evidence.

Stop is stronger than the old pause concept. It is the operator's way to say
"this agent should not continue owning execution resources".

## Queue And Message Behavior

Queued messages are durable scheduler inputs. `Stop` must not delete them.

While stopped:

- public/operator ingress may still append durable messages if admission policy
  allows it;
- the runtime must not process the queue;
- the scheduler decision for stopped posture is `Stop` or a lifecycle no-op;
- message admission must not wake the stopped agent into runnable posture.

After `Start`, queued messages are processed according to the scheduler
contract. Existing queue replay rules still apply:

- `Queued` and `Dequeued` messages may replay at the message level;
- `Processed`, `Aborted`, `Interjected`, and `Dropped` messages do not replay as
  normal queued messages.

## Task Behavior

`Stop` and task tools serve different purposes.

- `TaskStop` controls one managed task.
- Agent `Stop` controls runtime ownership for the whole agent.

On agent `Stop`:

- runtime-owned cancellable task handles should receive cancellation;
- command tasks, child-agent supervision tasks, task-owned worktree tasks, and
  sleep jobs should transition through the task reducer where possible;
- if a task cannot be cancelled cleanly or its handle is already gone, mark it
  `Interrupted` with evidence that the agent stopped;
- terminal tasks remain terminal;
- background task output that arrives after stop may be recorded as durable
  evidence, but it must not cause model reentry until a future `Start`.

A future implementation may distinguish externally-owned tasks from
runtime-owned tasks. The default for current runtime-owned task records should
be conservative: do not assume they remain live after agent `Stop`.

## Workspace Occupancy

`Stop` releases active workspace occupancy.

It should not remove workspace history or attachments. The agent may keep its
workspace binding records so a future `Start` can re-enter or re-acquire the
workspace according to workspace policy.

This keeps the resource ownership boundary clear:

- stopped agents do not hold exclusive write occupancy;
- started agents acquire workspace occupancy when execution requires it;
- failure to release occupancy should be reported as a lifecycle/control error,
  not silently ignored.

## Timers, Sleep, And Wake Hints

`Stop` cancels runtime-owned autonomous wakeups.

- session sleep wake tasks should become inert after stop;
- pending wake hints whose only purpose is to resume autonomous work should be
  cleared or ignored while stopped;
- durable timer records may remain in the ledger, but timer delivery must not
  wake a stopped agent into runnable posture;
- after `Start`, active durable timers can be re-evaluated by the waiting plane.

## Scheduler Boundary

This RFC preserves the scheduler RFC boundary:

- lifecycle control decides whether the agent is runnable;
- scheduler decides what a runnable agent should do next.

`Stopped` is a hard scheduler gate. For stopped agents,
`decide_next_action` should produce `Stop` or a liveness-only no-op decision and
must not produce `StartModelTurn`, `ReduceMessageOnly`, or `EmitSystemTick`.

`Start` does not bypass the scheduler. It changes lifecycle posture to runnable
and notifies the run loop. The next actual action must still come from
`SchedulerProjection -> decide_next_action -> execute`.

## SchedulerDecisionExecutor Ownership

The implementation should converge on `SchedulerDecisionExecutor` as the entry
point for status-like posture writes, without turning it into a host-side-effect
executor.

Recommended shape:

```rust
impl SchedulerDecisionExecutor<'_> {
    async fn bootstrap_recovered(&self) -> Result<AgentState>;
    async fn apply_control(&self, action: ControlAction) -> Result<ControlPostureOutcome>;
    async fn request_shutdown(&self, reason: ShutdownReason) -> Result<ShutdownPostureOutcome>;
    async fn transition_to_sleep(&self, sleeping_until: Option<DateTime<Utc>>) -> Result<AgentState>;
    async fn admit_message_wake(&self, message: &MessageEnvelope) -> Result<AgentState>;
}
```

These methods should own:

- scheduler projection reads;
- lifecycle or scheduler decision event construction;
- mutation of `AgentState.status`, `current_run_id`, `sleeping_until`, pending
  counts, and lifecycle-gated wake fields;
- `write_agent` for those posture changes.

They should not own:

- workspace host release I/O;
- provider transport shutdown details beyond abort token cancellation;
- task process kill implementation;
- HTTP/TUI/CLI formatting.

For external side effects, the executor should return an outcome:

```rust
struct ControlPostureOutcome {
    state: AgentState,
    occupancy_to_release: Option<String>,
    tasks_to_cancel: Vec<TaskRecord>,
    aborted_run_id: Option<String>,
}
```

Lifecycle or host code performs those side effects and records their results.

## Events And Evidence

Lifecycle control should emit durable evidence distinct from normal scheduler
messages:

- `control_request_admitted`
- `control_applied`
- `scheduler_decision` or `scheduler_posture_decision`
- `current_run_aborted` when a run is aborted by stop/shutdown
- task transition events for cancelled/interrupted active tasks
- workspace occupancy release events when applicable

The event payload should include:

- action: `start` or `stop`;
- previous status;
- next status;
- boundary: `control`, `bootstrap`, `shutdown`, `lifecycle_sleep`, or
  `message_admission`;
- affected run id, task ids, workspace occupancy id, and evidence strings when
  present.

## Migration Plan

### Step 1: Document And Gate Deprecated Actions

- Add this RFC.
- Mark `Pause` and `Resume` as lifecycle concepts to remove from the primary
  operator surface.
- Accept request-facing aliases as deprecated compatibility shapes:
  `pause -> stop` and `resume -> start`.
- Return start/stop guidance in user-facing lifecycle errors and docs.

### Step 2: Introduce `Start` And Executor Control Entry

- Add `ControlAction::Start`.
- Implement `SchedulerDecisionExecutor::apply_control` for `Start` and `Stop`.
- Keep old action variants only as temporary parser aliases if needed.
- Add tests for stopped queue gating and start reactivation.

### Step 3: Contain Legacy `Paused`

- Stop generating `AgentStatus::Paused` from lifecycle control.
- Keep `AgentStatus::Paused` readable as a legacy persisted posture until a
  storage migration removes or rewrites old ledgers.
- Treat legacy paused agents as non-runnable in scheduler, message dispatch,
  waiting, and task posture gates.
- Replace lifecycle pause/resume tests with stopped/start tests while retaining
  narrow legacy-state coverage where persisted-state compatibility matters.

### Step 4: Make Stop Resource Semantics Explicit

- Abort current run on stop.
- Release workspace occupancy on stop.
- Clear sleep/wake autonomous posture on stop.
- Request cancellation or interruption for runtime-owned active tasks on stop.
- Transition runtime-owned active tasks to cancelled/interrupted according to
  task reducer rules.
- Ensure stopped agents do not emit work-queue or wake-hint system ticks.

### Step 5: Move Remaining Posture Writes Behind Executor Methods

- Bootstrap recovery uses `bootstrap_recovered`.
- Message admission wake uses `admit_message_wake`.
- Sleep transition uses `transition_to_sleep`.
- Shutdown uses `request_shutdown`.
- Lifecycle control uses `apply_control`, records `scheduler_posture_decision`
  evidence, and returns external cleanup obligations to the caller.

## Verification Plan

Add focused tests for:

- stopped agent does not dequeue queued messages;
- `Start` does not directly create a model turn;
- `Start` hands queued work to scheduler on the next run-loop decision;
- `Stop` aborts current run and records `current_run_aborted`;
- `Stop` releases workspace occupancy;
- `Stop` clears sleep/wake autonomous posture;
- `Stop` interrupts or cancels runtime-owned active tasks through the task
  reducer;
- task result arriving after stop is durable evidence but does not cause model
  reentry;
- no direct `AgentState.status = ...` writes remain outside scheduler posture
  helpers for lifecycle-controlled fields.

## Open Questions

- Should request-facing `pause` map to `stop`, or should it be rejected with a
  clear error?
- Should request-facing `resume` map to `start`, or should it be rejected with a
  clear error?
- Do externally-owned child agents need a separate stop policy from
  runtime-owned command tasks?
- Should `Start` reacquire the last active workspace immediately or only when a
  tool/execution path needs it?

## Relationship To Other RFCs

- [Runtime Scheduler Contract](./runtime-scheduler-contract.md): owns next-action
  decisions for runnable agents.
- [Agent Control Plane Model](./agent-control-plane-model.md): owns the broader
  agent-plane control and inspection surfaces.
- [Command Tool Family](./command-tool-family.md): owns per-task command control,
  including `TaskStop`.
- [Workspace Binding and Execution Roots](./workspace-binding-and-execution-roots.md):
  owns workspace binding and execution-root projection.
