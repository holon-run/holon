---
title: RFC: Agent Actor Handle And Task Invocation Model
date: 2026-06-13
status: draft
---

# RFC: Agent Actor Handle And Task Invocation Model

## Summary

Holon should model every agent as an actor-like, long-lived execution
container. Public root agents, named agents, private child agents, and future
collaborative agents should all be profiles of the same `Agent` object, not
separate runtime species.

The interoperability model should be:

```text
AgentId / ActorId
  = stable identity of the actor

AgentHandle / ActorRef
  = callable reference / capability for sending work to the actor

Message
  = admitted input sent to an actor

TaskHandle
  = waitable asynchronous result of a concrete call or execution

TaskKind::ActorInvocation
  = a task created by sending a message to an actor

Run / Continuation
  = internal execution fragment that advances a task
```

This deliberately avoids introducing a second public waitable handle named
`InvocationHandle`. If sending work to an actor is an asynchronous operation,
then the result of that send is already a task. Actor invocation, shell command
execution, workflow execution, and compatibility child-agent supervision should
all be represented as different task kinds under one wait and observation
system.

The key boundary is:

- callers hold an `AgentHandle` / `ActorRef` when they are allowed to invoke an
  agent;
- each accepted actor call returns a `TaskHandle`;
- `wait_for task_result <task_id>` is the common wait path for actor calls,
  commands, workflows, and child-agent compatibility tasks;
- the runtime may use one or more internal runs and continuations to advance the
  task;
- the agent remains alive after any task or run finishes unless its own
  lifecycle is explicitly changed.

This keeps the actor model simple for callers while preserving Holon's need for
long-lived context, follow-up, waiting, retries, self-scheduling, and parent
supervision.

## Problem

Current child-agent delegation exposes too much of the internal supervision
shape, but adding a separate `InvocationHandle` risks creating another handle
that overlaps with `TaskHandle`.

The parent can spawn a child agent and receive a task handle. That handle
currently looks like both:

- the operational supervision task for the child run; and
- the semantic delegated work requested from the child.

This creates confusion when the parent wants to say:

- the result is incomplete;
- continue from the same context;
- apply this follow-up to the same delegated work;
- keep the child alive but mark this specific call closed later.

The same tension appears beyond private children:

- public agents should be addressable without exposing how the scheduler runs
  them;
- named agents should accept work over time and keep context;
- dynamic workflows need to call agents and wait on those calls using the same
  primitive used for commands and workflows;
- self-scheduling needs `wait_for` to suspend the current continuation, not
  merely block a shell process;
- task or run records should not become the identity of the agent itself.

Holon therefore needs an explicit callable actor reference, while keeping the
result of a call in the existing waitable task plane.

## Goals

- define one actor-like abstraction for all agent profiles;
- make `Agent` the long-lived context owner;
- make `AgentHandle` / `ActorRef` the callable entry point for agent work;
- make `TaskHandle` the common asynchronous handle returned by actor calls,
  commands, workflows, and child-agent compatibility operations;
- model "invoke an actor" as `TaskKind::ActorInvocation`, not as a parallel
  `InvocationHandle`;
- allow follow-up, cancellation, progress observation, and result waiting
  through task APIs;
- allow one actor-invocation task to span multiple internal runs;
- make `wait_for` attach to the current task / continuation and target the
  result of another task, external event, or operator input;
- make child-agent delegation a special policy profile, not a special object
  model.

## Non-goals

- do not require Holon to expose open-ended public multi-agent collaboration in
  the first implementation;
- do not remove existing task handles before migration paths exist;
- do not make every low-level runtime event an actor invocation;
- do not define a full permission system in this RFC;
- do not require streaming transport support before the actor-handle model
  exists;
- do not make shell scripts responsible for simulating runtime suspension or
  resume semantics.

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

## 2. AgentId And ActorRef

`AgentId` / `ActorId` is stable identity. It says which actor exists.

`AgentHandle` / `ActorRef` is the callable reference used to send work to that
actor. It may include:

- target agent id;
- routing or mailbox address;
- caller authority and visibility constraints;
- allowed operations;
- default trust / origin classification;
- optional parent-supervision relationship;
- revocation or expiry metadata for external capabilities.

Inside a trusted local runtime, knowing an `AgentId` may be enough to resolve an
actor. At API boundaries, however, Holon should prefer returning and accepting
an explicit actor handle instead of treating every bare id as a capability.

This distinction gives dynamic workflows a concrete invocation entry point:

```text
actor = get_actor(agent_id)
task = actor.send(message)
```

