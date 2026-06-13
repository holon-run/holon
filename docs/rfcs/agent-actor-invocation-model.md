---
title: RFC: Agent Actor And Invocation Model
date: 2026-06-13
status: draft
---

# RFC: Agent Actor And Invocation Model

## Summary

Holon should model every agent as an actor-like, long-lived execution
container. Public root agents, named agents, private child agents, and future
collaborative agents should all be profiles of the same `Agent` object, not
separate runtime species.

The external interoperability model should be:

```text
Agent = addressable actor / execution container
Message = admitted input to an agent
Invocation = externally observable unit of work created from admitted input
InvocationHandle = stable handle for progress, follow-up, cancellation, result
Run = internal scheduler attempt that advances one or more invocations
RunResult = result of one internal execution attempt
InvocationResult = result of the externally visible work
```

The key boundary is:

- external systems hold an `InvocationHandle`;
- the runtime may use one or more internal runs to advance that invocation;
- the agent remains alive after any run finishes unless its own lifecycle is
  explicitly changed.

This keeps the actor model simple for callers while preserving Holon's need for
long-lived context, follow-up, waiting, retries, and parent supervision.

## Problem

Current child-agent delegation still exposes too much of the internal
supervision shape.

The parent can spawn a child agent and receive a task handle, but that handle
currently looks like the child work itself. When the child produces a terminal
task result, the parent has no natural way to say:

- the result is incomplete;
- continue from the same context;
- apply this follow-up to the same delegated work;
- keep the child alive but mark this specific work closed later.

This is not only a private-child problem. The same tension appears for every
agent style:

- public agents should be addressable without exposing how the scheduler runs
  them;
- named agents should accept work over time and keep context;
- child agents should be reusable across follow-ups while their delegated work
  remains open;
- task or run records should not become the public identity of the work.

Holon therefore needs a model where the external handle represents "the work I
started with an agent," not "the runtime run that happened to process it."

## Goals

- define one actor-like abstraction for all agent profiles;
- make `Agent` the addressable context owner;
- make `Invocation` the external unit of observable work;
- create a stable handle immediately when agent work is admitted;
- allow follow-up, cancellation, progress observation, and result waiting
  through that handle;
- allow one invocation to span multiple internal runs;
- keep internal scheduler tasks/runs replaceable without changing the public
  interoperability model;
- make child-agent delegation a special policy profile, not a special object
  model.

## Non-goals

- do not require Holon to expose open-ended public multi-agent collaboration in
  the first implementation;
- do not remove existing task handles before migration paths exist;
- do not make every low-level runtime event an invocation;
- do not define a full permission system in this RFC;
- do not require streaming transport support before the handle model exists.

## Core Model

## 1. Agent

An `Agent` is the actor.

It owns:

- identity and profile;
- addressability policy;
- mailbox / input queue;
- context and memory;
- workspace projection;
- active work focus;
- lifecycle state;
- visible lineage and supervision relationships.

The runtime should not define separate core object types for public agents,
child agents, named agents, or delegated agents. Those are profiles and policy
combinations over the same object:

- visibility: public or private;
- ownership: self-owned or parent-supervised;
- context derivation: fresh or forked;
- workspace projection: inherited, bound, or isolated;
- ingress policy: operator, parent, runtime, external, or some combination.

From a multi-agent scheduling perspective, they are all actors with different
admission and lifecycle policies.

## 2. Message

A `Message` is an admitted input to an agent mailbox.

Messages preserve:

- origin;
- trust;
- priority;
- causation and correlation ids;
- sender identity when available;
- target agent id;
- optional target invocation id.

Not every runtime wake or internal bookkeeping event needs to become a public
invocation. But every external request that asks an agent to do meaningful work
should be admitted as a message associated with an invocation.

## 3. Invocation

An `Invocation` is the public unit of work.

It answers:

- what did a caller ask this agent to do?
- what progress has been made?
- is more input needed?
- has the caller accepted or closed the work?
- what final result should be observed by the caller?

An invocation is created at message admission time, before the scheduler
decides which run will process the message.

This means callers do not need to wait for an internal `Run` or `Task` handle
before they can wait on, cancel, or follow up on the work.

## 4. InvocationHandle

An `InvocationHandle` is the stable external handle.

It should support:

