# Workspace & Worktree Lifecycle

Every agent has one explicit active workspace defining the instruction root (`AGENTS.md` / `CLAUDE.md`), default `ExecCommand` working directory, and relative `ApplyPatch` base. `agent_home` is the fallback for agent-local state, not project source trees.

## Native Tools & CLI Equivalents

- `GetWorkspaceState` — inspect attached workspaces, execution roots/worktrees, active selection, and occupancy when uncertain.
- `AttachWorkspace` / `holon workspace attach <path>` — register a workspace binding without switching focus.
- `SwitchWorkspace` — activate an attached workspace or execution root by `workspace_id`, `execution_root_id`, or `path` without attaching or creating worktrees (`holon workspace exit` returns focus to `agent_home`).
- `CreateWorktree` (`workspace_id`, `branch`, `base_ref`) — create and activate an isolated git worktree by default, reusing a unique live worktree for that branch and failing closed on conflicts.
- `RemoveWorktree` — remove a clean, unoccupied non-canonical worktree (`return_to` is required when removing the active root).
- `DetachWorkspace` / `holon workspace detach <workspace_id>` — remove a workspace binding (returning to `agent_home` if active) without deleting retained worktrees on disk.

## Subprocess Working Directory & File References

- Shell `cd` inside `ExecCommand` affects only that subprocess and never updates the runtime active workspace, scoped instructions, or relative `ApplyPatch` base.
- Match file references to one of the four delivery surfaces (`delivery_contract`):
  - Active `workspace`: relative repo paths (`src/lib.rs`, `src/lib.rs:42`).
  - `agent_home`: agent-home-relative paths (`memory/self.md`, `media/generated/plot.png`) labeled as `agent_home`.
  - `runtime_artifact`: durable task/evidence log or artifact references.
  - `external_local_path`: explicit absolute paths outside workspace and `agent_home` when local filesystem paths are meaningful to the recipient.
