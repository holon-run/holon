---
title: RFC: Execution Policy and Virtual Execution Boundary
date: 2026-04-21
status: draft
issue:
  - 77
---

# RFC: Execution Policy and Virtual Execution Boundary

## Summary

Holon should treat execution authority as three related layers:

- `resource authority`: which resource classes matter
- `execution policy`: which runtime states may use those resources
- `virtual execution environment`: which of those rules Holon can actually
  enforce through the execution backend

The current implementation is phase-1 `host_local`. This RFC makes that
boundary explicit instead of overstating isolation.

## Why

Holon already exposes local file and process operations, but it does not yet
provide strong backend-mediated confinement for filesystem, network, secrets, or
child processes. The contract should describe that honestly.

## Phase-1 Backend Contract

The only implemented backend is:

- `host_local`

`host_local` means:

- execution happens on the operator's local machine
- workspace projection and runtime admission are explicit
- some surfaces can be hidden or denied by runtime policy
- stronger process or filesystem isolation is not yet guaranteed

## Resource Classes

Phase-1 policy work should treat at least these as first-class resource
classes:

- process execution
- filesystem mutation
- filesystem read scope
- network access
- secrets / ambient credentials
- managed execution tasks
- managed worktree creation

## Current Honest Guarantee

Today Holon can be explicit about:

- whether process execution is exposed
- whether background tasks are available
- whether managed worktree projection or artifact creation is available
- which workspace projection and access mode are active

Today Holon cannot honestly claim full confinement for:

- arbitrary filesystem reads and writes
- child-process network isolation
- secret exfiltration prevention
- backend-neutral path virtualization

## Relationship To Workspace Model

Execution policy should use the workspace RFC vocabulary:

- `workspace_anchor`
- `execution_root`
- `projection_kind`
- `access_mode`

This keeps execution policy aligned with runtime state that Holon already
understands.

Policy should distinguish worktree authority from worktree lifecycle
ownership.

Creating a worktree projection or task-owned worktree artifact is an execution
boundary capability. Destroying a task-created worktree should be governed by
the task or artifact lifecycle, not by generic workspace switching.

## Future Direction

Future backends may include:

- copied local execution
- container-backed execution
- ssh / remote execution

Those stronger backends should be introduced as new enforceable contracts, not
retrofitted into the semantics of `host_local`.

## Related Historical Notes

Supersedes and absorbs:

- `docs/archive/execution-policy-and-venv-boundary.md`
- `docs/archive/virtual-execution-environment.md`