The handle identifies who may be called. The returned task identifies this
particular asynchronous call.

## 3. Message

A `Message` is an admitted input to an agent mailbox.

Messages preserve:

- origin;
- trust;
- priority;
- causation and correlation ids;
- sender identity when available;
- target agent id;
- optional target task id for follow-up;
- optional work item or conversation correlation.

Not every runtime wake or internal bookkeeping event needs to become a public
actor invocation. But every external request that asks an agent to do
meaningful work should be admitted as a message and normally create an
`ActorInvocation` task.

## 4. Task

A `Task` is the common waitable asynchronous work record.

It answers:

- what operation was started?
- who owns or may observe it?
- what status and progress have been recorded?
- what result or failure should be observed?
- can the task be cancelled, followed up, or resumed?

The task kind defines the operational shape:

```rust
enum TaskKind {
    ActorInvocation,
    Command,
    Workflow,
    ChildAgentCompat,
}
```

`TaskKind::ActorInvocation` is the semantic unit created by sending a message to
an actor. It replaces the need for a separate public `InvocationHandle`.

Example task handle shape:

```json
{
  "task_id": "task_xxx",
  "kind": "actor_invocation",
  "actor": {
    "agent_id": "agent_xxx"
  },
  "status": "queued",
  "waitable": true
}
```

For command execution, the same handle surface may point at a process task:

```json
{
  "task_id": "task_cmd_xxx",
  "kind": "command",
  "status": "running",
  "waitable": true
}
```

The public distinction is not "invocation handle versus task handle"; it is
"different task kinds under one task handle contract."

## 5. Actor Invocation Task

An actor invocation task is created when work is accepted for an actor.

It should record:

- task id;
- task kind: `ActorInvocation`;
- target agent id / actor ref;
- originating message id;
- caller identity and trust classification;
- optional parent task id;
- optional work item id selected or created by the target agent;
- completion policy;
- current status and progress projection;
- links to internal runs and continuations.

An actor invocation task is not the agent. The same agent may have many
concurrent or historical invocation tasks:

```text
Agent agent_a
  Task task_1: ActorInvocation("write a design note")
  Task task_2: ActorInvocation("review this draft")
  Task task_3: Command("npm run build:index")
```

The task may span multiple internal runs:

```text
task_actor_123
  run_1 -> child reports draft result
  parent sends follow-up to task_actor_123
  run_2 -> child fixes missing parts
  task_result -> completed
```

## 6. Run And Continuation

A `Run` is an internal scheduler activation that advances agent state.

A run may:

- process one message;
- batch several messages;
- advance one task;
- advance several correlated tasks;
- resume a suspended continuation;
- retry after failure;
- stop early because it produced an interim result.

A `Continuation` is the resumable execution point owned by the current task.
This is the unit that matters for dynamic workflow self-scheduling. When a
Shell VM builtin calls `wait_for`, the runtime should suspend the current
continuation and record the wait against the current task; it should not merely
block a child shell process.

Runs and continuations are operational records. They are useful for debugging,
scheduling, provider execution, cancellation of the current attempt, and
accounting. They should not be the primary external identity of either the
agent or the actor call.

## Public API Direction

## Resolve Or Create An Actor

The generic actor lookup operation is:

```text
GetActor(agent_id | agent_selector, options) -> ActorRef
```

For trusted in-runtime calls, this may be implicit: a tool can accept
`agent_id` and resolve the actor internally. For external or cross-principal
calls, returning an explicit `ActorRef` makes capability and routing boundaries
visible.

`SpawnAgent` creates a new actor and returns an actor reference:

```json
{
  "actor": {
    "agent_id": "agent_child",
    "handle_id": "actor_ref_xxx"
  }
}
```

If `SpawnAgent` also includes an initial work message, it should return both
the actor reference and the task created by sending that message:

```json
{
  "actor": {
    "agent_id": "agent_child",
    "handle_id": "actor_ref_xxx"
  },
  "task": {
    "task_id": "task_initial",
    "kind": "actor_invocation",
    "status": "queued",
    "waitable": true
  }
}
```

The important point is that `SpawnAgent` should not make the initial run the
identity of the child. The child identity is the actor; the delegated work is
the returned actor-invocation task.

For migration, `SpawnAgent(private_child, initial_message=...)` may still return
a parent-supervision task handle. That handle should be folded into, or clearly
linked to, the actor-invocation task instead of becoming a separate semantic
handle.

## Send Work To An Actor

The generic semantic operation is:

```text
ActorSend(actor_ref, message, options) -> TaskHandle
```

