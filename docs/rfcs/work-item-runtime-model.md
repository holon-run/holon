---
title: RFC: Work Item Runtime Model
date: 2026-04-18
status: draft
---

# RFC: Work Item Runtime Model

## Summary

This RFC introduces `Work Item` as a higher-level runtime unit for sustained agent work.

The goal is to let Holon evolve from a message-driven runtime into a work-item-driven runtime, without overloading existing lower-level concepts such as `Turn` and `Task`.

## Problem

Today Holon's runtime semantics are mostly anchored at lower layers:

- message ingress
- turn execution
- internal task execution

Those layers are necessary, but they are not sufficient for a more proactive and continuously operating agent.

In particular, the runtime still lacks a stable unit for:

- ongoing high-level work
- queueing future work without immediately interrupting the current turn
- deciding what should be reactivated on tick
- tracking progress above individual turns and internal tasks

Without that layer, the system is forced to approximate high-level work using lower-level signals.

## Work item vs turn vs task

This RFC introduces `Work Item` as a runtime concept that is intentionally distinct from both `Turn` and `Task`.

These three concepts operate at different layers and should not be used interchangeably.

### Turn

A `Turn` is the smallest conversational execution unit.

It represents one round of agent progress after a user input, timer tick, or other activation event. A turn may include:

- model sampling
- tool calls
- intermediate assistant output
- a terminal turn settlement

A turn answers the question:

- what happened in this round of interaction?

A turn does not answer:

- what high-level work is currently being pursued?
- whether that high-level work is now complete?

### Task

A `Task` is an operational execution unit inside the runtime.

Tasks exist to perform concrete work, such as:

- a command task
- a delegated child-agent task
- a background task
- other runtime-managed execution jobs

A task answers the question:

- what concrete execution is currently running or waiting?

A task does not by itself define the user-visible high-level work goal.

### Work Item

A `Work Item` is the high-level unit of ongoing agent work.

It is the runtime representation of a sustained piece of work the agent is trying to move forward. A work item is larger than a turn and larger than any individual internal task.

A work item answers the question:

- what meaningful piece of work is the agent currently advancing?

A `Work Item` is defined by a distinct delivery target, not merely by message boundaries or execution steps.

Examples:

- fix daemon restart when the pid file is stale
- split `tui.rs` without changing operator-visible behavior
- review a PR and produce findings
- investigate a runtime failure and explain the root cause

A work item may span:

- multiple turns
- multiple internal tasks
- multiple pauses, waits, and resumptions

This makes `Work Item` the correct home for:

- queueing
- activation
- progress tracking
- waiting state
- completion state

### Relationship between the three

The intended relationship is:

- one `Work Item` may span many `Turn`s
- one `Work Item` may create many internal `Task`s
- one `Turn` may advance a `Work Item`
- one `Turn` may also only update state or clarify direction without completing the `Work Item`

In short:

- `Turn` is conversational
- `Task` is operational
- `Work Item` is goal-oriented

### Why this distinction matters

Without a distinct `Work Item` layer, the runtime is forced to overload lower-level concepts:

- treating turn settlement as high-level completion
- treating internal tasks as user-visible goals
- treating raw message ingress as the work queue

That leads to weak proactive behavior, weak resumption behavior, and unclear completion semantics.

This RFC uses `Work Item` to provide the missing higher-level runtime unit.

## Goals

This RFC aims to establish a minimal runtime model that allows Holon to:

- continue work across turns instead of treating each ingress as isolated
- accept new ingress without always interrupting the current work immediately
- decide on tick whether there is runnable work worth activating
- track progress at a level above internal runtime tasks
- create a stable home for future completion and evidence semantics

This RFC does not attempt to fully define all completion policy details. It focuses first on the runtime container and lifecycle for sustained work.

## Work Queue

A `Work Queue` is the runtime container for high-level ongoing work.

It is not:

- a raw message queue
- a transcript index
- an internal task scheduler
- an external issue backlog

Instead, the work queue answers the question:

- what high-level work does the agent currently believe exists in this runtime?

### Why a queue is needed

