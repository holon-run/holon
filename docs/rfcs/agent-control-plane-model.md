---
title: RFC: Agent Control Plane Model
date: 2026-04-21
status: draft
---

# RFC: Agent Control Plane Model

## Summary

This RFC defines Holon's top-level runtime and control-plane model.

The central direction is:

- `Agent` is the primary runtime object
- `WorkItem`, `Task`, and `Waiting` are distinct control-plane objects with
  narrower responsibilities
- creating another execution context belongs to the agent plane, not the task
  plane
- workspace or worktree isolation is an execution projection property, not a
  separate first-class noun
- task-created worktrees are supervised artifacts owned by task lifecycle, not
  by child agent identity

This RFC is the parent model for related plane-specific RFCs:

- delegation belongs to the agent plane
- managed command execution belongs to the task plane
- blocked and resumable state belongs to the waiting plane

## Problem

Holon's current model still carries overlapping concepts for context creation
and execution control.

Today the system has several adjacent ideas:

- public session creation
- internal `child_agent_task` supervision
- historical `subagent_task` and `worktree_subagent_task` migration records
- command-backed background tasks
- sleep, callback, and wake mechanisms

These capabilities are all useful, but they do not belong to the same layer.

The result is conceptual overload:

- `Task` is asked to represent command execution, child context creation, and
  waiting-like behavior
- `session` and `subagent` both describe new execution contexts
- worktree isolation is partly expressed as a task kind rather than as a
  property of execution context
- worktree artifact lifecycle must stay separated from child-agent execution
  state
- prompt and tool guidance have to explain several overlapping abstractions at
  once

Because Holon has not yet shipped a stable public contract, this is the right
time to simplify the model directly rather than preserve historical surface
shapes.

## Core Judgment

Holon should unify around `Agent` as the primary context-owning runtime
primitive.

Everything else should become narrower:

- `WorkItem` expresses what meaningful work exists
- `Task` expresses managed execution, primarily command-backed execution
- `Waiting` expresses why work is blocked and what may reactivate it

The important long-term distinction is not `session` versus `subagent`.

The important distinctions are:

- root versus child
- public versus private
- parent-supervised versus self-owned
- fresh versus forked context derivation
- inherited versus isolated workspace projection

## Canonical Runtime Objects

Holon should standardize on four primary control-plane objects.

## 1. `Agent`

`Agent` is the only object that owns an execution context.

An agent owns:

- queue
- lifecycle state
- prompt context and local summaries
- brief or user-facing history
- workspace projection
- ingress policy
- current work focus

This means:

- one agent owns one context
- current public sessions are root self-owned agents
- delegated subagents are child agents with bounded lifecycle modes

Holon should keep one runtime object here.

It should not create separate runtime types for:

- session agents
- child agents
- long-lived named agents
- future collaborative agents

Those should be different profiles of the same `Agent` object.

## 2. `WorkItem`

`WorkItem` is the unit of meaningful work identity.

A work item owns:

- title or objective statement
- plan and progress state
- acceptance or boundary notes
- blocked, active, or done status

A work item does not own:

- prompt context
- shell execution state
- waiting capability lifecycle

Its job is to anchor what the agent is trying to accomplish.

## 3. `Task`

`Task` is the unit of managed execution.

Its primary long-term use is:

- command-backed execution
- supervised execution inspection
- output retrieval
- stop and continuation control

A task does not own an independent reasoning context.

That is why delegated child work should not remain a task-shaped public
creation concept.

However, a task may still serve as a supervision handle for a child agent when
the runtime intentionally returns one.

## 4. `Waiting`

`Waiting` is the unit of blocked or resumable state.

It answers:

- what is this agent waiting on?
- which work item is blocked?
- what future condition should reactivate execution?

Waiting does not own prompt context and should not be treated as a task
substitute.

## Ownership Rules

The runtime should preserve these ownership boundaries strictly.

- only `Agent` owns context
- only `WorkItem` owns work identity
- only `Task` owns managed execution lifecycle
- only `Waiting` owns blocked or resumable state

These rules matter because they prevent several common sources of confusion:

- a shell command pretending to be a delegated agent
- a waiting record pretending to be a background task
- a work item pretending to be a runtime scheduler

## Agent Profile Model

Holon should use one `Agent` definition with profile-based variation.

In other words:

- child agent is not a separate runtime type
- long-lived private child is not a separate runtime type
- public named agent is not a separate runtime type

They are different combinations of agent profile fields.

The detailed first-pass profile contract now lives in:

- `agent-profile-model.md`

This control-plane RFC keeps the higher-level rule:

- the runtime should have one `Agent` object model
- public tools may expose a smaller set of stable spawn modes that map onto a
  richer internal agent profile

## Control Planes

The public control surface should be split into four planes.

