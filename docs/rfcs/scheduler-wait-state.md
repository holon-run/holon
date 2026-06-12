---
title: RFC: Scheduler Wait State And Recoverable Agent Continuation
date: 2026-05-19
status: updated
---

# RFC: Scheduler Wait State And Recoverable Agent Continuation

## Summary

Holon's scheduler should derive an explicit scheduling state for each open
WorkItem and use that state to decide whether an agent should continue now,
wait for a trusted wake source, ask the operator, remain blocked, or become
idle.

Sleep should not by itself mean "there is no runnable work". It should become
the runtime posture after the agent has committed enough durable state for the
runtime to classify the work. If any WorkItem is still runnable, turn
closure must not silently strand the agent; the runtime should enqueue or
schedule a continuation instead.

The current agent-facing contract is intentionally small: `WaitFor` records one
explicit wait condition and yields the turn. It supports three wake classes:
operator input, task result, and external state. `blocked_by` remains as
display text on WorkItems; `UpdateWorkItem` no longer exposes blocker mutation
or fallback recheck fields to agents. After inspecting evidence and confirming
a blocker is resolved, agents use `PickWorkItem(clear_blocker=true, reason=...)`
to restore the WorkItem as runnable while making the recovery explicit.

`WaitFor` is not generic WaitCondition CRUD. It is the narrow public path for
the common scheduler waits that the runtime can reason about.

## Problem

Holon is intended to drive long-lived goals. In practice, an agent pursuing a
long objective often reaches a turn boundary where it is:

- ready to continue immediately;
- waiting for an internal task result;
- waiting for external state;
- waiting for operator input;
- blocked by an unrecoverable or unstructured dependency;
- actually complete; or
- idle because no open work exists.

Today those states are not expressed as a single scheduler-facing contract.
They are partially encoded through:

- WorkItem lifecycle and readiness;
- `plan_status`;
- natural-language `blocked_by`;
- task lifecycle;
- waiting intents;
- external trigger callbacks;
- turn closure and legacy `Sleep` calls.

That makes closure-time behavior hard to reason about. A long-lived agent can
appear to be asleep even though it has runnable work, or it can record an
external wait that depends only on a callback path with no visible recovery or
audit path if the callback never arrives.

The runtime needs a generic scheduling model that keeps agent continuation
reliable without teaching the core scheduler provider-specific business rules.

## Goals

- Define a scheduler-facing state model for WorkItems.
- Make runnable work non-silent: indefinite sleep must not strand runnable
  WorkItems.
- Add a minimal structured wait path without requiring provider-specific wait
  metadata.
- Keep task result, external ingress, timer, operator input, and system tick
  wakeups compatible with a future generic wake-source model.
- Preserve the boundary between generic runtime scheduling and provider or
  skill-specific business policy.
- Support incremental migration from existing WorkItem, waiting intent, task,
  and external trigger records.

## Non-goals

- Do not define GitHub, CI, review, merge, deployment, inbox, or other
  provider-specific fallback durations.
- Do not make the scheduler decide whether an external condition is
  semantically resolved.
- Do not require every external wait to have a timer.
- Do not replace WorkItem planning or todo tracking with a new project
  management system.
- Do not make `Sleep` a complex business workflow tool.

## Current structure

The current repository already has the major runtime pieces:

- `WorkItem` records goal, readiness, plan state, blockers, todo progress, and
  completion.
- `Task` records command tasks, child-agent tasks, and other runtime-owned
  execution units.
- external trigger capability records provide providerless ingress for waking
  or enqueueing agent input.
- waiting intents provide a lower-level way to describe external waiting.
- closure and lifecycle code decide whether to enqueue additional work or let
  the agent rest.

This RFC does not propose replacing those pieces. It adds a clearer derived
scheduling layer over them and exposes the minimal `WaitFor` tool needed to
create scheduler-visible wait conditions.

## Core model

### WorkItemSchedulingState

The scheduler should derive a state for each WorkItem:

```text
WorkItemSchedulingState =
  Runnable
  WaitingOperator
  WaitingTask
  WaitingExternal
  WaitingTimer
  WaitingSystem
  Blocked
  Completed
```

`Idle` is an agent-level state, not a WorkItem state. An agent is idle when it
has no open WorkItems and no other runnable runtime work.

### Runnable

A WorkItem is runnable when it is open, ready for execution, not blocked, and
not covered by an active wait condition.

Example derivation:

