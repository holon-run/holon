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
- blocker state
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

- the agent's current work-item focus
- follow-on work
- work that is blocked on input or an external condition
- work that has completed and should no longer trigger activation

This makes tick, proactive behavior, and resume logic much harder to reason about.

### Work queue lifecycle model

The minimal initial lifecycle model is:

- `open`
- `done`

`open`
- the work item still represents unfinished work

`done`
- the work item is complete and should no longer drive activation

The current work item is not represented as a lifecycle status.

Instead, the agent owns an explicit focus pointer:

- `current_work_item_id`

If `current_work_item_id` points to an open work item, that item is the current
work item for the agent.

Queued and blocked are derived views, not primary statuses:

- queued work is `open` work that is not the current work item and has no blocker
- blocked work is `open` work with `blocked_by` set
- completed work is `done` work

This avoids forcing the agent to encode scheduling intent by writing a status
field such as `active` or `waiting`. The agent can instead call explicit actions
such as `PickWorkItem` and `CompleteWorkItem`.

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
- update an existing open or blocked work item
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

1. if `current_work_item_id` points to an open, unblocked work item, continue that work item
2. otherwise, if there is another open, unblocked work item, wake the agent so it can pick one
3. otherwise, do not wake the agent for more work

This is intentionally simple.

It gives tick a concrete purpose without turning the runtime into a semantic
scheduler.

The runtime may surface candidate work items, but the agent is responsible for
choosing the current work item with `PickWorkItem`.

### Why this matters

This makes the runtime less dependent on raw ingress for liveness.

A resumed or background-capable agent can keep working because there is known runnable work, not only because a new message arrived.

## Scheduling

The initial scheduling model should remain deliberately simple.

This RFC proposes a single-current-work-item model.

### Lifecycle and focus

Only the following stored lifecycle states matter for the first version:

- `open`
- `done`

The current work item is stored separately as:

- `current_work_item_id`

Runnable work is:

- any `open` work item whose `blocked_by` field is not set

Blocked work is:

- any `open` work item whose `blocked_by` field is set

Completed work is:

- any `done` work item

`queued` and `blocked` can still appear in UI or prompt copy as derived labels,
but they should not be fields the agent has to set directly.

### Minimal scheduling loop

The minimal scheduling loop is:

1. determine whether there is a current runnable work item
2. if not, surface open runnable candidates to the agent
3. let the agent pick the current work item
4. run one turn against that current work item
5. persist explicit work-item actions from the agent

### Non-preemptive by default

The initial model should be non-preemptive.

If the agent already has a current work item, new ingress should not automatically interrupt it.

Instead:

- if the ingress still belongs to the same delivery target, it should update the current work item
- if it forms a different delivery target, it should usually become another open work item

This keeps the scheduler stable and avoids unnecessary objective thrashing.

### Tick behavior

Tick should not ask whether any message has recently arrived.

Tick should ask whether there is any runnable work item worth activating.

The minimal tick rule is:

1. if the current work item is open and unblocked, wake and continue it
2. else if there is another open and unblocked work item, wake so the agent can pick it
3. else remain idle

### One current work item per agent

The initial model should allow only one current work item per agent.

This does not forbid multiple internal runtime tasks or delegated child tasks.

It only means that, at the high-level work-item layer, the agent is advancing one top-level delivery target at a time.

### Focus changes

Switching work items should be an explicit tool action, not an implicit status
write.

`PickWorkItem` sets `current_work_item_id`.

If there was a previous current work item, it remains open unless the agent
explicitly completes it or marks it blocked through a separate action.

This keeps switching simple for the agent:

- pick the work item to work on now
- update it if its checklist or blocker changed
- complete it when the delivery target is satisfied

## Progress and state updates

Each work item should carry lightweight task-card state.

The minimal useful fields are:

- `id`
- `delivery_target`
- `state`
- `blocked_by`
- `plan`
- `result_summary`
- `created_at`
- `updated_at`

`delivery_target` is the statement of what this work item is trying to deliver.
It should remain stable across normal progress updates, but it may be refined
with an explicit `UpdateWorkItem.delivery_target` call when the agent has
learned a narrower or clearer statement for the same underlying task. Agents
should not create duplicate work items solely to refine the target wording.