Without a work queue, the runtime has no stable place to hold:

- the current active work item
- queued follow-on work
- work that is waiting on input
- work that has completed and should no longer trigger activation

This makes tick, proactive behavior, and resume logic much harder to reason about.

### Work queue status model

The minimal initial status model is:

- `active`
- `queued`
- `waiting`
- `completed`

`active`
- the work item currently being advanced

`queued`
- the work item is known and runnable, but not currently active

`waiting`
- the work item exists, but is blocked on missing input or some external condition

`completed`
- the work item is no longer expected to drive activation

This RFC intentionally avoids introducing more statuses until the basic model is working.

## Ingress and work-item resolution

New ingress does not automatically become a new work item.

That would collapse the work queue back into a message queue.

Instead, ingress first enters the runtime as raw input, and then affects the work queue through work-item resolution.

### Ingress examples

Ingress may include:

- a user message
- an issue or PR event
- a command invocation
- a benchmark event
- a webhook or callback

### Resolution outcomes

A new ingress may result in one of the following high-level outcomes:

- update the current work item
- create a new work item
- update an existing queued or waiting work item
- remain informational only

The important point is that ingress and work items are different layers:

- ingress answers what arrived
- work queue answers what work now exists

### Delivery target as the boundary

The key distinction is whether newly discovered work still belongs to the same delivery target.

If the agent discovers additional work that is still required to complete the current delivery target, that work should remain inside the current `Work Item` and expand its `Work Plan`.

Examples:

- adding a missing regression test while fixing the same bug
- extracting a helper while completing the same refactor
- normalizing one internal mapping required to finish the same diagnostics change

If the newly discovered work forms a different delivery target, it may become a new `Work Item`.

Examples:

- while fixing one runtime bug, discovering a separate benchmark reporting feature worth implementing
- while completing one refactor, identifying another independent runtime cleanup line

This RFC intentionally uses delivery target, rather than ingress source, as the boundary for deciding whether work stays inside the current item or becomes a new item.

## Activation and tick behavior

The work queue becomes the basis for activation.

Tick should not ask:

- did any message arrive?

Tick should ask:

- is there any runnable work item that should now be activated?

### Minimal activation rule

The minimal initial rule is:

1. if there is an `active` work item, continue that work item
2. otherwise, if there is a runnable `queued` work item, activate one
3. otherwise, do not wake the agent for more work

This is intentionally simple.

It gives tick a concrete purpose without requiring a full planning system.

### Why this matters

This makes the runtime less dependent on raw ingress for liveness.

A resumed or background-capable agent can keep working because there is known runnable work, not only because a new message arrived.

## Scheduling

The initial scheduling model should remain deliberately simple.

This RFC proposes a single-active-work-item scheduler.

### Scheduling states

Only the following work-item states matter for scheduling:

- `active`
- `queued`
- `waiting`
- `completed`

Runnable work is:

- the current `active` work item
- or, if none is active, a `queued` work item

Non-runnable work is:

- `waiting`
- `completed`

### Minimal scheduling loop

The minimal scheduling loop is:

1. select the work item to advance
2. run one turn against that work item
3. commit work-item state updates

In practice:

- if there is an `active` work item, continue it
- otherwise, if there is a `queued` work item, activate one and continue it
- otherwise, do not wake for more work

### Non-preemptive by default

The initial model should be non-preemptive.

If a work item is already `active`, new ingress should not automatically interrupt it.

Instead:

- if the ingress still belongs to the same delivery target, it should update the current work item
- if it forms a different delivery target, it should usually become a `queued` work item

This keeps the scheduler stable and avoids unnecessary objective thrashing.

### Tick behavior

Tick should not ask whether any message has recently arrived.

Tick should ask whether there is any runnable work item worth activating.

The minimal tick rule is:

1. if there is an `active` work item, wake and continue it
2. else if there is a runnable `queued` work item, activate one and wake it
3. else remain idle

### One active work item per agent

The initial model should allow only one `active` work item per agent.

This does not forbid multiple internal runtime tasks or delegated child tasks.

