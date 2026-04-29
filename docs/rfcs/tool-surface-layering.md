---
title: RFC: Tool Surface Layering
date: 2026-04-21
status: draft
---

# RFC: Tool Surface Layering

## Summary

This RFC proposes a clearer layering model for Holon's public tools.

The goal is to stop treating all asynchronous or stateful capabilities as one
flat "tool bag" and instead organize the surface around a small number of
planes with distinct responsibilities.

It also proposes one important contract rule for the model-facing surface:

- a given agent should see a stable tool surface derived from its profile and
  runtime capabilities
- message provenance and trust should affect authority interpretation and
  policy, not whether a tool exists in this turn

The proposed planes are:

- work plane
- command plane
- agent plane
- waiting plane

Within that plane model, the stable public catalog should be understandable as
capability families such as:

- core agent tools
- local environment tools
- agent creation tools
- authority-expanding tools
- external trigger tools

This RFC is the umbrella document for the tools RFC series.

## Problem

Holon's current tool surface has useful pieces, but the semantics are uneven.

In particular:

- high-level work tracking lives in `update_work_item` / `update_work_plan`
- command execution lives in `exec_command` plus task control tools
- waiting lives partly in `Sleep`, partly in `CreateExternalTrigger`, and
  partly in background task semantics

These capabilities are not all at the same level, but they still appear in a
mostly flat catalog.

This creates several problems:

- the model sees different execution layers as siblings
- `Task` becomes overloaded as a catch-all for unrelated semantics
- prompt guidance becomes harder because "when to use which tool" is not
  anchored in a clear runtime model
- the current implementation can make the visible tool set drift based on the
  current message trust class, which is a poor fit for a long-lived agent
- future extensions such as interactive command continuation and explicit child
  agents risk adding more overlap instead of reducing it

## Goals

- define a stable conceptual layering for Holon's public tools
- define a stable per-agent capability classification for the model-facing tool
  catalog
- make future tool additions fit into explicit planes
- reduce overlap between task, agent, and waiting semantics
- improve prompt guidance by giving each tool family a clearer home
- keep room for Holon-specific runtime primitives instead of copying Codex or
  Claude Code directly

## Non-goals

- do not finalize every individual tool name in this RFC
- do not require all existing tools to be renamed immediately
- do not replace current implementation details in one pass
- do not require the internal runtime execution layer to use the same names as
  the public surface

## Stable Tool Surface

For a given agent, the public tool catalog should be stable by default.

The primary inputs to tool visibility should be:

- agent profile
- runtime capability
- active workspace or execution boundary state

The primary inputs should not be:

- whether the current message came from an operator, timer, callback, channel,
  or webhook
- whether the current message trust label is `trusted_*` or
  `untrusted_external`

Message provenance still matters, but it should matter through:

- authority interpretation
- instruction precedence
- audit and prompt framing
- admission and provenance framing

It should not usually matter by making the tool list itself change from turn to
turn for the same agent.

## Stable Capability Families

The plane model remains the runtime architecture lens. For the model-facing tool
catalog, Holon should also describe a small set of stable capability families.

### 1. Core Agent Tools

These are the stable baseline tools an agent can use without directly widening
its local execution boundary.

Typical members include:

- `Sleep`
- `AgentGet`
- `Enqueue`
- `TaskList`
- `TaskStatus`
- `TaskOutput`
- `TaskInput`
- `TaskStop`
- `update_work_item`
- `update_work_plan`

This family is the default center of agent continuity, inspection, and control.

### 2. Local Environment Tools

These tools operate inside the agent's currently attached and active local
execution boundary.

Typical current members include:

- `exec_command`
- `ApplyPatch`
- `UseWorkspace`

These tools describe the local execution surface that is already available to
the agent through its current profile and runtime boundary state.

`UseWorkspace` is the single model-facing operation for making a workspace
active. With `path`, it discovers, attaches, and activates a project workspace
or isolated execution root. With `workspace_id`, it activates a known workspace,
including returning to the built-in `agent_home` workspace.

The model-facing surface should not expose a state where the agent has no
active workspace. `EnterWorkspace` and `ExitWorkspace` are retired names in the
new contract, and `SwitchWorkspace` is intentionally not added as a separate
model-facing tool.

### 3. Agent Creation Tools

These tools create or supervise additional agent contexts.

Typical members include:

- `SpawnAgent`

This family belongs to the agent plane, not the command plane and not the
waiting plane.

### 4. Authority-Expanding Tools

These tools widen what the agent is allowed to operate on.

The clearest current non-tool example is:

- `AttachWorkspace`

This family should be treated separately from ordinary local-environment tools.
Attaching a new workspace is not the same thing as operating within an already
attached workspace.

If Holon later exposes this family to the model, it should do so through
explicit agent-profile rules rather than by making arbitrary path attachment
feel like a normal local edit or command action.