`id` is generated by the runtime, not supplied by the agent. The current
implementation uses a `work_` prefix plus a UUID v4 value. The identifier should
be treated as globally unique in practice, not as a per-agent or per-workspace
sequence.

`state` is `open` or `done`.

`blocked_by` is optional and explains why an open work item cannot currently be
advanced.

`plan` is the current checklist for reaching the delivery target.

`result_summary` is optional completion metadata written when the work item is
completed.

Progress narration should normally remain in the transcript, briefs, tool
records, and final messages. The runtime should associate those records with
the current work item through `current_work_item_id` and explicit
`work_item_id` fields, rather than asking the agent to duplicate progress prose
inside the work item itself.

The goal is not to create a full project-management system.

The goal is to let the runtime answer:

- what are we doing?
- what checklist remains?
- is this item blocked?
- what transcript/tool/brief records belong to this item?

## Persistence model

`WorkItem` and `WorkPlan` should be persisted as first-class runtime records.

They should not be embedded directly into `AgentState`.

`current_work_item_id` is per-agent focus state. It may be stored as a small
work-queue focus record or as an explicit field on agent runtime state, but it
should not be inferred from a work-item lifecycle status.

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
- `current_work_item_id` answers which work item the agent is currently advancing
- `WorkPlan` answers the current checklist for one work item

The first implementation may store these as append-only runtime records, using
the same general persistence style already used for tasks, timers, and other
runtime snapshots.

`WorkPlan` is work-item-scoped.

It should not be treated as identical to an agent-wide todo snapshot, even if
the initial plan-step state set overlaps with existing todo/checklist concepts.

## Prompt and tool model

The runtime should not rely on prompt injection alone, and it should not rely on tools alone.

The proposed model is:

1. prompt projection provides awareness
2. tools provide explicit state mutation

### Prompt projection

At the start of a turn, the runtime should inject a compact work-queue summary into the prompt.

That projection should distinguish between the current work item and other open work items.

For the current work item, the runtime should inject the full current snapshot, including:

- `id`
- `delivery_target`
- `state`
- `blocked_by` when present
- `plan` when present

The prompt should call this section `current_work_item`, not `active_work_item`.

For other open work items, prompt projection should remain compact.

That compact summary should include:

- a small number of runnable open work items
- a small number of blocked open work items
- each item's id, delivery target, and blocker when present

This makes the agent work-item-aware by default.

`done` work items should not be injected into the normal prompt projection.

If the agent changes focus during a turn with `PickWorkItem`, the initial prompt
will still contain the old projection. The `PickWorkItem` tool result must
therefore return the new current work-item snapshot and clearly state that
subsequent tool calls in the current turn are bound to the new current work
item.

### Tool model

The initial tool surface should be action-oriented.

The agent should not have to encode scheduling decisions by writing lifecycle
status strings. It should call tools whose names match the intended action.

The proposed tool surface is:

- `CreateWorkItem`
- `PickWorkItem`
- `UpdateWorkItem`
- `CompleteWorkItem`
- `GetWorkItem`
- `ListWorkItems`

`CreateWorkItem` creates a new open work item.

`PickWorkItem` sets the agent's `current_work_item_id`.

`UpdateWorkItem` updates mutable task-card fields for an existing work item,
including delivery-target refinements for the same underlying task.

`CompleteWorkItem` marks a work item done and optionally records the completion
summary.

`GetWorkItem` and `ListWorkItems` provide explicit read
access so the prompt projection does not become a hidden database query surface.
Agents should prefer these read tools before switching, completing, or expanding
cross-turn work.

There is no separate `UpdateWorkPlan` in this model. The current plan is
exposed through the work-item tool surface and is updated by `UpdateWorkItem`
using full-snapshot replacement semantics. The storage layer may still persist
plan snapshots separately from work-item snapshots.

The source of truth remains the persisted work-item store and focus pointer,
not the prompt.

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

- agent-issued `CreateWorkItem`
- agent-issued `PickWorkItem`
- agent-issued `UpdateWorkItem`
- agent-issued `SpawnAgent` with work-item delegation metadata
- a host/control-plane path that can create a new open work item directly

