---
title: RFC: Agent Delegation Tool Plane
date: 2026-04-21
status: draft
---

# RFC: Agent Delegation Tool Plane

> **Supersession note:** The draft
> [Unified Agent Identity, Relations, And Message Delivery](./agent-identity-relations-and-message-delivery.md)
> RFC preserves bounded delegation while replacing the combined `SpawnAgent`
> product surface with `CreateAgent` and result-bearing `InvokeAgent`; reliable
> message delivery remains an internal runtime contract in the first release.
> `InvokeAgent(target = new_subagent)` is the canonical bounded delegation
> entrypoint.

## Summary

This RFC proposes that bounded delegation in Holon should move from a task-kind
mental model toward an explicit agent-plane model.

The central direction is:

- delegated work is fundamentally context creation
- public delegation tools should reflect that directly
- `child_agent_task` is a legacy combined task/supervision record to migrate,
  not the canonical target model
- `subagent_task` and `worktree_subagent_task` should be treated as legacy
  migration records, not the final public abstraction
- result-bearing delegation should be expressed as
  `InvokeAgent(target = new_subagent)` with an optional workspace mode, not as
  `SpawnAgent` or a separate public task kind

## Problem

Today Holon exposes bounded delegation through:

- `CreateTask(kind=subagent_task, ...)`
- `CreateTask(kind=worktree_subagent_task, ...)`

This works operationally, but it obscures what is really happening.

A delegated subagent run is not just "a background task." It is:

- another bounded execution context
- with its own prompt and local reasoning
- with separate execution state
- optionally with separate workspace isolation

Treating this primarily as a task kind creates several problems:

- it overloads `Task` with context-creation semantics
- it makes delegation look more similar to command execution than it really is
- it makes future multi-agent evolution harder to explain
- it keeps public naming behind Holon's emerging runtime model

## Goals

- define delegation as part of the agent plane
- separate child-agent semantics from command-task semantics
- keep bounded delegation available without requiring a full multi-agent public
  platform immediately
- leave room for worktree-isolated child execution

## Non-goals

- do not require Holon to expose open-ended multi-agent collaboration in the
  first version
- do not require public named child agents immediately
- do not remove current implementation support for bounded delegation before a
  replacement exists

## Proposed Direction

## 1. Delegation Is Context Creation

The public mental model should be:

- create another execution context to handle bounded work

not:

- create a generic task and hope the model remembers it is actually another
  agent

The superseding public surface is:

- `InvokeAgent(target = new_subagent)` for bounded child delegation
- `InvokeAgent(target = new_subagent, workspace_mode = worktree)` for
  worktree-isolated delegation

The important point is that delegation belongs to the agent plane while the
returned `TaskHandle` represents the result-bearing invocation.

## 2. Task Should Not Stay The Primary Delegation Word

Task control is a good fit for:

- command lifecycle
- output retrieval
- stop behavior

It is a worse fit for:

- context isolation
- bounded delegated reasoning
- worktree-isolated child execution

Holon should therefore avoid making `Task` the long-term primary word for
delegation.

## 3. Boundedness Remains Essential

Moving delegation into the agent plane does not mean adopting an unconstrained
worker swarm.

Bounded delegation should remain explicit:

- child scope is limited
- child lifecycle is finite
- result returns to the parent context
- ownership and cleanup are runtime-controlled

The goal is not "more agents." The goal is "clearer semantics for the agent
contexts Holon already creates."

## 4. Worktree Isolation Belongs Here Too

`worktree_subagent_task` is the clearest example that current public naming is
behind runtime reality.

A worktree-isolated delegated run is not merely a generic task. It is:

- child execution
- with a distinct workspace projection
- with its own artifact lifecycle

That should be described in agent-plane terms, not only task-plane terms.

The intended public model is:

- `InvokeAgent(target=new_subagent, initial_message=...,
  workspace_mode=worktree)` creates the delegated child context and its first
  result-bearing invocation
- `initial_message` is delivered as the child agent's first delegation message
  and is used to derive the stable parent-visible task label
- the created child uses the delegation-created worktree as its active execution
  projection
- durable ownership evidence keeps the worktree artifact attached to the
  supervised child delegation until lifecycle cleanup resolves it
- the child agent is the active holder while it runs, but not the lifecycle
  owner of the artifact

