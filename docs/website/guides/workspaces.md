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
that one command process. Changing the active workspace requires
`holon workspace attach` or the `UseWorkspace` tool — this redefines where
runtime tools operate.

| Action | Effect |
|--------|--------|
| `cd /other` in shell | Only affects that command |
| `UseWorkspace` | Changes active workspace for all subsequent operations |
| `holon workspace attach /path` | CLI equivalent of UseWorkspace |

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

## Workspace Modes

### Direct Mode

The default. All operations happen in-place on the real filesystem:

```bash
holon workspace attach /home/user/project
```

Use direct mode for normal development work where you want changes to
persist on disk immediately.

### Isolated Mode (Worktrees)

Isolated workspaces create a managed worktree — a separate checkout that can
be modified without affecting the original:

```bash
# The runtime creates an isolated worktree
holon workspace attach /path/to/repo --mode isolated --isolation-label experiment
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

## UseWorkspace Tool

Agents use the `UseWorkspace` tool during execution to switch workspaces
programmatically:

- `UseWorkspace({ path: "/repo" })` — attach to a directory
- `UseWorkspace({ workspace_id: "agent_home" })` — return to agent home
- `UseWorkspace({ workspace_id: "ws-..." })` — switch to a known workspace
- `mode: "isolated"` — request an isolated worktree
- `access_mode: "exclusive_write"` — request write access

## See Also

- [Runtime Model](/concepts/runtime-model.md) — Workspace lifecycle in the runtime
- [CLI Reference](/reference/cli.md) — All workspace commands
- [Agent Templates](/guides/agent-templates.md) — How templates initialize agent homes