`DetachWorkspace` belongs with the same binding-management concern if exposed.
The first version should be control-plane or CLI oriented rather than part of
the default model-facing local environment surface. It removes an agent-local
workspace binding; it does not delete directories or remove host registry
entries.

Holon should not add `ForgetWorkspace` to the public tool surface in this
phase. Host registry cleanup should be a separate later design.

### 5. External Trigger Tools

These tools create or cancel an external channel that may wake or re-enter the
agent later.

Typical members include:

- `CreateExternalTrigger`
- `CancelExternalTrigger`

This family belongs to the waiting plane, but it is specific enough that it
deserves to be called out separately from local waiting posture such as
`Sleep`.

The next naming review targets after this family should be:

- `exec_command`
- `update_work_item`
- `update_work_plan`

## Proposed Planes

## 1. Work Plane

The work plane answers:

- what meaningful work is the agent currently advancing?

This plane is represented by:

- `update_work_item`
- `update_work_plan`

It is intentionally higher-level than turn execution and higher-level than
background jobs.

The work plane should own:

- durable delivery target
- current progress summary
- plan state
- waiting/completed status at the work level

The work plane should not own:

- raw command execution state
- child-agent lifecycle details
- task output bytes

## 2. Command Plane

The command plane answers:

- what shell execution should happen right now?
- what long-running command is still running?
- how do I inspect or stop that command?

This plane is represented by:

- `exec_command`
- task inspection and control tools for command-backed execution

The command plane should own:

- foreground shell execution
- managed background command lifecycle
- output retrieval
- stop semantics
- future interactive command continuation

The command plane is the correct home for:

- `tty=true`
- command auto-promotion into managed runtime execution
- command-specific output and stop behavior

## 3. Agent Plane

The agent plane answers:

- when should Holon create another context to do bounded work?
- how should Holon inspect the context-owning agent itself?

This plane should eventually represent:

- agent inspection
- bounded child-agent spawn
- worktree-isolated child-agent spawn
- later multi-agent extensions if Holon chooses to expose them

The key point is that delegation should be treated as context creation, not as
just another task kind.

The same plane should also own the stable inspection primitive for the current
agent, rather than overloading task metadata surfaces for agent identity and
work-focus questions. That inspection primitive should be `AgentGet`, returned
through an `AgentGetResult` envelope, while task-backed handles remain the
responsibility of `TaskStatus`.

## 4. Waiting Plane

The waiting plane answers:

- what should wake the agent later?
- what condition is the agent currently blocked on?

This plane should own:

- callback-backed waiting
- timer-backed waiting
- explicit cancellation of no-longer-relevant waits
- external trigger capabilities such as callback-backed wake and re-entry

Waiting is not the same as command execution and is not the same as high-level
work identity. It deserves a separate plane.

## Relationship Between The Planes

The intended relationship is:

- one work item may use command tools
- one work item may spawn child agents
- one work item may create waiting intents
- command and waiting state may drive later turns, but they do not replace the
  work item as the high-level unit

In short:

- work plane is goal-oriented
- command plane is execution-oriented
- agent plane is context-oriented
- waiting plane is reactivation-oriented

## Design Rules

Future tool changes should follow these rules:

1. every tool should belong primarily to one plane
2. public tool semantics should reflect runtime meaning, not only historical
   implementation shape
3. command lifecycle should not be mixed with delegation lifecycle
4. waiting should not be smuggled in through generic background-task language
5. tool existence should be stable per agent and should not drift merely because
   the current message provenance changed
6. the public surface may be narrower and more opinionated than the internal
   runtime execution substrate

## Immediate Implications

This layering suggests several follow-up directions:

- narrow `Task` semantics on the public surface toward command-backed managed
  execution
- move delegation toward an explicit agent plane
- keep waiting semantics centered on callbacks and timers
- separate authority-expanding tools such as `AttachWorkspace` from ordinary
  local-environment tools
- keep workspace attach/detach binding management separate from active
  workspace switching
- retire dedicated destructive worktree-discard tools in favor of task-owned
  cleanup and ordinary git worktree management
- use the work plane as the durable home for cross-turn intent

These implications are split into separate RFCs:

- agent delegation tool plane
- interactive command continuation
- tool contract consistency

## Migration Direction

The first migration step does not need to be a breaking rename.

A safe initial path is:

1. adopt the plane model in docs and prompt guidance
2. stop adding new cross-plane mixed tools
3. make new tool work fit one of the defined planes
4. gradually migrate overloaded tools to the correct plane

## Summary

Holon should stop treating tools as one flat list of unrelated capabilities.

The public surface should be organized around four explicit planes:

- work
- command
- agent
- waiting

And the model-facing catalog should be understandable through stable capability
families such as:

- core agent tools
- local environment tools
- agent creation tools
- authority-expanding tools
- external trigger tools

This gives Holon a clearer foundation for future tool evolution while staying
aligned with its runtime-first design.
