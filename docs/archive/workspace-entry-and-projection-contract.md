# Workspace Entry And Projection Contract

## Summary

`Holon` should separate:

- attaching a workspace to an agent
- entering a concrete execution root inside that workspace

The key judgment is:

- `workspace attachment` defines which workspace entry an agent may use
- `workspace entry` defines which execution root the agent is actively running
  against
- `git worktree` should be treated as one projection strategy, not as a
  top-level runtime concept

So the runtime surface should converge on:

- `AttachWorkspace`
- `EnterWorkspace`
- `ExitWorkspace`

## Problem

Today `Holon` already distinguishes:

- `workspace_anchor`
- `execution_root`
- managed worktree state

But the current control surface is still shaped around a worktree-first entry
model. Historically that made sense for the first git-centric workflow, but it
is no longer the right primary abstraction.

That creates three problems:

1. it makes a git-specific projection look like the primary runtime concept
2. it does not provide a unified way to express canonical-root entry vs
   derived-root entry
3. it leaves occupancy and access mode under-specified

This is increasingly awkward because:

- not every workspace is a git repository
- canonical-root execution and worktree execution are both valid
- occupancy should be expressed at workspace-entry time, not as an implicit
  side effect

## Scope

This RFC defines:

- the distinction between workspace attachment and workspace entry
- explicit `AttachWorkspace` plus unified `EnterWorkspace` / `ExitWorkspace`
  runtime surfaces
- projection kinds
- access modes
- the minimal occupancy model for phase 1

This RFC does not define:

- full sandbox behavior
- final execution profiles
- copied workspace semantics
- lock heartbeats or TTLs

## Two Distinct Operations

`Holon` should distinguish two operations clearly.

### 1. Attach Workspace

This means:

- the agent is allowed to reference a `workspace_entry`
- the runtime may bind a stable `workspace_anchor`
- instructions and workspace-local skills can resolve against that anchor

This does not yet mean:

- the agent is occupying an execution root
- the agent is running inside a worktree
- the agent has exclusive write access

### 2. Enter Workspace

This means:

- the agent selects an active execution root
- the runtime binds tool/file/process surfaces to that root
- the runtime records access mode and occupancy

This is the operation that should replace the old worktree-first top-level
concept.

## Core Model

The phase-1 entry model should include:

- `workspace_entry`
  - the host-owned attached workspace object
- `workspace_anchor`
  - the canonical root of that workspace entry
- `execution_root`
  - the actual root used for current file/process execution
- `projection`
  - how the execution root is derived from the workspace anchor
- `access_mode`
  - the intended occupancy and mutation model for the active execution root

## Projection Kinds

Phase 1 should support:

- `canonical_root`
  - the execution root is the workspace's canonical root
- `git_worktree_root`
  - the execution root is a git worktree derived from the canonical root

Future projections may include:

- `copied_root`
- `snapshotted_root`

But they should not be required for phase 1.

## Access Modes

Phase 1 should keep access modes small:

- `shared_read`
  - concurrent read-oriented use is allowed
- `exclusive_write`
  - the agent is the single coordinated writer for that execution root

This model is intentionally coarse.

It does not try to infer exact shell read/write behavior.
It only expresses runtime occupancy and intended mutation semantics.

## Canonical Root And Worktree Root

The core runtime rule should be:

- `canonical_root` is the default attached execution root
- `git_worktree_root` is the preferred derived execution root for isolated or
  parallel mutation work

In practice this means:

- multiple agents may read the same `canonical_root`
- `canonical_root` should not be treated as a concurrent mutation surface
- `git_worktree_root` is the natural per-agent mutation surface for git-backed
  workspaces

## EnterWorkspace

`EnterWorkspace` should be the unified entry surface.

Conceptually it should accept:

- `workspace`
- `projection`
- `access_mode`
- optional projection parameters
  - such as `branch_name`
  - or future copy/snapshot options

Examples:

- `EnterWorkspace(workspace=holon, projection=canonical_root, access_mode=shared_read)`
- `EnterWorkspace(workspace=holon, projection=canonical_root, access_mode=exclusive_write)`
- `EnterWorkspace(workspace=holon, projection=git_worktree_root, access_mode=exclusive_write, branch_name=fix-pr-123)`

The exact tool schema may evolve, but this semantic shape should remain stable.

## ExitWorkspace

`ExitWorkspace` should be the unified exit surface.

It should:

- leave the current execution root
- release any occupancy associated with that root
- optionally preserve or discard derived artifacts depending on projection type

For example:

- leaving `canonical_root` mainly releases occupancy
- leaving `git_worktree_root` may preserve or discard the worktree artifact

## Occupancy

Phase 1 should introduce a minimal occupancy model.

This does not need to be a full lock manager.

It only needs to make one important runtime fact explicit:

- write coordination is explicit per `execution_root`

The minimal model should support:

- `execution_root_id`
- `holder_agent_id`
- `access_mode`
- `acquired_at`
- optional TTL / heartbeat metadata

Phase-1 expectations:

- `shared_read` on `canonical_root` may be concurrent
- `exclusive_write` on a root conflicts only with another `exclusive_write`
- `exclusive_write` does not block `shared_read`
- `git_worktree_root` is usually agent-owned by construction, but review readers
  may still enter it with `shared_read`

## Why This Should Replace Worktree-First Entry

But the more stable runtime abstraction is:

- attach a workspace
- enter a workspace
- choose a projection
- choose an access mode

This scales better because:

- non-git workspaces still have a valid entry model
- git-backed workspaces can opt into derived roots
- future copied/snapshotted roots fit naturally
- occupancy is expressed at the right layer

## Relationship To Existing RFCs

This RFC refines and composes with:

- `workspace-binding-and-instruction-loading.md`
- `execution-policy-and-venv-boundary.md`

In particular:

- workspace binding defines what an agent may attach
- execution-policy / v env boundary defines backend and projection concepts
- this RFC defines the runtime control surface for entering those projections

## Phase-1 Implications

If `Holon` adopts this contract, phase 1 should move toward:

- `AttachWorkspace`
- `EnterWorkspace`
- `ExitWorkspace`
- explicit projection state in runtime-visible summaries
- explicit occupancy on entered execution roots

And away from:

- treating git worktree as the primary top-level abstraction

## Open Questions

Phase 1 decisions:

1. workspace entry requires a prior attach step
2. `canonical_root + exclusive_write` is allowed when explicitly requested
3. delegated agents inherit an explicit active-entry snapshot
4. phase 1 includes a first-class persisted occupancy table

## Decision

`Holon` should replace worktree-first entry semantics with a more general
workspace-entry contract.

The stable abstraction should be:

- attach workspace
- enter workspace with a projection
- run against the resulting execution root
- exit workspace and release occupancy
