---
title: RFC: Agent Delegation Tool Plane
date: 2026-04-21
status: draft
---

# RFC: Agent Delegation Tool Plane

## Summary

This RFC proposes that bounded delegation in Holon should move from a task-kind
mental model toward an explicit agent-plane model.

The central direction is:

- delegated work is fundamentally context creation
- public delegation tools should reflect that directly
- `child_agent_task` is the internal supervision shape for private child
  delegation
- `subagent_task` and `worktree_subagent_task` should be treated as legacy
  migration records, not the final public abstraction
- worktree-isolated delegation should be expressed as `SpawnAgent` with a
  workspace mode, not as a separate public task kind

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

This suggests an eventual surface shaped more like:

- spawn bounded child agent
- spawn bounded child agent with worktree isolation

The first public direction is `SpawnAgent` with `private_child` and
`workspace_mode=worktree`. The important point is that delegation should belong
to the agent plane.

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

- `SpawnAgent(private_child, workspace_mode=worktree)` creates the delegated
  child context
- the spawned child uses the task-created worktree as its active execution
  projection
- the supervising task owns the task-created worktree artifact
- the child agent is the active holder while it runs, but not the lifecycle
  owner of the artifact

This means worktree lifecycle should not follow the child agent directly. The
child may finish, stop, or be archived while the parent still needs to inspect
the task result. Cleanup therefore belongs to the supervising task or later
artifact garbage collection.

The task record now uses a single `child_agent_task` kind for supervised
private child delegation. Worktree isolation is represented with metadata:

- `workspace_mode=worktree`
- worktree path and branch metadata
- artifact cleanup state

## Migration Direction

The safest migration path is:

1. keep current implementation support
2. document legacy subagent task kinds as transitional forms
3. introduce agent-plane wording in prompts and docs
4. add an explicit `SpawnAgent` agent-plane tool
5. route worktree-isolated delegation through `SpawnAgent(...,
   workspace_mode=worktree)`
6. retire subagent task wording once the new surface is stable
7. merge `worktree_subagent_task` into the unified `child_agent_task`
   representation

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
supervision now uses `child_agent_task`.

This gives Holon a cleaner runtime story:

- command tasks are for command execution
- child agents are for bounded delegated context
- task-created worktrees are supervised artifacts, not separate public task
  kinds