```text
open
plan_status == ready
blocked_by == None
no active WaitCondition
```

Runnable WorkItems should cause the runtime to continue agent execution. If a
turn closes while runnable WorkItems exist, the runtime must not treat the
agent as truly idle. It should enqueue or schedule continuation.

### WaitingOperator

A WorkItem is waiting for the operator when it explicitly requires operator
input.

Example derivation:

```text
open
plan_status == needs_input
```

The scheduler should not auto-continue this work without operator input unless
a future explicit policy says otherwise.

### WaitingTask

A WorkItem is waiting on a runtime task when it has an active wait condition
whose wake source is a task result.

Task result wakeups are internal runtime signals. They usually do not need a
provider fallback because the runtime owns the task lifecycle and terminal
result delivery.

### WaitingExternal

A WorkItem is waiting externally when it has an active external wait condition.

The scheduler does not understand what the external source means. It only knows:

- there is an opaque waiting subject;
- one or more wake sources may reactivate the agent;
- a continuation should run when a wake source fires;
- the wait may be auditable as weak if it lacks a durable recovery path.

### WaitingTimer

A WorkItem is waiting for a timer when progress is intentionally delayed until
a runtime-owned timer fires.

Blocked WorkItem `recheck_at` deadlines are not part of `WaitingTimer`; they
remain `Blocked` and only create a fallback reminder for the owning agent to
inspect the blocker.

### WaitingSystem

A WorkItem is waiting for a runtime-owned system tick when progress should
continue by emitting scheduler-generated maintenance or recovery instead of
operator input, task completion, a wall-clock timer, or an external callback.

### Blocked

A WorkItem is blocked when it has an explicit blocker that is not represented
as a structured active wait.

Natural-language blockers remain useful for operator visibility, but they are
not enough for reliable automatic continuation. In the first implementation
phase, blocked WorkItems may carry a `recheck_at` deadline so the runtime can
remind the owning agent to inspect the blocker later.

The deadline does not change WorkItem readiness:

```text
blocked_by != None
recheck_at <= now
=> WorkItemSchedulingState::Blocked
=> enqueue or consume a reminder for the owning agent
=> agent must explicitly update or clear blocked_by
```

The scheduler must not parse `blocked_by` or infer whether the blocker is
resolved.

### Completed

Completed WorkItems do not participate in scheduling.

## Agent-facing `WaitFor`

`WaitFor` records explicit waiting state and yields the turn.

```text
WaitFor {
  reason: String
  wake: operator_input | task_result | external
  resource: Option<String>
  recheck_after_ms: Option<u64>
}
```

Rules:

- `reason` is required and must be non-empty.
- `wake=task_result` requires `resource=<task_id>`.
- `wake=external` may include `resource=<stable external object>`, such as a
  URL or `github:holon-run/holon#1435`; omitting `resource` means any external
  ingress can wake the wait.
- `wake=operator_input` may omit `resource`.
- `recheck_after_ms` is optional. When present, it records a fallback recheck
  deadline at `now + recheck_after_ms`; when omitted, the wait has no fallback
  recheck deadline.
- if a current open WorkItem exists, the wait is WorkItem-scoped;
- if no current open WorkItem exists, the wait is agent-scoped;
- to wait on another WorkItem, the agent must pick it first.

WorkItem-scoped `WaitFor` replaces active waits on that WorkItem, writes
`blocked_by=reason` for display, writes `recheck_at` only when
`recheck_after_ms` is present, clears `recheck_at` otherwise, clears
`recheck_consumed_at`, and releases only the current turn binding. The durable
`current_work_item_id` remains the focused WorkItem so the agent can resume or
inspect the same long-lived objective after the wait wakes. Scheduler readiness
comes from the active wait condition and WorkItem blocker, not from clearing the
agent's current focus.

Agent-scoped `WaitFor` has no WorkItem record to mutate. When
`recheck_after_ms` is present, the wait condition continuation records both the
relative delay and absolute `recheck_at` so scheduler diagnostics can classify
the external wait as recoverable.

Mapping:

| `wake` | `WaitCondition.kind` | `subject_ref` | `wake_sources` |
|--------|----------------------|---------------|----------------|
| `task_result` | `Task` | `resource` | `TaskResult { task_id: resource }` |
| `external` | `External` | `resource` when present | `ExternalIngress { external_trigger_id: None }` |
| `operator_input` | `Operator` | `resource` when present | `OperatorInput` |

