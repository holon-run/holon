---
title: Workspaces
summary: Workspace lifecycle — attach, exit, detach, worktree isolation, and how workspaces differ from shell directories.
order: 17
---

# Workspaces

A workspace is the execution root for an agent — where it reads files, runs
commands, and applies patches. Every agent always has exactly one active
workspace.

## What a Workspace Defines

| Concern | Set by workspace |
|---------|-----------------|
| Instruction root | Where `AGENTS.md` and policy files resolve |
| Execution root | Default working directory for commands |
| ApplyPatch target | Where file mutations land |
| Memory scope | Workspace-scoped episode records and indexes |

## Workspace vs Shell Directory

A workspace is **not** a shell `cd`. Shell `cd` changes the directory for
that one command process. Agents use explicit binding and activation tools to
change where runtime tools operate.

| Action | Effect |
|--------|--------|
| `cd /other` in shell | Only affects that command |
| `AttachWorkspace` | Adds a workspace binding without switching |
| `SwitchWorkspace` | Changes active workspace for subsequent operations |
| `holon workspace attach /path` | Attaches and activates through the CLI compatibility flow |

## Workspace Commands

### Attach

Attach to a project directory as the active workspace:

```bash
holon workspace attach /path/to/project
holon workspace attach --agent my-agent /path/to/project
```

This discovers or creates a workspace record for the directory and makes it
active for the agent. The workspace persists across sessions.

### Exit

Return to the agent's home workspace:

```bash
holon workspace exit
holon workspace exit --agent my-agent
```

### Detach

Remove a workspace record entirely:

```bash
holon workspace detach <workspace-id>
```

Detaching does not delete the directory — it removes the workspace record
from Holon's index. Memory and episode records associated with the workspace
are preserved.

## Worktree Isolation

`CreateWorktree` creates a managed linked worktree from an explicit attached
workspace, branch, and base ref:

```text
CreateWorktree {
  workspace_id: "ws_...",
  branch: "feature/example",
  base_ref: "origin/main"
}
```

Isolated workspaces are useful for:

- Safe experimentation without polluting the working copy
- Parallel work on the same repository by different agents
- PR review branches where changes should not leak

## Agent Home vs Project Workspace

| Workspace Type | Purpose | Example |
|---------------|---------|---------|
| Agent home | Agent-local state and memory | `~/.holon/agents/my-agent/` |
| Project workspace | Code and files being worked on | `/path/to/project` |

Every agent starts with its agent home as the active workspace. Use
`workspace attach` to switch to a project workspace, and `workspace exit` to
return to agent home.

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

## See Also

- [Runtime Model](/concepts/runtime-model.md) — Workspace lifecycle in the runtime
- [CLI Reference](/reference/cli.md) — All workspace commands
- [Agent Templates](/guides/agent-templates.md) — How templates initialize agent homes
