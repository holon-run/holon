---
title: RFC: Agent Workspace Switching
date: 2026-04-21
status: accepted
issue:
  - 76
---

# RFC: Agent Workspace Switching

## Summary

Holon should replace the model-facing `EnterWorkspace` / `ExitWorkspace`
surface with an always-active workspace model:

- every agent starts with `AgentHome` as its active workspace
- every agent must always have exactly one active workspace
- `AgentHome` is the non-removable fallback workspace
- agents use one surface, `UseWorkspace`, to change the active workspace
- project paths and isolated execution roots are activated through `UseWorkspace`
- returning to a known workspace, including `AgentHome`, also uses
  `UseWorkspace(workspace_id=...)`

This removes the "no active workspace" state from the model-facing runtime
contract. File tools, shell tools, checkpoints, and finalization should always
have a concrete `execution_root` and `cwd`.

## Core Model

### Active Workspace Invariant

The runtime must maintain this invariant:

> An agent always has exactly one active workspace.

At initialization, that workspace is `AgentHome`.

`AgentHome` is a built-in workspace whose `workspace_anchor`,
`execution_root`, and default `cwd` point at the agent's durable local home. It
is suitable for scratch notes, durable agent state, and non-project-local
artifacts, but it is not a substitute for a project workspace.

`AgentHome` must not be detached or exited. If the agent leaves a project
workspace, the target active workspace is `AgentHome`.

### Workspace Use

The model-facing operation is making a workspace active, not entering and
exiting a nullable context.

Phase-1 has one model-facing workspace tool:

- `UseWorkspace`

`UseWorkspace` accepts exactly one workspace selector:

- `path`
- `workspace_id`

The `path` selector is discovery-first. It should:

- accept a host path the agent wants to work in
- detect the project workspace anchor and concrete execution root
- attach or adopt the workspace binding when policy allows it
- select direct or isolated execution
- set the detected or created workspace as the active workspace
- return the resulting `workspace_id`, `workspace_anchor`, `execution_root`,
  `cwd`, ownership, and cleanup hints

The `workspace_id` selector is identity-first. It should:

- activate a previously known attached workspace or retained execution root
- return to `AgentHome` when `workspace_id = "agent_home"`
- never create a new workspace or isolated execution root by itself
- never detach or delete a workspace

`AgentHome` is not a special `target` value. It is the fixed built-in workspace
id `agent_home`.

### Deprecated Surfaces

Holon should retire `EnterWorkspace`, `ExitWorkspace`, and `SwitchWorkspace`
from the model-facing tool surface. This RFC intentionally does not preserve
backward compatibility for those names.

The internal runtime may still have lower-level concepts such as binding,
projection, occupancy, and cleanup state, but those concepts should not leak as
the default agent-facing workflow.

If a low-level administrative API keeps equivalent operations for tests or
control-plane recovery, it should not be the default model-facing contract and
must not expose a state where an agent has no active workspace.

### Direct And Isolated Modes

`UseWorkspace` should expose a small mode vocabulary:

- `direct`
- `isolated`

`direct` means the active execution root is the existing workspace or existing
worktree detected from the given path.

`isolated` means the runtime creates an isolated execution root for that
workspace. Phase 1 may implement this with a git worktree, but the public
contract should not be named after git worktrees. Future backends may include
copy-on-write directories, overlay filesystems, or other snapshot mechanisms.

The runtime still records internal projection details. Phase-1 internal
projection vocabulary may include:

- `canonical_root`
- `git_worktree_root`
- future non-git isolated roots

### Access Modes

Phase-1 access vocabulary:

- `shared_read`
- `exclusive_write`

The runtime should be explicit about which access mode is active instead of
inferring mutation intent from shell or patch tool usage.

### Path And Workspace Detection

`UseWorkspace(path=...)` should be lenient on input and strict on resulting
state.

The agent may provide:

- a repository root
- a repository subdirectory
- an existing git worktree
- a non-git directory, when allowed by policy

The runtime should normalize the result into:

- `workspace_anchor`: stable project identity root
- `execution_root`: concrete filesystem root for tools
- `cwd`: working directory within the execution root
- `detected_kind`: for example `canonical_repo`, `repo_subdir`,
  `existing_git_worktree`, or `plain_directory`
- `ownership`: for example `external`, `session_owned`, or `task_owned`

If the input path is a subdirectory, the runtime should keep the agent's `cwd`
at that subdirectory while keeping `execution_root` at the normalized root.

### Isolation Labels

For `mode=isolated`, the agent should provide a short `isolation_label`.

The label is not a natural-language task description. It is a path and branch
name hint. The runtime should accept imperfect labels, slugify and bound them,
and append a unique suffix when needed.

The public contract should follow a "wide in, strict out" rule:

- avoid rejecting recoverable label shape problems
- return the actual branch, path, and identifier that were created
- report non-recoverable conflicts with a clear recovery hint

### Switching Away And Cleanup

Switching away from a workspace should not delete files.

When `UseWorkspace(workspace_id="agent_home")` leaves a `session_owned` isolated
execution root, the runtime should:

- release active occupancy for the previous execution root
- set `AgentHome` as the active workspace
- record a retained artifact entry for the previous isolated root
- return cleanup hints, including the retained path and a safe cleanup command
  when one can be computed
- avoid destructive cleanup by default

This keeps active execution state separate from artifact lifecycle state while
still preventing the runtime from losing track of session-created isolated
roots.

### Worktree Ownership

An isolated execution root can exist for different reasons:

- `task_owned`: created for supervised child-agent or task execution
- `session_owned`: created by the current agent through `UseWorkspace`
- `external`: created outside this runtime and adopted through `UseWorkspace`

Task-owned artifacts are governed by the task lifecycle. Session-owned
artifacts are retained on switch-away and should be surfaced through runtime
state and cleanup hints. External artifacts are never cleaned up by Holon.

### Execution Root Storage

Holon should store runtime-created isolated execution roots under a global
runtime-managed directory, for example:

```text
~/.holon/execution-roots/<backend>/<workspace-id>/<label>-<short-id>/
```

The exact path is runtime-owned. Agents should not have to choose it.

Agent-home symlinks or per-agent indexes may be added later for discoverability,
but they are convenience views. The authoritative state is the runtime registry
and retained-artifact records.

## Occupancy

Canonical-root mutation should respect minimal occupancy semantics. Holon does
not need a full lock-manager design yet, but it should model when mutation is
shared versus exclusive.

## Why This Replaces Enter/Exit

`EnterWorkspace` / `ExitWorkspace` makes "currently no active workspace" a
normal state. That is a poor fit for long-lived agents because common tools
still need a root:

- `exec_command` needs a default working directory
- `ApplyPatch` needs an execution root
- compaction checkpoints need a durable place to write or reference state
- finalization and follow-up turns need a stable local context

`UseWorkspace` keeps the useful parts of the workspace model while removing the
nullable active state and avoiding a second switch-only tool. Worktrees remain
an implementation detail of isolated execution, not the public workspace
vocabulary.

## Relationship To Other RFCs

- workspace binding defines `workspace_anchor` and `execution_root`
- execution policy uses `projection_kind` and `access_mode`
- agent delegation defines worktree-isolated child execution
- task surface narrowing defines task-owned worktree cleanup responsibility
- tool surface layering should list `UseWorkspace` as the model-facing local
  environment workspace tool, not `EnterWorkspace`, `ExitWorkspace`, or
  `SwitchWorkspace`

## Related Historical Notes

Supersedes and absorbs:

- `docs/archive/workspace-entry-and-projection-contract.md`