This keeps `WorkItem` as an explicit runtime container rather than turning every
incoming message into a required work-item classification problem.

### Proposed minimal schemas

The initial `CreateWorkItem` shape should be:

- `delivery_target` required
- `plan` optional

`delivery_target`
- the statement of what this work item is trying to deliver
- should not be edited during normal progress updates
- may be updated to refine or narrow the same underlying task instead of
  creating a duplicate work item

`plan`
- optional full checklist snapshot

The initial `PickWorkItem` shape should be:

- `work_item_id` required

The tool should return:

- the new current work item snapshot
- the previous current work item snapshot when present
- a clear binding note that subsequent tool calls in this turn are associated
  with the new current work item unless they explicitly specify another
  `work_item_id`

The initial `UpdateWorkItem` shape should be:

- `work_item_id` required
- `delivery_target` optional
- `blocked_by` optional
- `plan` optional

`delivery_target`
- refines the current task statement for the same underlying work item
- must not be empty
- should be used instead of `CreateWorkItem` when the agent is only narrowing
  the current target after bounded inspection

`blocked_by`
- set when the item cannot currently be advanced
- omitted when no blocker changes
- explicitly cleared when the item becomes runnable again

`plan`
- replaces the current full checklist snapshot for the work item

Each plan item should contain:

- `step`
- `state`

The initial step state set should be:

- `pending`
- `doing`
- `done`

The initial `CompleteWorkItem` shape should be:

- `work_item_id` required
- `result_summary` optional

`result_summary`
- the short completion record, not a full progress log

The initial read shapes should be:

- `GetWorkItem(work_item_id, include_plan?)`
- `ListWorkItems(filter?, limit?, include_plan?)`

Useful initial filters are:

- current work item
- all open work items
- queued open work items
- blocked open work items
- done work items

The first rollout removes the old status-writing surface in one pass. Read
results expose `open` and `done` lifecycle state, plus a derived `focus` view:

- `current`
- `queued`
- `blocked`
- `done`

`current_work_item_id` is presented as focus, separate from lifecycle.

### Work-plan update semantics

Work-plan updates should use full-snapshot replacement semantics.

When a plan item changes state, the agent should submit the current complete work-plan snapshot, not a patch for one individual item.

This keeps the first version simpler for both the agent and the runtime:

- the agent rewrites the current full checklist
- the runtime stores the latest plan snapshot
- prompt projection can read from one stable current plan

## Delegation and child agents

Child-agent delegation should not be represented by a generic `parent_id` field
on `WorkItem`.

The ordinary work-item model should stay flat:

- `CreateWorkItem` creates one task card for the current agent
- same-agent decomposition should normally be represented in the work plan
- cross-agent delegation should be represented by a structured delegation record

This avoids making every work item carry ambiguous hierarchy metadata before the
runtime has UI, scheduling, query, or completion semantics for arbitrary work
graphs.

### SpawnAgent work-item delegation

The first version should not introduce a separate `DelegateWorkItem` tool.

Instead, `SpawnAgent` should accept optional work-item delegation metadata:

```text
SpawnAgent(
  summary,
  prompt,
  preset?,
  agent_id?,
  template?,
  workspace_mode?,
  work_item?: {
    parent_work_item_id: string,
    child_delivery_target?: string,
    child_plan?: WorkPlanItem[]
  }
)
```

If `work_item` is omitted, `SpawnAgent` behaves as a normal child-agent spawn
without work-item delegation semantics.

If `work_item` is present, the runtime should:

- validate that `parent_work_item_id` belongs to the parent agent
- create a child work item for the spawned child agent
- set the child agent's `current_work_item_id` to the child work item
- create a delegation record linking parent and child
- return the child work item id and delegation id in the `SpawnAgent` result

The child delivery target may be provided explicitly. If omitted, the runtime
may derive it from the spawn summary or prompt.

### Delegation record

A delegation record should include:

- `delegation_id`
- `parent_agent_id`
- `parent_work_item_id`
- `child_agent_id`
- `child_work_item_id`
- `state`
- `result_summary` when complete

Delegation state should be separate from work-item blocker state.