It only means that, at the high-level work-item layer, the agent is advancing one top-level delivery target at a time.

### End-of-turn work-item transitions

At the end of a turn, the current `active` work item should normally transition to one of:

- `active`
- `waiting`
- `completed`
- `queued`

Typical cases:

- `active -> active`
  - more work remains and should continue soon
- `active -> waiting`
  - progress now depends on user input or some external condition
- `active -> completed`
  - the work item is finished and should stop driving activation
- `active -> queued`
  - the item remains unfinished, but does not need to monopolize the active slot

## Progress and state updates

Each work item should be able to carry lightweight progress state.

The minimal useful fields are:

- `id`
- `delivery_target`
- `status`
- `summary`
- `progress_note`
- `created_at`
- `updated_at`

`delivery_target` is the stable statement of what this work item is trying to deliver.

`summary` and `progress_note` are runtime-facing projections of current understanding and recent progress.

The `Work Plan` represents how the runtime currently intends to achieve the `delivery_target`.

The goal is not to create a full project-management system.

The goal is to let the runtime answer:

- what are we doing?
- what happened since last activation?
- why is this still active, queued, or waiting?

## Persistence model

`WorkItem` and `WorkPlan` should be persisted as first-class runtime records.

They should not be embedded directly into `AgentState`.

`AgentState` remains the home for:

- runtime posture
- wake/sleep state
- continuation state
- compacted context metadata
- other per-agent lifecycle state

The work queue is a separate higher-level state layer and should use its own
persisted store.

This keeps the runtime model explicit:

- `AgentState` answers how the runtime is currently postured
- `WorkItem` answers what meaningful work currently exists
- `WorkPlan` answers the current checklist for one work item

The first implementation may store these as append-only runtime records, using
the same general persistence style already used for tasks, timers, and other
runtime snapshots.

`WorkPlan` is work-item-scoped.

It should not be treated as identical to an agent-wide todo snapshot, even if
the initial plan-step status set overlaps with existing todo/checklist concepts.

## Prompt and tool model

The runtime should not rely on prompt injection alone, and it should not rely on tools alone.

The proposed model is:

1. prompt projection provides awareness
2. tools provide explicit state mutation

### Prompt projection

At the start of a turn, the runtime should inject a compact work-queue summary into the prompt.

That projection should distinguish between the current active work item and non-active work items.

For the current active work item, the runtime should inject the full current snapshot, including:

- `id`
- `delivery_target`
- `status`
- `summary`
- `progress_note`
- `parent_id` when present

It should also inject the current full work-plan snapshot for that active work item.

For non-active work items, prompt projection should remain compact.

That compact summary should include:

- a small number of `queued` work items
- whether any work item is currently `waiting`
- short progress summaries where useful

This makes the agent work-item-aware by default.

`completed` work items should not be injected into the normal prompt projection.

### Tool model

The initial tool surface can remain write-oriented.

A first phase does not need a dedicated read tool if prompt projection already provides current awareness.

The minimal initial tool surface is:

- `update_work_item`
- `update_work_plan`

`update_work_item` is the single mutation tool for high-level work-item state. It is responsible for:

- creating a work item
- updating `delivery_target`
- updating `summary`
- updating `progress_note`
- updating `status`

`update_work_plan` manages the current structured checklist for a work item.

This is intentionally closer to an explicit checklist/progress update model than to a large family of fine-grained patch tools.

The source of truth remains the persisted work-item store, not the prompt.

The prompt is only a projection of that store.

### Adoption model

The initial rollout should remain message-driven by default.

In early phases, the runtime should still be able to operate normally even when
no `WorkItem` exists yet.

This means:

- message ingress continues to drive turns as it does today
- `WorkItem` is optional during early rollout phases
- the runtime should not require a semantic ingress-to-work-item resolver as a
  prerequisite for normal operation

Instead, work items should be adopted explicitly through higher-level mutation
paths.

The initial explicit adoption paths are:

- agent-issued `update_work_item`
- agent-issued `update_work_plan`
- a host/control-plane path that can enqueue a new queued work item directly

