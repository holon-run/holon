---
title: RFC: Agent State Model And Runtime Projection
date: 2026-05-19
status: draft
---

# RFC: Agent State Model And Runtime Projection

## Summary

Holon should define agent state as a layered runtime model rather than a single
opaque status string or an ever-growing `AgentSummary` payload.

An agent's observable state should be derived from authoritative records:
agent identity and lifecycle records, the current turn, queue state, WorkItems,
tasks, wait conditions, external trigger capability, and capability snapshots.
`AgentSummary` should be a stable projection assembled from those sources, not
the state source that the scheduler, UI, or API consumers treat as canonical.

This RFC complements
[Scheduler Wait State And Recoverable Agent Continuation](./scheduler-wait-state.md).
That RFC defines how WorkItems become runnable, waiting, blocked, or completed.
This RFC defines how the agent as a whole should express its runtime posture
across queue, turn, work, task, wait, wake, and capability state.

## Problem

Holon agents are long-lived runtime actors. A single agent can have:

- a durable identity and lifecycle;
- a current or recently completed turn;
- pending input in its queue;
- open WorkItems with different scheduling states;
- active command or child-agent tasks;
- external triggers and waiting intents;
- active model, tool, and execution capabilities;
- user-facing state shown through `/agents`, `/state`, `AgentGet`, or TUI.

Today these concepts are easy to collapse into vague labels such as
`sleeping`, `idle`, `waiting`, or `running`. Those labels hide important
distinctions:

- sleeping with runnable work is not truly idle;
- waiting for an internal task is different from waiting for external state;
- waiting for the operator is different from being blocked;
- queue input should take precedence over passive sleep posture;
- model/provider availability is capability state, not lifecycle state;
- `AgentSummary` is useful for display but should not become the source of
  truth for scheduling.

Without an explicit state model, different runtime surfaces can disagree:

- UI may show an agent as idle while the scheduler sees runnable work;
- scheduler code may depend on fields that were intended only for display;
- API consumers may not know which fields are stable contract and which are
  debug or convenience projection;
- runtime changes may keep adding unrelated fields to `AgentSummary`;
- `Sleep` may be mistaken for an authoritative agent state instead of a
  turn-end posture.

## Goals

- Define the authoritative layers that make up agent state.
- Distinguish durable state, runtime state, derived state, and user-facing
  projection.
- Define an agent-level scheduling posture that can be derived from queue,
  turn, WorkItem, task, and wait state.
- Make clear that `Sleep` is not the source of truth for whether an agent is
  idle.
- Keep `AgentSummary` stable and user-facing without making it a dumping ground
  for internal records.
- Give scheduler, API, and UI code a shared vocabulary for agent state.
- Support incremental migration from the current runtime structures.

## Non-goals

- Do not design a UI layout.
- Do not define provider-specific states such as GitHub checks, reviews,
  deployments, or inbox policy.
- Do not move every runtime record into `AgentSummary`.
- Do not make agents manually maintain derived status strings.
- Do not replace WorkItem scheduling state; this RFC consumes the WorkItem
  state model defined by the scheduler wait-state RFC.
- Do not require a large immediate schema rewrite before the projection is
  useful.

## Current structure

The repository already has several relevant surfaces:

- agent identity, profile, lifecycle, and execution snapshots;
- WorkItem records with lifecycle, plan status, blockers, readiness, and todo
  progress;
- command and child task lifecycle records;
- waiting intents and external trigger capability records;
- runtime closure and scheduler logic;
- `AgentSummary`, `AgentGet`, `/agents`, `/state`, and TUI-facing projections;
- model/provider availability data exposed independently through the model
  capability surface.

This RFC does not propose replacing those records. It proposes clarifying which
records are authoritative and how a stable agent projection should be derived.

## State layers

Agent state should be described in layers. Each layer has a different owner and
stability expectation.

### Agent identity and lifecycle

Authoritative source: agent registry and lifecycle records.

This layer answers:

- which agent is this;
- is the agent public or private;
- who owns it;
- which profile/template contract applies;
- does the agent still exist;
- is it active, archived, terminal, or otherwise unavailable.

This state changes rarely and should be safe to expose through stable API
projection.

### Turn execution state

Authoritative source: turn lifecycle and closure records.

This layer answers:

- is a model turn currently active;
- what triggered the current or last turn;
- did the last turn close normally, with an error, or by cancellation;
- what posture did the agent submit at turn end.