Terminal task results resolve matching task wait conditions and clear the
matching `WaitFor` blocker text. Operator and external wakeups record
reconciliation signals but do not automatically resolve the wait or mutate the
WorkItem, because the agent must inspect the new evidence.

`CompleteWorkItem` cancels active WorkItem-scoped waits.

## Superseded Phase 0: blocked WorkItem recheck fallback

The earlier phase-0 plan used the existing WorkItem blocker field plus one
small scheduler-facing deadline:

```text
WorkItem {
  blocked_by: Option<String>
  recheck_at: Option<Timestamp>
}
```

`UpdateWorkItem` previously supported a small `recheck_after` input when the
agent set or refreshed `blocked_by`. That public entry point is removed:
`UpdateWorkItem` updates WorkItem content and plan state only, while `WaitFor`
is the authoritative entry point for wait state and fallback rechecks.
Historical `recheck_at` and `recheck_consumed_at` fields remain in storage and
read models for older records and for `WaitFor`-managed fallback rechecks. A
resolved blocker is cleared by explicitly picking the WorkItem with
`clear_blocker=true` and a reason, not by setting `blocked_by` through
`UpdateWorkItem`.

Rules:

- Setting `blocked_by` with an explicit `recheck_after` sets one `recheck_at`
  deadline relative to the update time.
- Setting `blocked_by` without an explicit recheck value sets a default
  `recheck_at = now + 1 hour`.
- The 1-hour default is a generic runtime anti-forget fallback for unstructured
  blocked WorkItems. It is not a provider-specific GitHub, CI, review, merge,
  deployment, or inbox policy.
- Clearing `blocked_by` clears `recheck_at` and any delivery marker for that
  reminder.
- Updating objective, plan status, todo list, or other WorkItem fields without
  touching `blocked_by` must not reset `recheck_at`.
- `recheck_at` is one-shot. Once the due reminder is delivered or consumed, the
  runtime should not repeatedly enqueue new reminders for the same due
  deadline. If the agent still needs a fallback, it must set or refresh
  `blocked_by` with another `recheck_after`.

Due rechecks are delivered per owning agent, not per WorkItem:

- If one or more blocked WorkItems are due for the same agent, the scheduler
  should coalesce them into one low-priority recheck wake/event.
- The recheck payload or runtime context should identify the due WorkItems so
  the agent can decide what to inspect.
- Multiple due WorkItems should be ordered by earliest `recheck_at`, then by
  the existing WorkItem ranking.
- If the agent is sleeping or awake-idle, enqueue the coalesced recheck.
- If the agent is already running a turn, consume or clear the due recheck
  marker without interrupting and without enqueueing a later noise reminder.
- If the agent already has queued input but is not running, do not add an extra
  reminder solely for the recheck; rely on the existing queued turn/context to
  surface due blocked WorkItems.

The recheck reminder only asks the agent to reconcile state. It must not:

- make the WorkItem runnable;
- clear `blocked_by`;
- resolve a wait;
- choose provider-specific policy based on blocker text.

After the agent inspects the reminder evidence, `PickWorkItem(clear_blocker=true,
reason=...)` is the explicit recovery action that clears `blocked_by`, fallback
recheck fields, and active WorkItem-scoped waits.

## Deferred WaitCondition direction

The following shape is a future direction, not the first implementation scope.
It should be introduced only if the scheduler or a provider integration has a
clear contract for using these fields.

`WaitCondition` records that a WorkItem is waiting for a recoverable or
operator-visible condition.

Initial shape:

```text
WaitCondition {
  id
  work_item_id
  status: active | resolved | cancelled | expired

  kind: task | external | operator | timer
  source: Option<String>
  subject_ref: Option<String>
  waiting_for: String

  wake_sources: Vec<WakeSource>
  continuation: ContinuationSpec

  created_at
  updated_at
  expires_at: Option<Timestamp>
}
```

The fields `source`, `subject_ref`, and `waiting_for` are intentionally opaque
to the core scheduler. They are for display, correlation, and handoff to the
agent, skill, provider, or operator.

The scheduler should not mark an external wait as resolved simply because a
wake source fired. A wake source means "reconcile this wait now". The agent,
skill, provider, or operator-facing workflow decides whether the external
condition is actually resolved.

## WakeSource

`WakeSource` is the scheduler-executable part of a wait condition.

```text
WakeSource =
  TaskResult { task_id }
  ExternalIngress { external_trigger_id: Option<String> }
  Timer { wake_at }
  OperatorInput
  SystemTick
```