This keeps `WorkItem` as an explicit runtime container rather than turning every
incoming message into a required work-item classification problem.

### Proposed minimal schemas

The initial `update_work_item` shape should be:

- `id` optional
- `delivery_target` required
- `status` required
- `summary` optional
- `progress_note` optional
- `parent_id` optional

`id`
- omitted when creating a new work item
- provided when updating an existing work item
- generated by the system, not by the agent

`delivery_target`
- the stable statement of what this work item is trying to deliver

`summary`
- a compact statement of the current overall state of the work item

`progress_note`
- the latest meaningful checkpoint or blocker note
- intentionally stores only the latest note, not a history log

The initial `update_work_plan` shape should be:

- `work_item_id` required
- `plan` required

Each plan item should contain:

- `step`
- `status`

The initial step status set should be:

- `pending`
- `in_progress`
- `completed`

### Work-plan update semantics

`update_work_plan` should use full-snapshot replacement semantics.

When a plan item changes status, the agent should submit the current complete work-plan snapshot, not a patch for one individual item.

This keeps the first version simpler for both the agent and the runtime:

- the agent rewrites the current full checklist
- the runtime stores the latest plan snapshot
- prompt projection can read from one stable current plan

## Completion boundary

This RFC does not fully specify work-item completion semantics.

It only establishes where completion belongs.

Completion should be attached to `Work Item`, not overloaded onto:

- turn settlement
- internal task termination
- raw ingress exhaustion

Future RFCs or follow-up issues can define:

- what evidence is required to complete a work item
- when a work item should remain waiting instead of completing
- how closure claims and evidence artifacts should interact

## Work-item transition semantics

Transitions between:

- `active`
- `waiting`
- `queued`
- `completed`

should not be decided by runtime code alone, and should not be accepted from agent prose alone.

The proposed model is:

1. agent transition claim
2. minimal runtime fact check
3. scheduler/controller commit

### Agent transition claim

At the end of a turn, the agent should be able to express its intended transition for the current active work item.

The useful transition intents are:

- remain `active`
- move to `waiting`
- move to `queued`
- mark `completed`

The agent should also provide a short reason for the transition.

This keeps the semantic interpretation close to the model, rather than trying to fully hard-code it in the runtime.

### Minimal runtime fact check

Runtime fact checks should remain intentionally small.

The runtime should only guard against obvious contradictions, such as:

- marking a work item `completed` while there is still clearly active unfinished execution
- marking a work item `waiting` without any stated waiting reason

The runtime should not attempt full semantic judgment about whether the delivery target is truly satisfied.

Its role is only to prevent obviously inconsistent state transitions.

### Scheduler/controller commit

The scheduler or controller should commit the final persisted state transition.

This layer combines:

- the agent's transition claim
- the minimal runtime facts

and writes the resulting work-item state.

### Default bias

The initial default bias should be conservative:

- if there is no convincing reason to move out of `active`, remain `active`
- `waiting` should require an explicit reason
- `completed` should require that runtime facts do not show obvious unfinished execution

This keeps first-version behavior simple while avoiding the most obvious forms of false completion.

## Non-goals

This RFC does not attempt to:

- define full evidence or verification policy
- introduce a separate resolver agent
- model the full external issue backlog inside the runtime
- define every future status or scheduling policy
- redesign the user-facing UI in this document

## Open questions

The main open questions after this RFC are:

1. how explicit work-item mutation should be in the tool surface
2. how much work-item state should be injected into prompt projection
3. how completion should interact with closure evidence

## Current design direction

This RFC currently assumes:

- work-item boundaries are determined by delivery target
- newly discovered work that still serves the same delivery target should extend the current `Work Plan`
- newly discovered work that forms a different delivery target may become a new `Work Item`
- the first rollout remains message-driven by default
- `WorkItem` creation is explicit in early phases rather than inferred from every ingress
- a direct control-plane enqueue path is allowed for future queued work

This RFC does not yet impose stronger policy constraints on agent-derived work creation.

The intention is to observe real runtime behavior first before introducing tighter restrictions.