- observe status and progress;
- wait for result;
- send follow-up or correction;
- cancel the work;
- inspect summarized events;
- relate child results back to parent work;
- optionally accept or close the invocation when the caller owns completion.

The handle points to the invocation, not to an internal run.

Example shape:

```json
{
  "invocation_id": "inv_xxx",
  "agent_id": "agent_xxx",
  "status": "queued",
  "waitable": true
}
```

## 5. Run

A `Run` is the internal scheduler activation that advances agent state.

A run may:

- process one message;
- batch several messages;
- advance one invocation;
- advance several correlated invocations;
- resume after waiting;
- retry after failure;
- stop early because it produced an interim result.

Runs are operational records. They are useful for debugging, scheduling,
provider execution, cancellation of the current attempt, and accounting. They
should not be the primary external unit of work.

## 6. Results

Holon should distinguish:

- `RunResult`: what happened during one internal execution attempt;
- `InvocationResult`: the result of the external work represented by the
  invocation handle.

For simple one-shot work these may be produced at the same time. For delegated
or reviewed work they may differ:

```text
inv_123
  run_1 -> child reports draft result
  parent sends follow-up on inv_123
  run_2 -> child fixes missing parts
  parent accepts inv_123
  invocation_result -> completed
```

The agent remains alive throughout this sequence.

## Public API Direction

## Start Work On An Agent

The generic semantic operation is:

```text
InvokeAgent(agent_id, message, options) -> InvocationHandle
```

`InvokeAgent` means:

1. admit the message to the target agent mailbox;
2. create or attach an invocation;
3. ensure the target agent is scheduled;
4. return the invocation handle immediately.

The runtime may schedule a run immediately, later, or not at all if policy
rejects the message. That scheduling detail does not change the returned
external handle.

The first implementation does not need this exact tool name. Existing surfaces
can map to the same model:

- operator prompt to a public agent creates an invocation;
- parent follow-up to a child creates or updates an invocation;
- external callback work creates an invocation when it asks the agent to do
  meaningful work;
- runtime wake hints usually do not create a user-visible invocation.

## SpawnAgent

`SpawnAgent` creates an agent actor. If it also includes an initial work
message, it should return both:

```json
{
  "agent": {
    "agent_id": "agent_child"
  },
  "invocation": {
    "invocation_id": "inv_initial",
    "status": "queued",
    "waitable": true
  }
}
```

The important point is that `SpawnAgent` should not make the initial run the
identity of the child or of the delegated work.

For migration, `SpawnAgent(private_child, initial_message=...)` may still return
a task or supervision handle. That handle is a compatibility projection over
the run/supervision plane. The durable semantic handle should be the
invocation.

## Follow-Up

Follow-up should target the invocation, not the latest run:

```text
SendInvocationFollowup(invocation_id, message) -> InvocationHandle
```

This means:

- the child or target agent can continue with preserved context;
- the invocation remains the same externally visible work item;
- the runtime may create a new run, append input to a pending run, or queue the
  follow-up for later depending on scheduler state.

The caller should not need to know which internal run will consume the
follow-up.

## Observation And Waiting

Waiting should be possible on invocation result:

```text
WaitFor(wake=invocation_result, resource=invocation_id)
```

Observation should support at least:

- invocation status;
- current target agent;
- latest summary or progress item;
- open follow-up requirements;
- latest run id when useful for debugging;
- terminal result when closed.

Run/task observation may still exist, but it should be secondary and
operational.

## Completion Policy

An invocation needs an explicit completion policy.

Suggested first-pass policies:

- `auto_on_agent_result`: close when the agent produces a terminal result;
- `caller_acceptance`: keep open until the caller accepts, follows up, cancels,
  or delegates closure to the runtime;
- `manual`: only an explicit control action closes the invocation.

Simple public prompts can default to `auto_on_agent_result`.

Parent-supervised child work should usually default to `caller_acceptance`,
because the parent may need to review the child result and request fixes.

## Agent Lifecycle

Agent lifecycle is separate from invocation lifecycle.

An agent may be:

- active;
- idle;
- waiting;
- paused;
- archived;
- deleted or garbage-collected.

An invocation may be:

- queued;
- running;
- waiting;
- needs input;
- produced result;
- accepted;
- completed;
- failed;
- cancelled.