### Task result

Used when the runtime owns the executing task.

```text
WaitCondition {
  kind: task
  waiting_for: "task_completed"
  wake_sources: [
    TaskResult { task_id: "task_..." }
  ]
}
```

### External ingress

Used when a providerless external trigger or other ingress path may wake the
agent.

```text
WaitCondition {
  kind: external
  source: "github"
  subject_ref: "repo=holon-run/holon,pr=123"
  waiting_for: "checks_success"
  wake_sources: [
    ExternalIngress { external_trigger_id: "default" },
    Timer { wake_at: "..." }
  ]
}
```

The example uses GitHub-like labels only as opaque metadata. The scheduler does
not know what `checks_success` means.

### Timer

Used to make a wait recoverable even if another wake source fails or never
arrives. The timer wake should enqueue a reconciliation continuation, not assume
success or failure.

### Operator input

Used for waits that are explicitly waiting on the operator.

### System tick

Used for generic maintenance, audit, or soft recovery. A system tick is not a
provider-specific fallback policy.

## Turn-end and Sleep contract

Sleep should be treated as a runtime rest posture after state has been
committed, not as a model-facing tool or the primary source of scheduling truth.

At a turn boundary, the runtime should be able to classify the agent posture as:

```text
ContinueNow
Wait
NeedOperator
Blocked
Complete
Idle
```

These posture labels are the coarse turn-end contract. The implementation may
use a more granular `SchedulerDecisionKind` to express the concrete action that
realizes the posture:

- `ContinueNow` -> `StartModelTurn`, `ReduceMessageOnly`, or `EmitSystemTick`;
- `Wait` -> `WaitForTask`, `WaitForExternalChange`, or `WaitForTimer`;
- `NeedOperator` -> `WaitForOperator`;
- `Blocked` -> a liveness-only wait or sleep decision with blocker evidence;
- `Complete` -> no scheduling for completed WorkItems;
- `Idle` -> `Sleep`, `StayIdle`, `Stop`, or `Noop` depending on lifecycle and
  duplicate-suppression facts.

The preferred durable forms are:

- runnable WorkItem exists -> `ContinueNow`;
- active wait condition exists -> `Wait`;
- WorkItem needs input -> `NeedOperator`;
- WorkItem has unstructured blocker -> `Blocked`;
- WorkItem completed -> `Complete`;
- no open work -> `Idle`.

If a turn closes while any WorkItem is `Runnable`, closure should enqueue or
schedule a continuation instead of leaving the agent asleep.

If all open WorkItems are `WaitingTask`, indefinite sleep is safe because task
results are runtime-owned wake sources.

If all open WorkItems are `WaitingOperator`, indefinite sleep is safe because
operator input is the expected wake source.

If any open WorkItem is `WaitingExternal`, sleep is safe only to the degree that
the wait has recoverable wake sources. An external wait with only external
ingress may be allowed during migration but should be auditable.

If any open WorkItem is `Blocked` with only unstructured natural-language
blockers, the runtime should not auto-continue it, but it should make the state
visible as non-recoverable or weakly recoverable.

## External wait recovery policy

The core scheduler should not choose provider-specific fallback durations.

For legacy phase-0 records, the only built-in duration was the generic 1-hour
default for unstructured blocked WorkItems with no explicit `recheck_after`.
The scheduler still does not interpret `blocked_by`; it only detects that the
one-shot deadline is due and asks the agent to recheck.

The fuller external wait classification below is deferred with the
`WaitCondition` direction:

It may classify external waits by recoverability:

```text
RecoverableExternalWait:
  has ExternalIngress and Timer
  or has Timer
  or has provider-declared durable queue / recheck source

WeakExternalWait:
  has ExternalIngress only

ExplicitNoFallbackExternalWait:
  has no recovery path, with a recorded reason
```

The runtime can then evolve in phases:

### Phase 1: warning

Allow external waits without timer recovery, but surface an audit warning such
as `external_wait_without_recovery`.

Implemented status surfaces should expose active wait conditions without
provider-specific interpretation. Each active wait condition may include a
derived `external_recoverability` value:

- `recoverable` when the wait has a timer, system tick, or opaque
  provider/user-declared fallback metadata such as a durable queue or recheck
  source;
- `weak` when the wait only has external ingress/callback wake sources;
- `explicit_no_fallback` when opaque metadata records that no fallback exists
  and includes a reason.

