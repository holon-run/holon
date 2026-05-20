---
title: RFC: Scheduler Wait State And Recoverable Agent Continuation
date: 2026-05-19
status: draft
---

# RFC: Scheduler Wait State And Recoverable Agent Continuation

## Summary

Holon's scheduler should derive an explicit scheduling state for each open
WorkItem and use that state to decide whether an agent should continue now,
wait for a trusted wake source, ask the operator, remain blocked, or become
idle.

`Sleep` should not by itself mean "there is no runnable work". It should become
the agent's posture after the agent has committed enough durable state for the
runtime to classify the work. If any WorkItem is still runnable, an indefinite
sleep must not silently strand the agent; the runtime should enqueue or
schedule a continuation instead.

The scheduler should also gain a generic `WaitCondition` / `WakeSource` model.
This model records that a WorkItem is waiting, what opaque subject it is waiting
on, and which runtime-observable sources can reactivate the agent. The scheduler
does not interpret provider-specific meanings such as CI, review, merge,
inbox, or deployment state.

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
- `Sleep` calls.

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
- Represent waiting as structured runtime state instead of relying only on
  natural-language blockers.
- Keep task result, external ingress, timer, operator input, and system tick
  wakeups in one generic wake-source model.
- Preserve the boundary between generic runtime scheduling and provider or
  skill-specific business policy.
- Support incremental migration from existing WorkItem, waiting intent, task,
  and external trigger records.

## Non-goals

- Do not define GitHub, CI, review, merge, deployment, inbox, or other
  provider-specific fallback durations.
- Do not make the scheduler decide whether an external condition is
  semantically resolved.
- Do not require every external wait to have a timer in the first
  implementation phase.
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

This RFC does not propose replacing those pieces. It proposes adding a clearer
derived scheduling layer over them and evolving waiting intents into a more
general `WaitCondition` model.

## Core model

### WorkItemSchedulingState

The scheduler should derive a state for each WorkItem:

```text
WorkItemSchedulingState =
  Runnable
  WaitingOperator
  WaitingTask
  WaitingExternal
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

Runnable WorkItems should cause the runtime to continue agent execution. If an
agent calls indefinite `Sleep` while runnable WorkItems exist, the runtime must
not treat the agent as truly idle. It should enqueue or schedule continuation.

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

### Blocked

A WorkItem is blocked when it has an explicit blocker that is not represented
as a structured active wait.

Natural-language blockers remain useful for operator visibility, but they are
not enough for reliable automatic continuation. Over time, recoverable blockers
should become structured wait conditions.

### Completed

Completed WorkItems do not participate in scheduling.

## WaitCondition

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

`Sleep` should be treated as a rest posture after state has been committed, not
as the primary source of scheduling truth.

At a turn boundary, the runtime should be able to classify the agent posture as:

```text
ContinueNow
Wait
NeedOperator
Blocked
Complete
Idle
```

The preferred durable forms are:

- runnable WorkItem exists -> `ContinueNow`;
- active wait condition exists -> `Wait`;
- WorkItem needs input -> `NeedOperator`;
- WorkItem has unstructured blocker -> `Blocked`;
- WorkItem completed -> `Complete`;
- no open work -> `Idle`.

If an agent calls indefinite `Sleep` while any WorkItem is `Runnable`, closure
should enqueue or schedule a continuation instead of leaving the agent asleep.

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
- persisting and displaying structured wait lifecycle;
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
- `Blocked` -> expose blocker and do not auto-continue;
- `Completed` -> ignore for scheduling.

### 3. Introduce WaitCondition ledger records

Generalize or migrate waiting intents into `WaitCondition` records.

The first version can keep the shape small:

```text
kind
work_item_id
waiting_for
wake_sources
continuation
status
```

### 4. Normalize wake-source continuation

Task result, external ingress, timer, operator input, and system tick should all
enqueue continuation that asks the agent to reconcile the wait, rather than
implicitly resolving the wait.

### 5. Add external wait audit

Surface weak external waits that have no timer, durable queue, or explicit
`no_fallback` reason.

## Open questions

- Should `WaitingOperator` be represented only through `plan_status =
  needs_input`, or should it also be a `WaitCondition(kind = operator)`?
- Should `blocked_by` remain a separate WorkItem field forever, or eventually
  become display text attached to a `Blocked` scheduling record?
- What is the minimum continuation payload needed for reliable wake
  reconciliation?
- Should system tick be a first-class `WakeSource` or a scheduler-only audit
  mechanism?
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