Completing an invocation does not automatically archive the agent. Archiving an
agent should require either explicit lifecycle policy or cleanup rules.

For private parent-supervised agents, cleanup can still be automatic after all
supervised invocations are terminal and no policy keeps the child alive.

## Relationship To Work Items

`Invocation` and `WorkItem` are related but not identical.

- `WorkItem` is durable goal identity inside an agent's work plane.
- `Invocation` is the external request/response handle for work sent to an
  agent.

An invocation may create, select, or update a work item inside the target
agent. A work item may span multiple invocations over time.

The runtime should preserve explicit links instead of merging the concepts:

```text
Invocation -> target_agent_id
Invocation -> optional work_item_id
WorkItem -> current invocation ids / originating invocation id
```

This keeps operator-facing planning state separate from interoperability and
request tracking.

## Relationship To Tasks

`Task` should continue narrowing toward managed operational execution.

Under this RFC:

- command execution may still produce task handles;
- provider/tool runs may be represented internally as run/task records;
- supervised child execution may keep a compatibility task handle during
  migration;
- external callers should prefer invocation handles for agent work.

The long-term public distinction is:

```text
InvocationHandle = semantic handle for agent work
TaskHandle = operational handle for managed execution
```

A task may advance an invocation, but a task is not the invocation.

## Scheduling Semantics

The scheduler should treat messages and invocations as durable inputs.

At a high level:

```text
InvokeAgent
  -> MessageQueued
  -> InvocationCreated
  -> AgentScheduled
  -> RunStarted
  -> ProgressRecorded*
  -> RunResultRecorded
  -> InvocationUpdated
  -> maybe InvocationResultRecorded
```

If an agent is already running when a new invocation arrives, the runtime may:

- append the message to the mailbox for the next run;
- allow the current run to observe it at an interrupt/safe point;
- batch it into a later run.

The caller sees a stable invocation handle either way.

## Parent And Child Interoperability

Parent-child interaction should be ordinary agent-to-agent messaging with
policy constraints.

The parent does not need a special "child task" ontology. It needs:

- a target agent handle;
- an invocation handle for the delegated work;
- permission to send follow-up and observe result;
- optional operational access to current runs when supervision requires it.

Private child agents are therefore just agents whose ingress policy restricts
who can invoke them and who owns cleanup.

## Migration Direction

## Phase 1: Document The Boundary

- keep current `SpawnAgent` and task supervision behavior;
- document that child task handles are operational compatibility handles;
- introduce invocation terminology in RFCs and prompt guidance;
- make receipts distinguish `agent_id`, `invocation_id`, and `task_id` where
  all three exist.

## Phase 2: Add Invocation Records

- create invocation records for operator prompts and `SpawnAgent` initial
  messages;
- record links to target agent, originating message, work item, and runs;
- expose compact invocation status;
- allow `WaitFor` to wait on invocation completion.

## Phase 3: Move Follow-Up To Invocation

- route parent follow-up through invocation id;
- keep `TaskInput` as a compatibility path for supervised child tasks;
- make follow-up create a new message linked to the same invocation;
- keep the child agent alive while the invocation is open unless policy says
  otherwise.

## Phase 4: Narrow Task Semantics

- keep task handles for command execution and low-level operational
  supervision;
- remove the assumption that a terminal child task result means delegated work
  is finally accepted;
- eventually make agent work APIs return invocation handles as the primary
  waitable object.

## Open Questions

- Should the first public name be `InvokeAgent`, `SendAgentMessage`, or
  `StartAgentWork`?
- Should every operator prompt be an invocation, or only prompts that create
  durable work/result tracking?
- How much invocation progress should be stored as structured events versus
  summarized projection?
- Should `caller_acceptance` be available to public operators as a first-class
  mode, or only to parent-supervised delegation initially?
- How should invocation handles be authorized across public agents owned by
  different principals?

## Summary

Holon should use an actor-inspired model:

- all agents are addressable execution containers;
- messages enter agent mailboxes;
- meaningful external work creates an invocation immediately;
- callers hold invocation handles for progress, follow-up, cancellation, and
  result;
- internal runs/tasks are scheduler attempts, not the public identity of agent
  work.

This gives Holon one model for public agents, named agents, and child agents
while preserving the flexibility to retry, review, follow up, and keep agents
alive beyond any single run.