`ActorSend` means:

1. authorize use of the actor reference;
2. admit the message to the target agent mailbox;
3. create an `ActorInvocation` task;
4. ensure the target agent is scheduled;
5. return the task handle immediately.

The runtime may schedule a run immediately, later, or not at all if policy
rejects the message. That scheduling detail does not change the returned task
handle.

Existing surfaces can map to the same model:

- operator prompt to a public agent creates an actor-invocation task;
- parent follow-up to a child sends a message correlated with an existing task
  or starts a new actor-invocation task;
- external callback work creates an actor-invocation task when it asks the
  agent to do meaningful work;
- runtime wake hints usually resume an existing task / continuation and do not
  create a user-visible task by themselves.

## Follow-Up

Follow-up should target the actor-invocation task, not the latest run:

```text
TaskInput(task_id, message) -> TaskHandle
```

or, if the target actor needs to be explicit:

```text
ActorSend(actor_ref, message, { followup_for: task_id }) -> TaskHandle
```

This means:

- the child or target agent can continue with preserved context;
- the original actor-invocation task remains the externally visible work item
  when policy says the follow-up is part of the same call;
- the runtime may create a new run, append input to a pending run, or queue the
  follow-up for later depending on scheduler state.

The caller should not need to know which internal run will consume the
follow-up.

## Observation And Waiting

Waiting should use one wait system over task results and other target kinds:

```text
WaitFor(wake=task_result, resource=task_id)
WaitFor(wake=external, resource=external_ref)
WaitFor(wake=operator_input)
```

Internal representation can still preserve target kinds:

```rust
enum WaitTarget {
    TaskResult(TaskId),
    External(ExternalRef),
    OperatorInput,
}

struct WaitCondition {
    owner: ContinuationOwner,
    target: WaitTarget,
}
```

For Shell VM workflows, the critical rule is that `wait_for` is a runtime
builtin. It must:

1. identify the current task / continuation;
2. record the wait condition durably;
3. suspend that continuation;
4. return control to the Holon scheduler;
5. resume the continuation when the wake condition is satisfied.

If `wait_for` merely blocks inside a shell process, it is not self-scheduling;
it is only synchronous waiting.

Observation should support at least:

- task status and kind;
- current target actor;
- latest summary or progress item;
- open follow-up requirements;
- latest run id when useful for debugging;
- terminal result when closed.

Run observation may still exist, but it should be secondary and operational.

## Completion Policy

Actor-invocation tasks need an explicit completion policy.

Suggested first-pass policies:

- `auto_on_agent_result`: close when the agent produces a terminal result;
- `caller_acceptance`: keep open until the caller accepts, follows up, cancels,
  or delegates closure to the runtime;
- `manual`: only an explicit control action closes the task.

Simple public prompts can default to `auto_on_agent_result`.

Parent-supervised child work should usually default to `caller_acceptance`,
because the parent may need to review the child result and request fixes.

## Agent Lifecycle

Agent lifecycle is separate from task lifecycle.

An agent may be:

- active;
- idle;
- waiting;
- paused;
- archived;
- deleted or garbage-collected.

An actor-invocation task may be:

- queued;
- running;
- waiting;
- needs input;
- produced result;
- accepted;
- completed;
- failed;
- cancelled.

Completing a task does not automatically archive the agent. Archiving an agent
should require either explicit lifecycle policy or cleanup rules.

For private parent-supervised agents, cleanup can still be automatic after all
supervised tasks are terminal and no policy keeps the child alive.

## Relationship To Work Items

`TaskKind::ActorInvocation` and `WorkItem` are related but not identical.

- `WorkItem` is durable goal identity inside an agent's work plane.
- `ActorInvocation` is the asynchronous request/response task for work sent to
  an agent.

An actor-invocation task may create, select, or update a work item inside the
target agent. A work item may span multiple actor-invocation tasks over time.

The runtime should preserve explicit links instead of merging the concepts:

```text
Task(kind=ActorInvocation) -> target_agent_id
Task(kind=ActorInvocation) -> optional work_item_id
WorkItem -> current task ids / originating task id
```

This keeps operator-facing planning state separate from interoperability and
request tracking.

## Relationship To Commands And Workflows

Commands and workflows remain task kinds in the same waitable plane:

```text
TaskKind::Command
  = one process execution, such as ExecCommand

TaskKind::Workflow
  = Shell VM workflow execution that may call runtime builtins

TaskKind::ActorInvocation
  = actor call created by sending a message to an agent
```