## 1. Agent Plane

The agent plane owns context creation and agent lifecycle.

Its responsibilities are:

- create another execution context
- define parent-child relationships
- define public or private visibility
- define supervision and cleanup ownership
- define result routing and addressability

The central expansion primitive should be:

- `SpawnAgent`

This should be the only public way to create another reasoning context.

The central inspection primitive should be:

- `AgentGet`
  - Returns an `AgentGetResult` envelope carrying the current `AgentSummary`.

`AgentGet` should read agent-plane state for the context-owning agent:

- identity and visibility
- lifecycle / closure posture
- active work focus and waiting state
- visible child-agent lineage where policy allows

`AgentGet` is distinct from task-plane inspection. `TaskStatus` should inspect a
managed execution handle, while `AgentGet` should inspect the agent that owns
the broader context.

For bounded parent-supervised delegation, `SpawnAgent` should always return the
new `agent_id` and return a task-plane handle when the parent keeps a
supervising execution record for that child.

Under this model:

- "start a new session" becomes root-agent creation
- "delegate bounded work" becomes child-agent creation

## 2. Work Plane

The work plane owns work identity and plan continuity.

Its responsibilities are:

- create and update work items
- mark active, blocked, or done work
- keep plan and boundary state attached to the right work item

The work plane should answer:

- what is the agent currently trying to achieve?
- what remains open?
- what is blocked versus active?

## 3. Task Plane

The task plane owns managed execution.

Its responsibilities are:

- start command execution
- inspect running execution
- accept later input for interactive execution
- read output
- stop execution
- later continue interactive execution

The task plane should narrow toward command-backed execution as its center of
gravity.

This means:

- `ExecCommand` is the startup primitive
- `TaskList`, `TaskStatus`, `TaskOutput`, `TaskStop`, and later `TaskInput`
  inspect and control managed execution state
- delegated child context creation should move out of task language

The task plane may still supervise other managed execution handles when they
share the same operational semantics.

The clearest non-command example is:

- a bounded child agent spawned through `SpawnAgent` and returned to the parent
  as a supervision handle

## 4. Waiting Plane

The waiting plane owns blocked work and future reactivation.

Its responsibilities are:

- create waiting intents
- represent timer-backed and callback-backed waiting
- cancel obsolete waits
- reactivate the right work when future conditions arrive

Waiting should usually be anchored to a work item, not left as detached
runtime residue.

## Lifecycle Axes

Holon should describe agent lifecycle through explicit orthogonal axes rather
than overloaded product nouns.

## Root versus child

- root agents are primary entry points
- child agents are spawned from another agent and preserve provenance

## Public versus private

- public agents may receive external ingress
- private agents only receive parent-directed work or runtime-controlled input

## Parent-supervised versus self-owned

- parent-supervised agents remain inside a parent-owned supervision contract
- self-owned agents expose their own operator-facing lifecycle surface

This distinction is separate from visibility in the general model, even though
the first-pass presets intentionally couple:

- `private_child` with parent supervision
- `public_named` with self-owned lifecycle

Restart behavior should follow supervision semantics, not a separate lifetime
label.

That means:

- a supervised child must remain restart-safe while its supervision handle is
  still live
- child cleanup should happen when the supervision contract reaches terminal
  state or the parent runtime explicitly cleans up the child
- daemon restart may interrupt execution, but it must not silently erase a live
  supervised child handle

## Fresh versus forked

- fresh agents start from explicit handoff only
- forked agents start from an inherited parent context view

This distinction should be modeled explicitly.

It is an execution-context derivation choice, not a separate runtime type.

These axes are more stable and more expressive than preserving `session`
versus `subagent` as parallel primary abstractions.

## Fork Context Derivation

`fork` should not mean "copy the parent's raw transcript."

It should mean:

- the runtime derives a bounded inherited context view
- the parent agent may provide explicit handoff
- the child starts from the combination of both

The default contract should be:

- `fork` = runtime-derived context + handoff
- `fresh` = no inherited parent conversation view + handoff

If a parent wants to fully redefine the child's starting context, it should use
`fresh` rather than trying to override `fork`.

### Runtime responsibility

The runtime should own the base derivation logic for `fork`.

This is important because the runtime has the structured state needed to make a
stable cut:

- active work item
- active work plan
- waiting and callback state
- recent turn memory
- durable working memory
- finalized episode memory

The runtime should therefore build the inherited child context from structured
runtime state first, not from naive transcript replay.

### Parent-agent responsibility

The parent agent should still participate, but through explicit handoff rather
than through full manual context reconstruction.

Handoff is where the parent expresses:

- what the child is being asked to do
- what constraints matter most
- what output form or return contract is expected
- which details deserve extra emphasis beyond the default inherited context

This gives the parent useful steering power without making `fork` semantics
unpredictable.