The `WorkItem` schema should not include `parent_work_item_id`. Read surfaces
and prompt projection can return delegation context by joining delegation
records with work items:

- child-side reads can include the parent agent, parent work item, and
  delegation id
- parent-side reads can include child delegations for that work item

This keeps `WorkItem` focused on the task card while still making delegation
context visible when it matters.

Spawning a child agent does not automatically make the parent work item blocked.
The parent agent may continue working on the same work item, switch to another
work item, or explicitly set `blocked_by` if it is truly waiting on the child.

Child-agent results must be associated back to the parent work item through the
delegation record, not by looking at the parent agent's current focus when the
result is delivered. This keeps result routing correct even if the parent agent
has already picked a different current work item.

## Completion boundary

This RFC does not fully specify work-item completion semantics.

It only establishes where completion belongs.

Completion should be expressed by `CompleteWorkItem` and persisted on the
`WorkItem`, not overloaded onto:

- turn settlement
- internal task termination
- raw ingress exhaustion

Future RFCs or follow-up issues can define:

- what evidence is required to complete a work item
- when a work item should remain open but blocked instead of completing
- how closure claims and evidence artifacts should interact

## Work-item action semantics

Work-item scheduling should be expressed through tool actions, not by asking the
agent to directly mutate a status machine.

The proposed model is:

1. agent action
2. minimal runtime fact check
3. persisted work-item or focus update

### Agent actions

The useful first-version actions are:

- create a work item
- pick the current work item
- update a work item's checklist or blocker
- complete a work item

The agent should not have to produce progress prose just to keep the work item
fresh. Normal progress can live in transcript text, brief records, tool records,
and final answers, all associated to the current work item by runtime binding.

This keeps the semantic interpretation close to the model, rather than trying to
fully hard-code it in the runtime, while avoiding ambiguous fields such as
`summary` and `progress_note`.

### Minimal runtime fact check

Runtime fact checks should remain intentionally small.

The runtime should only guard against obvious contradictions, such as:

- picking a work item that does not belong to the agent
- picking a work item that is already done
- completing a work item while there is still clearly unfinished execution
- setting an empty blocker

The runtime should not attempt full semantic judgment about whether the delivery target is truly satisfied.

Its role is only to prevent obviously inconsistent state transitions.

### Commit

The runtime should commit the persisted state or focus update.

This layer combines:

- the agent's explicit action
- the minimal runtime facts

and writes the resulting work-item record or `current_work_item_id`.

### Default bias

The initial default bias should be conservative:

- if the agent does not pick a different work item, keep the current work item
- blocked state should require `blocked_by`
- completion should require that runtime facts do not show obvious unfinished execution

This keeps first-version behavior simple while avoiding the most obvious forms of false completion.

## Non-goals

This RFC does not attempt to:

- define full evidence or verification policy
- introduce a separate resolver agent
- model the full external issue backlog inside the runtime
- define every future lifecycle field or scheduling policy
- redesign the user-facing UI in this document

## Open questions

The main open questions after this RFC are:

1. whether `blocked_by` is enough for the first blocked-work model, or whether
   a later `resume_hint` field is needed
2. how much delegation state should be injected into prompt projection
3. how much work-item state should be injected into prompt projection
4. how completion should interact with closure evidence

## Current design direction

This RFC currently assumes:

- work-item boundaries are determined by delivery target
- newly discovered work that still serves the same delivery target should extend the current `Work Plan`
- newly discovered work that forms a different delivery target may become a new `Work Item`
- the first rollout remains message-driven by default
- `WorkItem` creation is explicit in early phases rather than inferred from every ingress
- `current_work_item_id` is controlled by explicit agent action
- queued and blocked views are derived from `open`, `current_work_item_id`, and `blocked_by`
- progress narration remains in transcript/brief/tool records and is associated
  back to work items by runtime binding
- child-agent delegation is expressed through `SpawnAgent` work-item metadata
  and delegation records, not `WorkItem.parent_id`
- a direct control-plane path is allowed for future open work

This RFC does not yet impose stronger policy constraints on agent-derived work creation.

The intention is to observe real runtime behavior first before introducing tighter restrictions.
