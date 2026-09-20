---
title: Workspace and execution
summary: Current workspace identity, agent home, execution roots, worktrees, and host-local policy contract.
order: 70
---

# Workspace and execution

This page defines the current contract for workspace identity, execution
roots, worktree isolation, and host-local execution policy.

> **Last verified:** 2026-09-13 against `src/types.rs`
> `ActiveWorkspaceEntry`, `WorkspaceOccupancyRecord`, `WorktreeSession`,
> `src/system/types.rs` `ExecutionSnapshot`, and `src/runtime/workspace.rs`.

## Source RFCs

- [Workspace Binding and Execution Roots](https://github.com/holon-run/holon/blob/main/docs/rfcs/workspace-binding-and-execution-roots.md)
- [Workspace Entry and Projection](https://github.com/holon-run/holon/blob/main/docs/rfcs/workspace-entry-and-projection.md)
- [Agent Workspace Tool Surface](https://github.com/holon-run/holon/blob/main/docs/rfcs/agent-workspace-tool-surface.md)
- [Execution Root Registry](https://github.com/holon-run/holon/blob/main/docs/rfcs/workspace-execution-root-registry.md)
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
- `SwitchWorkspace` activates an existing workspace or execution root.
- `AttachWorkspace` adds a binding without changing the active projection.

## Agent home

`agent_home` is the built-in fallback workspace for agent-local state:

| Directory | Purpose |
|-----------|---------|
| `AGENTS.md` | Long-lived agent contract (loaded as guidance) |
| `memory/` | Curated memory markdown (`self.md`, `operator.md`) |
| `notes/` | Working notes |
| `work-items/` | WorkItem plan artifacts (`plan.md`) |
| `skills/` | Agent-local skills |
| `tmp/` | Short-lived working files; may be cleaned up at any time |
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

When an agent needs isolated file changes, `CreateWorktree` creates or safely
reuses a runtime-managed linked worktree from an explicit `branch` and
`base_ref`:

- The worktree has a separate `execution_root` from the canonical workspace.
- Switching away retains the worktree artifact.
- `RemoveWorktree` performs clean-only removal and optional
  merge-proven branch deletion.
- Worktrees use git worktrees on the host-local filesystem; they are not
  containerized sandboxes.

## Execution snapshot (`ExecutionSnapshot`)

The `ExecutionSnapshot` in `AgentSummary` captures the current execution
context:

- Execution profile and policy snapshot (backend, process-execution,
  background-task, and managed-worktree flags)
- Attached workspaces and registered execution roots
- Active workspace id, anchor, execution root, execution root id, and cwd
- Projection kind and access mode
- Worktree root when the execution root is a worktree

The surrounding `AgentSummary` also reports the active run id
(`agent.current_run_id`), the active workspace occupancy, and the worktree
session.

## Host-local policy

Holon's current execution model is **host-local**: processes run on the host
filesystem with the agent user's permissions. Key constraints:

- `cwd` is always within the execution root.
- Process execution is not containerized or sandboxed by the runtime.
- Network access is not confined by default.
- The `execution_environment` summary in model context describes the current
  policy snapshot as a transparency contract, not a hard sandbox guarantee.

## File references and output delivery contract

Holon defines an explicit contract for output delivery and cross-surface file
references:

### Self-contained output delivery

Agent delivery is brief-centric. Final briefs and assistant messages must be
self-contained: an operator should understand outcomes, verification status,
risks, and required actions without digging into intermediate tool logs or
opening referenced files. File references serve as entry points to supporting
artifacts, not substitutes for the result summary itself.

### Surface-specific file reference formats

The appropriate reference form depends on the output surface where it appears:

- **Project Markdown (same execution root)**: Use document-relative paths
  (e.g., `./sub/doc.md` or `../sibling.md`).
- **Local records crossing roots**: Use confirmed execution-host absolute paths
  (e.g., `/home/user/...`).
- **Briefs and assistant Markdown**: Use confirmed execution-host absolute paths
  for Markdown links and inline code paths. Do not invent machine-specific paths
  when location metadata is unconfirmed.
- **Public channels or shared documentation**: Prefer portable relative paths
  or published URLs; avoid leaking machine-specific host paths.
- **Legacy `workspace://` URIs**: Supported for backward compatibility across
  resolvers and Web GUI previews (`workspace://<workspace_id>/<path>?root=<execution_root_id>`),
  but deprecated as the default format for newly generated agent output.

### Location resolution

The runtime exposes `POST /api/file-references/resolve` to resolve absolute paths,
legacy workspace URIs, and relative paths (with explicit `base_file`) against
registered workspaces and execution roots. It deduplicates file roots by filesystem
anchor with canonical workspace priority so references remain unambiguous across
worktrees.

## Known gaps

- Runtime task-owned worktree cleanup and agent-owned explicit cleanup still
  use separate orchestration paths.
- Workspace occupancy is advisory; the runtime does not enforce exclusive
  write access at the filesystem level.
- Managed worktrees require a git workspace and are created with
  `git worktree add`; the runtime has no isolated-workspace path for non-git
  directories.
