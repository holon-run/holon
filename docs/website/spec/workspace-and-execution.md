---
title: Workspace and execution
summary: Current workspace identity, agent home, execution roots, worktrees, and host-local policy contract.
order: 70
---

# Workspace and execution

This page defines the current contract for workspace identity, execution
roots, worktree isolation, and host-local execution policy.

> **Last verified:** 2026-05-23 against `src/types.rs`
> `ActiveWorkspaceEntry`, `WorkspaceOccupancyRecord`, `WorktreeSession`,
> `ExecutionSnapshot`, and `src/runtime/workspace.rs`.

## Source RFCs

- [Workspace Binding and Execution Roots](https://github.com/holon-run/holon/blob/main/docs/rfcs/workspace-binding-and-execution-roots.md)
- [Workspace Entry and Projection](https://github.com/holon-run/holon/blob/main/docs/rfcs/workspace-entry-and-projection.md)
- [Execution Policy and Virtual Execution Boundary](https://github.com/holon-run/holon/blob/main/docs/rfcs/execution-policy-and-virtual-execution-boundary.md)
- [Agent Home Directory Layout](https://github.com/holon-run/holon/blob/main/docs/rfcs/agent-home-directory-layout.md)
- [Instruction Loading](https://github.com/holon-run/holon/blob/main/docs/rfcs/instruction-loading.md)
- [Agent and Workspace Memory](https://github.com/holon-run/holon/blob/main/docs/rfcs/agent-and-workspace-memory.md)

## Core model

Every agent has exactly one **active workspace**. The active workspace defines:

| Concept | Meaning |
|---------|---------|
| `workspace_id` | Stable identifier for the workspace |
| `workspace_anchor` | Filesystem path to the workspace root |
| `execution_root` | The root for process execution (may differ from anchor) |
| `cwd` | Current working directory for shell commands |
| `projection_kind` | How the workspace is projected (`CanonicalRoot`, `GitWorktreeRoot`) |
| `access_mode` | How the agent holds the workspace (`SharedRead`, `ExclusiveWrite`) |

### Active workspace vs shell `cd`

- The active workspace is **runtime state**, not shell state.
- Shell `cd` in `ExecCommand` changes that one command's working directory
  but does **not** change the active workspace, instruction root, AGENTS.md
  scope, or `ApplyPatch` relative-path base.
- `UseWorkspace` is the tool for switching the active workspace.

## Agent home

`agent_home` is the built-in fallback workspace for agent-local state:

| Directory | Purpose |
|-----------|---------|
| `AGENTS.md` | Long-lived agent contract (loaded as guidance) |
| `memory/` | Curated memory markdown (`self.md`, `operator.md`) |
| `notes/` | Working notes |
| `work-items/` | WorkItem plan artifacts (`plan.md`) |
| `skills/` | Agent-local skills |
| `.holon/` | Runtime-owned state, ledger, index, cache |

**Key contract:**

- `.holon/` is runtime-owned; agents must not edit it.
- `AGENTS.md` may evolve but should capture durable agent behavior, not
  transient plans or copied project docs.
- `agent_home` is always available as a workspace, even when no project
  workspace is attached.

## Workspace occupancy

Workspaces track **occupancy**: which agent holds the workspace and how:

| Field | Purpose |
|-------|---------|
| `holder_agent_id` | The agent currently occupying the workspace |
| `access_mode` | `SharedRead` or `ExclusiveWrite` |
| `acquired_at` | When occupancy was acquired |
| `released_at` | When occupancy was released (if released) |

Workspace occupancy is used for coordination; it is not a hard lock. The
runtime uses occupancy records for diagnostics and cleanup, not for
preventing concurrent access at the filesystem level.

## Worktrees

When an agent needs isolated file changes, `UseWorkspace` with
`mode=isolated` creates a runtime-managed worktree:

- The worktree has a separate `execution_root` from the canonical workspace.
- Worktree lifecycle is tied to the agent session; the runtime cleans up on
  agent stop or explicit release.
- Worktrees use the host-local filesystem (git worktrees or temp directories);
  they are not containerized sandboxes.

## Execution snapshot (`ExecutionSnapshot`)

The `ExecutionSnapshot` in `AgentSummary` captures the current execution
context:

- Active run id
- Active workspace id and occupancy
- Execution root and cwd
- Worktree session (if applicable)
- Host-local policy flags

## Host-local policy

Holon's current execution model is **host-local**: processes run on the host
filesystem with the agent user's permissions. Key constraints:

- `cwd` is always within the execution root.
- Process execution is not containerized or sandboxed by the runtime.
- Network access is not confined by default.
- The `execution_environment` summary in model context describes the current
  policy snapshot as a transparency contract, not a hard sandbox guarantee.

## Known gaps

- Worktree cleanup is best-effort; stale worktrees may persist after abnormal
  agent termination.
- Workspace occupancy is advisory; the runtime does not enforce exclusive
  write access at the filesystem level.
- Isolated worktrees use git worktrees; non-git workspaces fall back to temp
  directories with weaker cleanup guarantees.