Turn state is transient. It is important for scheduling and operator
observability, but it should not be confused with durable agent lifecycle.

### Queue state

Authoritative source: message queue and admission records.

This layer answers:

- are there pending operator, external, system, or self-enqueued messages;
- what priority and provenance do they have;
- is the agent eligible to start another turn.

Queue state should take precedence over passive sleep posture. An agent with
queued input is not merely asleep.

### Work state

Authoritative source: WorkItem records and the derived WorkItem scheduling
model.

This layer answers:

- what is the current focused WorkItem;
- which WorkItems are open, completed, queued, blocked, waiting for operator,
  or runnable;
- whether any WorkItem can be automatically continued.

The scheduler wait-state RFC defines the WorkItem-level states:

```text
Runnable
WaitingOperator
WaitingTask
WaitingExternal
Blocked
Completed
```

Agent state consumes those derived WorkItem states rather than reimplementing
them as a separate hand-maintained status.

### Task state

Authoritative source: task lifecycle records.

This layer answers:

- are command tasks or child-agent tasks active;
- are any tasks accepting input;
- which WorkItems or waits depend on task results;
- did task completion produce a continuation trigger.

Task state is runtime-owned. Waiting for a task should be represented as a task
dependency or wait condition, not as an unstructured `blocked_by` string.

### Wait and wake state

Authoritative source: wait condition, waiting intent, timer, task result,
operator input, and external trigger records.

This layer answers:

- is the agent waiting for operator, task, external, or timer state;
- which wake sources can reactivate it;
- whether the wait is recoverable, weak, expired, resolved, or cancelled;
- what continuation should run when a wake source fires.

External trigger capability is part of the wake surface. It should be modeled
as ingress capability or wake source, not as the whole agent state.

### Capability state

Authoritative source: runtime capability and provider snapshots.

This layer answers:

- which model is active;
- which models/providers are currently available;
- which tools and execution resources are exposed;
- which workspace and execution environment are active.

Capability state informs what the agent can do. It should not be overloaded as
work state or lifecycle state.

### User-facing projection

Authoritative source: derived projection assembled from the above layers.

This layer answers:

- what should `/agents`, `/state`, `AgentGet`, and TUI show;
- what status should be stable enough for clients to depend on;
- what details are diagnostic and may remain runtime-internal.

`AgentSummary` belongs to this layer. It should summarize stable facts and
derived posture, but it should not become the primary store for queue, task,
wait, or capability records.

## Derived AgentSchedulingPosture

The runtime should derive an agent-level scheduling posture from the layers
above.

Initial shape:

```text
AgentSchedulingPosture =
  ActiveTurn
  HasQueuedInput
  HasRunnableWork
  WaitingForTask
  WaitingForExternal
  WaitingForOperator
  Blocked
  Idle
  Archived
```

This posture is not manually written by the agent. It is derived.

Suggested precedence:

1. `Archived` if the agent lifecycle is terminal or unavailable.
2. `ActiveTurn` if a turn is currently executing.
3. `HasQueuedInput` if admitted queue input is pending.
4. `HasRunnableWork` if any open WorkItem is runnable.
5. `WaitingForTask` if open work is waiting on runtime-owned task results.
6. `WaitingForExternal` if open work is waiting on external wake sources.
7. `WaitingForOperator` if open work requires operator input.
8. `Blocked` if open work has only non-recoverable or unstructured blockers.
9. `Idle` if no open work, queued input, active turn, or active wait remains.

The exact precedence can evolve, but the important rule is that passive sleep
posture must not hide higher-priority facts such as queued input or runnable
work.

## Sleep semantics

`Sleep` should not be treated as the authoritative agent state.

`Sleep` is a turn-end action that says the agent is yielding control after
submitting whatever durable state it has chosen to submit. The runtime should
then derive the actual agent posture from queue, WorkItem, task, wait, and
lifecycle records.

Examples:

```text
Sleep + runnable WorkItem
  => AgentSchedulingPosture::HasRunnableWork
  => runtime should enqueue or schedule continuation

Sleep + active task wait
  => AgentSchedulingPosture::WaitingForTask
  => runtime can rest until task result continuation

Sleep + needs_input WorkItem
  => AgentSchedulingPosture::WaitingForOperator
  => runtime should wait for operator input

Sleep + no open work and no queue
  => AgentSchedulingPosture::Idle
```

This keeps `sleeping` as a runtime posture, not a broad state bucket that
swallows runnable, waiting, blocked, and idle cases.

