---
title: RFC: Agent Workspace Tool Surface
date: 2026-07-18
status: accepted
issue:
  - 1224
---

# RFC: Agent Workspace Tool Surface

## Summary

Holon exposes workspace lifecycle through explicit binding, activation, and
worktree-artifact operations while preserving the invariant that every agent
always has one active workspace.

The model-facing tool family is:

- `GetWorkspaceState`
- `AttachWorkspace`
- `DetachWorkspace`
- `SwitchWorkspace`
- `CreateWorktree`
- `RemoveWorktree`

`UseWorkspace` remains as a compatibility alias during migration. New
workflows should not use it to attach repositories or create isolated roots.

## Lifecycle Model

Workspace state has three independent layers:

1. **Binding**: which workspace identities the agent may use.
2. **Active projection**: the workspace, execution root, and `cwd` currently
   used by file and command tools.
3. **Worktree artifact**: a durable Git worktree identity with provenance,
   branch metadata, lifecycle state, and cleanup evidence.

Switching projections does not detach bindings or remove artifacts. Detaching
a binding does not remove artifacts. Removing an artifact does not forget the
workspace identity.

`agent_home` is the permanent fallback binding and active projection. It cannot
be detached.

## Read Surface

`GetWorkspaceState` returns attached bindings, the active projection,
registered execution roots, worktree provenance and lifecycle metadata, and
live occupancy and Git cleanup evidence when available.

The result is derived from durable runtime state and refreshed against live Git
state. Retained and removed artifacts remain inspectable.

## Binding Surface

`AttachWorkspace { path }` discovers the stable workspace anchor and creates an
agent binding without changing the active projection.

For Git repositories, a repository subdirectory is normalized to its worktree
root. A linked worktree is normalized to the canonical repository identity;
the linked root remains a projection of that workspace rather than becoming a
new workspace.

`DetachWorkspace { workspace_id }` removes one agent-local binding and does not
delete directories, branches, or worktrees. If the target is active, the
runtime first switches atomically to `agent_home`. If that switch fails, the
binding is unchanged. Retained artifacts remain durably discoverable.

`agent_home` returns `protected_workspace`.

## Activation Surface

`SwitchWorkspace` activates an existing target:

```text
SwitchWorkspace {
  workspace_id?: string,
  execution_root_id?: string,
  path?: string,
  cwd?: string
}
```

Exactly one selector is required.

- `workspace_id` activates the canonical root of an attached workspace.
- `execution_root_id` activates a registered, non-removed projection.
- `path` discovers an existing canonical or linked-worktree projection, but
  does not attach a new repository.
- `cwd` must remain inside the resolved execution root.

Repository subdirectories resolve to the worktree root while retaining the
requested subdirectory as the default `cwd`. Linked worktrees resolve through
the shared Git common directory to an already attached origin workspace.

`SwitchWorkspace` never creates or removes a worktree. Repeating the active
target is an idempotent no-op.

## Worktree Creation

`CreateWorktree` creates or safely reuses a linked worktree:

```text
CreateWorktree {
  workspace_id: string,
  branch: string,
  base_ref: string,
  label?: string,
  activate?: bool = true,
  on_existing?: "reuse" | "error" = "reuse"
}
```

Before mutation, the runtime resolves `base_ref` to a commit and records both
the requested ref and resolved commit.

When exactly one live linked worktree already checks out `branch`,
`on_existing="reuse"` registers or refreshes that artifact and may activate it.
Reuse never applies `base_ref`, resets, forces, moves, or recreates the branch.
The result explicitly reports `disposition="reused"`, branch tip, worktree
identity, and `base_ref_applied=false`.

Branch-only existence, canonical-checkout occupancy, multiple candidates,
missing backing paths, and identity mismatches return structured conflicts
without changing Git or active state.

New worktrees use runtime-generated paths. `label` is only a readable naming
hint. Creation and activation are separate internal transitions; activation
failure retains the created artifact for later inspection or switching.

## Worktree Removal

`RemoveWorktree` removes a registered linked worktree:

```text
RemoveWorktree {
  execution_root_id: string,
  return_to?: "canonical" | "agent_home" | execution_root_id,
  branch_policy?: "keep" | "delete_if_merged" = "keep",
  merged_into?: git_ref
}
```

The runtime validates the durable artifact generation against live Git
common-directory, worktree git-directory, path, branch, and checkout state. It
also requires explicit artifact authorization for the calling agent. It
refuses canonical roots, arbitrary path selectors, force removal, dirty
worktrees, locked worktrees, and roots with any additional active occupancy.

An active target requires `return_to`. All read-only identity, occupancy,
dirty-state, and reachability checks complete before the switch. After the
switch, the shared runtime registry atomically grants a cleanup lease only
when no occupancy remains. The lease blocks new occupancy until Git removal
and the execution-root tombstone are complete.

`branch_policy="keep"` removes only the worktree. `delete_if_merged` deletes
the branch only after proving its tip is an ancestor of `merged_into`.
Detached HEAD removal requires an equivalent reachability proof.

Runtime-created and shell-created worktrees use the same explicit safety
checks. Provenance affects defaults and automatic recovery, not whether an
explicitly registered artifact may be inspected or safely removed.

Successful removal soft-deletes the execution-root entry and records structured
audit evidence. Dirty or unverifiable targets are retained with changed-file or
inspection-failure evidence. Re-creating a worktree at a previously removed path
allocates a new execution-root generation; old artifact IDs remain tombstones.

## Compatibility

`UseWorkspace` remains dispatchable while callers migrate:

- it is hidden from the model-facing catalog;
- historical direct selection and path-based implicit attach remain
  dispatch-compatible;
- historical `mode="isolated"` and `isolation_label` remain
  dispatch-compatible;
- all compatibility results direct callers to `AttachWorkspace`,
  `SwitchWorkspace`, or `CreateWorktree`.

The compatibility result includes a deprecation summary. No new workflow or
prompt guidance should introduce `UseWorkspace`.

CLI `workspace exit` means switch to `agent_home`. CLI `workspace detach`
shares the binding transition with `DetachWorkspace`. Neither operation removes
worktrees.

## Audit Contract

Workspace operations record structured events for binding attach/detach,
projection switch, worktree create/reuse, cleanup retained/removed/failed,
branch retention/deletion, and compatibility alias use.

Audit payloads include workspace and execution-root identities and do not use
untrusted path input as an authorization decision.