`ExecCommand` can remain a small primitive for one command. A future Shell VM
workflow can be a richer task kind that mixes shell commands and Holon runtime
builtins. The difference is not that workflow can do everything `ExecCommand`
can do; the difference is that workflow builtins are visible to the runtime and
can suspend/resume the current continuation.

Long-term, `TaskHandle` should be the common handle for:

- command execution;
- workflow execution;
- actor invocation;
- compatibility child-agent supervision.

The task kind and capability policy decide which operations are available on a
given handle.

## Scheduling Semantics

The scheduler should treat messages and tasks as durable inputs.

At a high level:

```text
ActorSend
  -> MessageQueued
  -> TaskCreated(kind=ActorInvocation)
  -> AgentScheduled
  -> RunStarted
  -> ProgressRecorded*
  -> RunResultRecorded
  -> TaskUpdated
  -> maybe TaskResultRecorded
```

If an agent is already running when a new actor-invocation task arrives, the
runtime may:

- append the message to the mailbox for the next run;
- allow the current run to observe it at an interrupt/safe point;
- batch it into a later run.

The caller sees a stable task handle either way.

## Parent And Child Interoperability

Parent-child interaction should be ordinary actor messaging with policy
constraints.

The parent does not need a special "child task" ontology. It needs:

- a target actor handle;
- a task handle for the delegated actor call;
- permission to send follow-up and observe result;
- optional operational access to current runs when supervision requires it.

Private child agents are therefore just agents whose ingress policy restricts
who can invoke them and who owns cleanup.

## Migration Direction

## Phase 1: Document The Boundary

- keep current `SpawnAgent` and task supervision behavior;
- document that `agent_id` identifies the actor and task ids identify concrete
  async calls or executions;
- introduce `ActorRef` / `AgentHandle` terminology in RFCs and prompt guidance;
- stop describing `InvocationHandle` as a parallel public handle;
- make receipts distinguish actor identity from task identity.

## Phase 2: Normalize Task Kinds

- introduce explicit task kinds such as `ActorInvocation`, `Command`,
  `Workflow`, and `ChildAgentCompat`;
- ensure operator prompts and `SpawnAgent` initial messages are represented as
  actor-invocation tasks;
- record links to target agent, originating message, work item, and runs;
- expose compact task status across task kinds.

## Phase 3: Unify Waiting

- keep `WaitFor(wake=task_result, resource=task_id)` as the shared wait path;
- make actor invocation, command, workflow, and child-agent compatibility tasks
  all waitable through the same condition model;
- make Shell VM `wait_for` a runtime builtin that suspends the current
  continuation instead of blocking a shell process;
- preserve external and operator waits as target kinds in the same wait system.

## Phase 4: Move Follow-Up To Task Correlation

- route parent follow-up through the actor-invocation task id;
- keep `TaskInput` as a compatibility spelling where appropriate;
- make follow-up create a new message linked to the same task when completion
  policy allows;
- keep the child agent alive while relevant tasks are open unless policy says
  otherwise.

## Phase 5: Add Actor Handles

- expose explicit actor references where bare `agent_id` is not enough;
- include routing, policy, and capability metadata in actor handles;
- allow trusted local APIs to resolve an actor by id as a convenience;
- avoid making bare externally supplied ids equivalent to invocation authority.

## Open Questions

- Should the first public name be `ActorSend`, `InvokeAgent`, or
  `SendAgentMessage` if all return a task handle?
- How explicit should `ActorRef` be in trusted in-runtime tool calls, where
  `agent_id` may already be sufficient?
- Should follow-up mutate the original actor-invocation task, create a linked
  task, or choose based on completion policy?
- Which task operations are universal, and which are task-kind-specific?
- How much task progress should be stored as structured events versus
  summarized projection?
- Should `caller_acceptance` be available to public operators as a first-class
  mode, or only to parent-supervised delegation initially?
- How should actor handles be authorized across public agents owned by
  different principals?

## Summary

Holon should use an actor-inspired model:

- all agents are addressable execution containers;
- actor handles / refs are the callable entry points;
- sending a message to an actor creates an `ActorInvocation` task;
- task handles are the common waitable handles for actor calls, commands,
  workflows, and child-agent compatibility operations;
- `wait_for` suspends the current task continuation and waits on a target such
  as another task result, external event, or operator input;
- internal runs are scheduler attempts, not the public identity of agent work.

This gives Holon one model for public agents, named agents, child agents, shell
VM workflows, and self-scheduling while avoiding a second waitable handle that
duplicates `TaskHandle`.