### Default fork anchor: active work item

When a parent has an active work item, `fork` should anchor inheritance on that
active work item by default.

This means the child should usually inherit:

- the full current snapshot of the active work item
- the full current snapshot of the active work plan
- waiting or callback state attached to that active work item
- the small hot-turn tail most relevant to that active work item
- finalized episode summaries associated with that active work item when needed

This does **not** mean the runtime should blindly copy every historical turn
tagged to that work item.

The default inherited view should remain a bounded projection centered on the
active work item, not a work-item-filtered transcript dump.

### Non-active work should stay out by default

History and state from non-active work items should normally stay out of the
forked child context.

They should only enter when one of the following is true:

- the handoff explicitly references them
- they contain constraints or decisions the child must obey
- they carry unresolved dependencies that still block the active work item
- they share the same delivery target or parent work chain in a way the runtime
  can identify structurally

This keeps delegated children focused and reduces contamination from unrelated
long-session history.

### Cache and stability implications

This rule also improves prompt stability.

The inherited fork prefix should be composed mostly of structured durable
projections that do not change every turn, while:

- hot tail remains small and volatile
- handoff remains explicit and task-shaped

That is better than rebuilding a large transcript-shaped blob on every fork,
and it leaves room for prompt-prefix cache reuse.

## Provenance Versus Supervision

Holon should not overload one `parent_agent_id` field with every possible
meaning.

There are at least two different relationships:

- provenance: which agent created or derived this one?
- supervision: which agent currently owns control, result routing, or cleanup?

These should be modeled separately.

Useful distinctions include:

- `lineage_parent_agent_id` for origin and audit
- `supervisor_agent_id` for current control responsibility

This matters most once an agent becomes long-lived or externally reachable.

A public named agent may still preserve lineage provenance, while no longer
remaining under strong parent supervision.

## Workspace Projection Model

Workspace or worktree isolation should not be modeled as a separate control
plane object.

It should be modeled as an execution projection property owned by the agent,
and consumed by execution started under that agent.

Useful projection modes include:

- `inherit`
- `worktree`
- `explicit_root`

This means:

- a worktree-isolated delegated run is a child agent with
  `workspace_mode=worktree`
- a task usually executes within its owning agent's workspace projection
- task tools should not become the long-term home for workspace-context
  creation semantics
- a task-created worktree is artifact state owned by the supervising task
  lifecycle
- a child agent may hold that worktree as its active execution projection while
  running, but the child agent should not be the artifact owner

This keeps workspace isolation explicit without creating another overlapping
kind such as "workspace task."

The historical `worktree_subagent_task` shape has converged into a unified
supervised `child_agent_task` handle with `workspace_mode=worktree` metadata.
Cleanup is driven by task cleanup or future artifact GC rather than by
model-facing workspace switching.

## Routing And Result Rules

Message and result routing must stay explicit.

### External ingress

Direct operator ingress should target a public self-owned agent unless an
explicit future policy says otherwise.

Private child agents should not receive direct operator ingress.

### Delegated child results

By default:

- private child agents report status and results to the parent
- child-local reasoning history stays local unless summarized back explicitly

Parent agents remain responsible for synthesis, prioritization, and final
user-facing reasoning.

Child results should be treated as bounded outputs or observations, not as a
transfer of overall understanding.

## Spawn Result Contract

`SpawnAgent` should always return the created `agent_id`.

In the first stable model, private child creation should also return
`task_handle`.

The important rule is:

- `agent_id` identifies the context-owning runtime object
- `task_handle` is a structured `TaskHandle` execution receipt that identifies
  the parent's managed supervision handle

This keeps the control planes distinct:

- the agent plane creates and identifies the child context
- the task plane may supervise that child when the runtime chooses to expose a
  bounded handle

`TaskHandle` is shared by any operation that produces asynchronous execution
state:

```rust
pub struct TaskHandle {
    pub task_id: String,
    pub task_kind: String,
    pub status: TaskStatus,
    pub initial_output: Option<String>,
}
```

The handle is not an independently creatable control-plane object. It is an
execution receipt returned as a side effect of an operation verb such as
`SpawnAgent` or `ExecCommand`.

### When a task handle should exist

A `task_handle` is appropriate when the spawned child is:

- private
- bounded
- parent-supervised
- still part of the parent's managed lifecycle

This is the most natural shape for delegated work that still behaves like a
managed execution unit from the parent's perspective.

In the first-pass profile model, this is not just a common case. It is the
default rule:

- private spawned agents return `task_handle`

### When a task handle should not exist

A `task_handle` should normally be absent when the spawned agent is:

- public
- operator-addressable
- not under strong parent supervision
- intended to remain open as its own long-lived runtime object

In those cases, the runtime should return only `agent_id` and rely on
agent-plane addressing rather than a parent-scoped supervision handle.