## AgentSummary contract

`AgentSummary` should be a stable projection, not the canonical state store.

It may include:

- identity and lifecycle facts;
- current workspace/profile/model summary;
- current turn or execution snapshot;
- current WorkItem focus and compact work-state summary;
- derived `AgentSchedulingPosture`;
- compact waiting/wake summary;
- capability summary useful to clients.

It should avoid:

- embedding full WorkItem lists;
- embedding full task output or task records;
- embedding provider-specific business state;
- duplicating large model/provider metadata when a separate capability endpoint
  exists;
- fields that scheduler logic must mutate directly to make state true.

If a field is only useful for debugging, it should be clearly marked as a
diagnostic projection or live under a separate debug endpoint.

## API and UI expectations

### `/agents`

Should expose compact per-agent identity, lifecycle, and derived posture. It
should be enough for a client to distinguish:

- active;
- queued;
- runnable;
- waiting for task;
- waiting for external source;
- waiting for operator;
- blocked;
- idle;
- archived.

### `/state`

Should expose the bootstrap projection needed by the current client without
becoming the canonical store for every runtime record. Large or independently
scoped capability data should stay on dedicated endpoints where possible.

### `AgentGet`

Should provide a richer agent-plane projection, including derived posture and
compact lineage/task/work summaries. It should not be treated as a transcript
dump or raw ledger export.

### TUI

Should display derived posture and compact reasons rather than inferring state
from vague labels. For example, "waiting for external wake" and "has runnable
work" should be different display states even if the last turn ended with
`Sleep`.

## Scheduler contract

The scheduler should rely on authoritative records and derived posture, not on
display strings.

Scheduler owns:

- deriving agent posture from queue, turn, WorkItem, task, and wait state;
- preventing silent indefinite rest when queued input or runnable work exists;
- routing task, timer, operator, external, and system wake sources into
  continuations;
- exposing enough audit information to explain why an agent is or is not
  runnable.

Scheduler does not own:

- interpreting provider-specific business state;
- deciding that CI, review, deployment, or inbox state is semantically done;
- maintaining user-facing display labels as source of truth;
- stuffing every runtime detail into `AgentSummary`.

## Migration plan

### Phase 1: Document and name the layers

- Adopt the state-layer vocabulary in RFCs and runtime comments.
- Treat `AgentSummary` as projection in new code.
- Avoid adding unrelated authoritative state directly to `AgentSummary`.

### Phase 2: Introduce derived posture

- Add an internal derived `AgentSchedulingPosture`.
- Compute it from lifecycle, active turn, queue, WorkItem scheduling state,
  task, and wait data.
- Surface it in `AgentGet` and compact API projections.

### Phase 3: Align closure and scheduler behavior

- Make turn closure use derived posture when deciding whether sleep can become
  true idle.
- Ensure queued input and runnable WorkItems override indefinite sleep.
- Ensure task and external waits are represented through structured wait
  state where possible.

### Phase 4: Normalize projection surfaces

- Update `/agents`, `/state`, and TUI to display derived posture consistently.
- Move large or independently scoped capability data behind dedicated
  capability endpoints.
- Keep debug-only fields out of stable client contracts.

### Phase 5: Audit ambiguous states

- Emit diagnostics for agents that appear idle while open work is runnable.
- Emit diagnostics for unstructured blockers that look recoverable but have no
  wait condition.
- Emit diagnostics for external waits without a durable or recoverable wake
  path, following the scheduler wait-state RFC.

## Open questions

- Should `AgentSchedulingPosture` be a single enum, or should the API expose a
  primary posture plus secondary flags such as `has_active_tasks` and
  `has_weak_external_wait`?
- Should `HasQueuedInput` outrank `ActiveTurn` in any projection where queued
  input indicates backpressure?
- What is the minimal stable posture set needed by TUI and API clients?
- Which existing `AgentSummary` fields should be classified as stable,
  diagnostic, or deprecated?
- How much wait detail should be exposed in compact `/agents` responses versus
  richer `AgentGet` responses?
- Should archived or terminal agents retain their last derived posture for
  historical display, or always collapse to `Archived`?

## Design principles

- Source-of-truth records should remain explicit and narrow.
- Derived state should be recomputable from authoritative records.
- Projection should serve users and clients, not drive hidden state mutation.
- `Sleep` should yield execution; it should not erase runnable work.
- Capability, lifecycle, work, task, wait, and queue state should remain
  distinguishable even when summarized.
