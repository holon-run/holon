---
title: Workspaces and execution environments
summary: Workspace, execution root, and worktree binding, switching, and isolation contracts.
order: 30
---

# Workspaces and execution environments

For the model, see [Workspaces](/concepts/workspaces.md); for the procedure,
see [Work in a workspace or worktree](/guides/work-with-workspaces.md).
Internal invariants are in the
[Workspace and execution spec](/spec/workspace-and-execution.md).
Every agent has one active workspace, which can use a canonical root or a
registered worktree projection.

## What a Workspace Defines

| Concern | Set by workspace |
|---------|-----------------|
| Instruction root | Where `AGENTS.md` and policy files resolve |
| Execution root | Default working directory for commands in the active projection |
| ApplyPatch target | Base for relative patch paths; explicit absolute paths remain explicit targets |
| Memory scope | Workspace-scoped episode records and indexes |

## Workspace vs Shell Directory

See [Workspaces](/concepts/workspaces.md) for the scope distinction.

| Action | Effect |
|--------|--------|
| `cd /other` in shell | Only affects that command |
| `AttachWorkspace` | Adds a workspace binding without switching |
| `SwitchWorkspace` | Changes active workspace for subsequent operations |
| `holon workspace attach /path` | Attaches a binding for a path; does not switch the active workspace |

## Workspace Commands

### Attach

`holon workspace attach [--agent <id>] <path>`

This discovers or creates a workspace record for the directory and binds it to
the agent. Attaching does not by itself change the active workspace; activate
it with `SwitchWorkspace` when you are ready to work there. The binding
persists across sessions.

### Exit

`holon workspace exit [--agent <id>]` returns to the agent's home workspace.

### Detach

`holon workspace detach [--agent <id>] <workspace-id>` removes the target
agent's binding. An active target returns to agent home first.

Detaching does not delete project files or retained worktree artifacts.
Memory and episode records associated with the workspace are preserved.

## Worktree Isolation

`CreateWorktree` creates a managed linked worktree from an explicit attached
workspace, branch, and base ref:

| Field | Contract |
|-------|----------|
| `workspace_id` | Required attached repository workspace |
| `branch` | Required branch to check out |
| `base_ref` | Required starting ref for a new worktree |
| `activate` | Defaults to `true` |
| `on_existing` | `reuse` or `error`; reuse requires one unambiguous live worktree and does not reset it |
| `label` | Optional descriptive label |

`RemoveWorktree` accepts exactly one of `execution_root_id` or `path`.
Removal refuses dirty, locked, unregistered, canonical, or occupied roots.
Removing an active root requires `return_to`. `branch_policy` is `keep` or
`delete_if_merged`; the latter can use `merged_into` to name the comparison ref.
See the [workspace guide](/guides/work-with-workspaces.md) for safe cleanup.

## Agent Home vs Project Workspace

See [Workspaces](/concepts/workspaces.md) for their roles and
[Customize an agent](/guides/customize-agent.md) for choosing where to put
guidance. Agent home normally lives at `~/.holon/agents/<agent-id>/`; use
runtime-reported paths when `HOLON_HOME` or the active projection differs.

## Agent Workspace Tools

- `GetWorkspaceState({})` — inspect bindings, active projection, worktrees, and occupancy
- `AttachWorkspace({ path: "/repo" })` — attach without switching
- `SwitchWorkspace({ workspace_id: "ws_..." })` — activate a canonical root
- `SwitchWorkspace({ execution_root_id: "..." })` — activate a retained worktree
- `SwitchWorkspace({ workspace_id: "agent_home" })` — return to agent home
- `CreateWorktree(...)` — create or safely reuse a linked worktree
- `RemoveWorktree(...)` — clean-only registered cleanup
- `DetachWorkspace({ workspace_id: "ws_..." })` — remove a binding; active targets first return to agent home

`UseWorkspace` remains a hidden compatibility alias for historical calls.
`SwitchWorkspace` accepts exactly one selector: `workspace_id`,
`execution_root_id`, or `path`, with optional `cwd`. It only activates an
existing binding or registered projection; it does not attach a repository.
Shell `workdir` and `cd` do not change that activation.

## See Also

- [Runtime Model](/concepts/runtime-model.md) — Workspace lifecycle in the runtime
- [CLI Reference](/reference/cli.md) — All workspace commands
- [Agent Templates](/reference/agent-templates.md) — How templates initialize agent homes
