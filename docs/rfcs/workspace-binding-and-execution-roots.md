---
title: RFC: Workspace Binding and Execution Roots
date: 2026-04-21
status: accepted
issue:
  - 62
---

# RFC: Workspace Binding and Execution Roots

## Summary

Holon should keep host-owned workspace identity separate from agent-owned
durable state. A workspace is not just a path alias. It is the stable resource
boundary used for execution policy, workspace-scoped memory, instruction
loading, and execution projection.

The minimum stable directory model is:

- `holon_home`
- `workspace_registry`
- `agent_home`
- `attached_workspaces`
- `active_workspace`
- `workspace_anchor`
- `execution_root`
- `cwd`

## Core Judgments

- workspaces are host-owned resources, not private agent homes
- workspace identity is the durable key for workspace-scoped memory,
  instructions, and policy decisions
- `agent_home` is durable identity and state, not project identity
- the default initial workspace should be the agent home so every agent has a
  stable local root before any external project is attached
- `workspace_anchor` is the stable project root for instructions and local
  discovery
- `execution_root` is the concrete filesystem projection used for tools
- shell `cd` does not redefine workspace attachment or instruction roots
- worktree execution is a projection of one workspace, not a new project
  identity
- Holon is agent-first and supports multiple attached workspaces over time,
  rather than assuming one session is permanently bound to the process cwd

## Runtime Model

### `workspace_registry`

The host keeps a registry of known workspaces. Each workspace entry has a
stable `workspace_id` and `workspace_anchor`.

### `attached_workspaces` and `active_workspace`

An agent may attach to more than one workspace over time. One workspace entry
is active for the current execution.

`attached_workspaces` is the agent's durable workspace capability set. It
answers which known workspaces the agent may enter, remember, and reason about.

`active_workspace` is only the current execution choice. It should not be
treated as the complete set of workspaces available to the agent.

### `DetachWorkspace`

Detaching a workspace removes one workspace binding from one agent's
`attached_workspaces`.

It must not:

- delete the workspace directory
- delete or rewrite the host `workspace_registry`
- delete task-owned worktree artifacts
- affect other agents that have attached the same workspace

Detaching should be allowed even when the workspace path no longer exists,
because one important use case is cleaning stale agent-local bindings.

The first version should be a control-plane or CLI operation, not a default
model-facing local-environment tool. It shrinks an agent's durable workspace
capability set, so it belongs with binding management rather than ordinary
file work.

If the target workspace is currently active, the default behavior should reject
the detach and require switching to another workspace first. The default target
for leaving a project workspace is `AgentHome`. A future recovery path may add
an explicit force mode for broken active workspaces, but that is a separate
failure-recovery design.

### `ForgetWorkspace`

Holon should not introduce host-global workspace forgetting in this phase.

The host workspace registry may remain append-only or historically retained
until a separate registry cleanup design exists. Agent-local stale state should
be solved first with `DetachWorkspace`.

### `workspace_anchor`

`workspace_anchor` is the stable root used for:

- project identity
- workspace-scoped instructions
- workspace-scoped skill discovery

### `execution_root`

`execution_root` is the concrete root used for file and shell work. By default
it matches `workspace_anchor`, but managed worktree execution may change it.

### `cwd`

`cwd` is the current working directory within `execution_root`. It is allowed
to move during work, but it must stay subordinate to the execution root.

## Shell Rule

Shell side effects are weak evidence. A shell command may change `cwd`, but it
must not implicitly redefine:

- workspace attachment
- instruction roots
- write authority

## Relationship To Other RFCs

- instruction loading builds on `workspace_anchor`
- agent workspace switching builds on `execution_root`
- execution policy uses the same workspace vocabulary
- tool surface layering should keep attach/detach binding management separate
  from active workspace switching

## Related Historical Notes

Supersedes and absorbs the workspace-binding portions of:

- `docs/archive/workspace-binding-and-instruction-loading.md`