Agent-facing projections such as `AgentGet` and `ListWorkItems` should preserve
the opaque wait metadata alongside the derived classification so agents and
skills can reconcile provider-specific details without teaching the scheduler
about GitHub, CI, or other providers.

### Phase 2: soft recovery

Allow a generic system tick to re-enter the agent for audit or reconciliation.
This is runtime safety, not business policy.

### Phase 3: strict mode

Require external waits to declare one of:

- a recoverable wake source;
- a provider-declared durable queue or recheck source;
- an explicit `no_fallback` reason.

## Scheduler responsibility boundary

The scheduler owns:

- deriving `WorkItemSchedulingState`;
- preventing silent indefinite sleep while runnable work exists;
- persisting and displaying blocked WorkItem recheck deadlines;
- routing task, external ingress, timer, operator, and system tick wake sources;
- enqueueing continuation for wake reconciliation;
- surfacing weak or non-recoverable waits in audit/status views.

The scheduler does not own:

- interpreting CI, review, merge, deployment, inbox, or provider-specific state;
- choosing provider-specific fallback durations;
- deciding whether an external condition has semantically passed;
- resolving an external wait without agent, skill, provider, or operator
  reconciliation.

## Incremental implementation plan

### 0. Historical blocked WorkItem recheck fallback

The earlier implementation added the minimal
`blocked_by + recheck_after/recheck_at` contract:

- persist `recheck_at` on blocked WorkItems;
- historically extend `UpdateWorkItem` with `recheck_after` or an equivalent
  small input;
- apply the 1-hour default only when setting or refreshing `blocked_by` without
  an explicit recheck value;
- clear `recheck_at` when `blocked_by` is cleared;
- coalesce due reminders per agent;
- preserve `Blocked` readiness until the agent explicitly updates the WorkItem.

This legacy phase must not be extended with new agent-facing blocker fields.

### 1. Derive scheduling state

Add an internal `WorkItemSchedulingState` derivation from existing WorkItem,
task, waiting, and blocker state.

Use the derived state in work listing, closure, and scheduler diagnostics before
changing tool surfaces.

### 2. Use scheduling state in closure

Replace ad hoc runnable-work checks with state-derived closure behavior:

- `Runnable` -> enqueue or schedule continuation;
- `WaitingTask` -> sleep until task result;
- `WaitingOperator` -> wait for operator input;
- `WaitingExternal` -> wait for wake source and surface recoverability;
- `WaitingTimer` -> wait until the runtime timer fires;
- `WaitingSystem` -> emit a runtime-owned system tick;
- `Blocked` -> expose blocker and do not auto-continue;
- `Completed` -> ignore for scheduling.

### 3. Add narrow WaitFor ledger writes

Generalize waiting state into `WaitCondition` records through the narrow
`WaitFor` tool. This is not open-ended CRUD; the schema stays small:

```text
reason
wake
resource
```

### 4. Normalize wake-source continuation

External ingress, timer, operator input, and system tick should enqueue
continuation that asks the agent to reconcile the wait, rather than implicitly
resolving the wait. Terminal task results are the exception: a matching
`task_result` wait is resolved because the awaited runtime-owned task has
reached a terminal state.

### 5. Add external wait audit

Surface weak external waits that have no timer, durable queue, or explicit
`no_fallback` reason.

## Open questions

- Should `plan_status=needs_input` eventually be migrated into
  `WaitCondition(kind = operator)`, or remain a separate planning posture?
- Should `blocked_by` remain a separate WorkItem display field forever, or
  eventually become display text derived only from active wait records?
- What is the minimum continuation payload needed for reliable wake
  reconciliation?
- Should system tick be a first-class `WakeSource` for agent-facing waits, or
  remain scheduler-only?
- How should weak external waits be shown in `ListWorkItems`, `AgentGet`, and
  TUI surfaces?

## Relationship to existing RFCs

- `work-item-runtime-model.md` defines WorkItem as the durable goal anchor.
  This RFC defines the scheduling state derived from that anchor.
- `waiting-plane-and-reactivation.md` describes waiting and reactivation
  concepts. This RFC narrows the scheduler-facing contract around
  `WaitCondition` and `WakeSource`.
- `external-trigger-capability.md` defines providerless ingress as an
  agent-level capability. This RFC treats external trigger capability as one
  possible wake source, not as the waiting state itself.
- `runtime-scheduler-contract.md` defines broader scheduler behavior. This RFC
  should either extend it or be folded into it after discussion.