In the first-pass profile model, this means:

- public named agents return `agent_id` without `task_handle`

### Task-handle capabilities for supervised children

When a child agent is returned with a `task_handle`, the parent should use the
task plane for bounded supervision:

- `TaskStatus` for handle-level lifecycle state using `task_handle.task_id`
- `TaskInput` for supervisor follow-up input
- `TaskOutput` for progress and result output
- `TaskStop` for cancellation or termination of the supervised execution

This should not replace future mailbox-style agent communication.

It only defines the bounded supervision path for parent-controlled child work.

### Relationship to `AgentGet`

`AgentGet` and task-handle inspection should answer different questions.

- `AgentGet` should inspect the agent as a context-owning runtime object
- `TaskStatus` should inspect the parent-visible supervision handle

For example:

- `AgentGet` may expose profile, visibility, work focus, and waiting state
- `TaskStatus` should stay focused on lifecycle and control metadata for the
  managed execution handle

This avoids collapsing agent identity and task supervision into one overloaded
detail surface.

### Long-lived child agents

If Holon later introduces long-lived named child agents, they should be treated
as addressable agents, not as upgraded tasks.

If such an agent becomes public or externally reachable, Holon should weaken or
remove strong parent-supervision semantics by default while preserving lineage
metadata.

## Default Child Boundary

The initial Holon model should optimize for bounded private child agents, not
for open-ended multi-agent collaboration.

The default bounded-child contract should therefore be:

- direct operator ingress is not allowed
- child-to-child direct messaging is not part of the initial model
- recursive child spawning is disabled by default
- shared persistent memory writes are disabled by default
- external side effects remain governed by runtime policy and profile
  capability boundaries

This keeps delegation useful without prematurely turning bounded child work
into a public team model.

## Bounded Children Versus Collaborative Agents

Holon should distinguish between:

- bounded child agents
- long-lived private child agents
- public collaborative agents

These should still be profiles of one `Agent` object, not different runtime
types.

The important distinction is not the noun. The important distinction is the
profile.

A useful first-phase mental model is:

- bounded private child: private, parent-only ingress, rejoin-parent
- long-lived private child: private, machine-only or parent-only ingress,
  restart-safe while supervised, still belongs to the parent's supervision tree
- public named agent: public ingress, self-owned lifecycle, lineage preserved
  for audit only

Holon does not need public collaborative agents to validate the core agent
plane model.

The first stable milestone should optimize for bounded private child agents,
with long-lived private children added only when delegated objectives
truly require them.

## Tool Surface Direction

The intended public direction is:

- agent plane: create and manage contexts
- work plane: express meaningful work identity
- task plane: manage command execution
- waiting plane: manage blocked and resumable state

This RFC therefore sets the parent model for these related RFCs:

- [Agent Delegation Tool Plane](./agent-delegation-tool-plane.md)
- [Task Surface Narrowing](./task-surface-narrowing.md)
- [Command Tool Family](./command-tool-family.md)
- [Waiting Plane And Reactivation](./waiting-plane-and-reactivation.md)

Those RFCs should stay narrower than this one.

They refine one plane each.

This RFC defines the shared object model and plane boundaries underneath them.

## Simplification Decision

Because Holon has not yet shipped a stable public API, the preferred direction
is direct simplification rather than compatibility layering.

That means the preferred direction is not:

- keep `session`, `subagent`, and workspace-task concepts as parallel primary
  nouns

It is:

- make `Agent` the primary runtime primitive
- treat root sessions as the product-facing form of root agents
- treat delegated subagents as child-agent lifecycle modes
- treat worktree isolation as a projection mode
- keep `Task` focused on managed execution

## Open Questions

The following questions remain open after this RFC:

- should Holon expose long-lived named child agents early, or keep the public
  agent plane bounded-only for longer?
- should worktree-isolated child creation become a parameter on `SpawnAgent`
  or a specialized convenience tool built on the same model?
- should long-lived private child agents be exposed early as an explicit spawn
  profile for workflows such as PR shepherding, CI follow-up, or webhook-driven
  continuation?
- how much waiting state should appear by default in prompt context versus
  on-demand inspection?
- should the product continue using the word `session` as a user-facing term
  even after the runtime fully migrates to `Agent`?

## Summary

Holon should adopt one top-level control-plane model:

- `Agent` owns context
- `WorkItem` owns work identity
- `Task` owns managed execution
- `Waiting` owns blocked and resumable state

From that model:

- delegation belongs to the agent plane
- command execution belongs to the task plane
- waiting belongs to the waiting plane
- workspace or worktree isolation is a projection property, not a separate
  runtime noun

This gives Holon a clearer foundation for context isolation, delegation,
worktree-backed execution, and future multi-agent growth.