This means worktree lifecycle should not follow the child agent directly. The
child may finish an invocation or stop while the parent still needs to inspect
the task result or decide its lifecycle. Cleanup therefore remains governed by
the persistent supervision relation, durable artifact ownership evidence, and
later artifact garbage collection; it does not belong to one invocation task.

The current implementation may use a single `child_agent_task` record for
supervised child delegation. That combined record is a legacy migration shape.
The canonical target model stores the result-bearing request as an
`ActorInvocation` task and lifecycle authority as a separate
`AgentSupervision` relation. Worktree isolation remains execution-projection
metadata:

- `workspace_mode=worktree`
- worktree path and branch metadata
- artifact cleanup state

The `TaskHandle` returned by
`InvokeAgent(target=new_subagent, initial_message=...)` identifies only the
`ActorInvocation`. `TaskStatus` and `TaskOutput` report that invocation's
lifecycle and result. They may include references to the delegated child and
its supervision relation, but the task is not itself `child_supervision`:

- `child_agent_id` is the delegated private context
- `task_id` is the parent-visible invocation handle for status, output,
  cancellation, and task-correlated input
- `supervision_id` identifies the separate persistent lifecycle relation
- `parent_agent_id` and optional work-item delegation ids preserve ownership
- lifecycle stop/delete authority and the unresolved cleanup obligation remain
  on the supervision relation after the invocation becomes terminal
- later work sent to the retained child uses another
  `InvokeAgent(target=existing_agent, ...)`, not the completed invocation task

## Migration Direction

The safest migration path is:

1. keep current implementation support
2. document legacy subagent and `child_agent_task` records as transitional forms
3. introduce agent-plane wording in prompts and docs
4. add explicit `CreateAgent` and `InvokeAgent` agent-plane tools
5. route bounded and worktree-isolated delegation through
   `InvokeAgent(target=new_subagent, ...)`
6. retire subagent task wording once the new surface is stable
7. migrate combined task/supervision records into separate `ActorInvocation`
   tasks and `AgentSupervision` relations, and remove `SpawnAgent` from the
   agent-facing tool surface

## Relationship To Work Items

Delegation should not replace work items.

The intended relationship is:

- work item expresses the high-level objective
- child agent executes bounded sub-work
- command task executes shell-level operational work when needed

This keeps goal identity, context isolation, and command execution separate.

The parent agent should remain responsible for:

- synthesis of child findings
- prioritization across work items
- final user-facing reasoning

Delegation should not transfer overall understanding to the child.

## Initial Delegation Scope

Holon should begin with bounded private child delegation, not with general
public multi-agent collaboration.

That means the initial delegation profile should prefer:

- private child agents
- finite lifecycle by default
- parent-directed or machine-directed ingress only
- explicit promotion only when a delegated objective truly needs a durable
  private child across waits

This keeps the first delegation surface useful for real work without forcing
Holon to solve public collaborative-agent semantics too early.

## Default Child Context Rule

For bounded child delegation, the default should be:

- `fork` uses runtime-derived inherited context plus explicit handoff
- `fresh` uses explicit handoff without inheriting the parent conversation view

When a parent has an active work item, the inherited context for `fork` should
normally be centered on that active work item rather than on the parent's full
raw transcript.

That default inherited view should prefer:

- active work item snapshot
- active work plan snapshot
- waiting state relevant to that work item
- a small recent hot tail
- related episode summaries when needed

This keeps child delegation aligned with Holon's work-plane truth and avoids
turning delegation into ad hoc transcript copying.

## Open Questions

The following questions remain open after this RFC:

- should Holon eventually expose durable named child agents, or keep the public
  agent plane bounded-only for a longer time?
- should parent-child communication be direct tool surfaces or remain result
  rejoin only in the first version?
- how far future artifact GC should go beyond current task-owned cleanup
  metadata

## Summary

Holon treats delegation as part of the agent plane rather than keeping
`subagent_task` as the long-term public abstraction. Runtime-created
delegation uses a result-bearing `ActorInvocation` task alongside a persistent
`AgentSupervision` relation.

This gives Holon a cleaner runtime story:

- command tasks are for command execution
- child agents are for bounded delegated context
- task-created worktrees are supervised artifacts, not separate public task
  kinds
