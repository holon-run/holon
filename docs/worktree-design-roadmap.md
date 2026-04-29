# Holon Worktree Design Roadmap

This document defines how `Holon` should add worktree support.

The design follows the parts of Claude Code that are worth borrowing, while
staying consistent with `Holon`'s current runtime shape.

The goal is not:

- "let the model happen to run `git worktree` through shell"

The goal is:

- make worktree isolation a real runtime concept
- make it safe enough to use for iterative development
- make it inspectable and disposable when results are poor

## Why This Should Exist

`Holon` is already able to:

- run coding tasks
- spawn bounded background subtasks
- keep per-session state
- expose shell and file tools

But it cannot yet treat isolated working copies as first-class runtime units.

Without a formal worktree abstraction, any "parallel development" mode would
depend on:

- prompt discipline
- shell command luck
- ad hoc cleanup

That is not strong enough for a repeatable development workflow.

## Design Principle

Worktree support should be added in three layers.

Those layers map closely to how Claude Code handles the problem:

1. explicit session-level worktree entry/exit
2. task/subagent-level worktree isolation
3. orchestration for parallel development across multiple worktrees

`Holon` should build them in that order.

## Layer 1: Session-Level Worktree Tools

### Goal

Allow a running session to explicitly enter and exit a worktree.

### Why

This gives `Holon` a simple, inspectable baseline:

- create a worktree
- move the current session into it
- do work there
- exit it without deleting artifacts; cleanup belongs to the artifact owner

### Proposed Tools

- `EnterWorktree`
- `ExitWorktree`

### Proposed Semantics

`EnterWorktree`

- only valid in a git repository
- creates a new worktree rooted under a managed directory
- switches the current session `workspace_root` to that worktree
- records:
  - original cwd
  - original branch
  - worktree path
  - worktree branch

`ExitWorktree`

- only operates on a worktree created by `EnterWorktree` in the current session
- supports:
  - `action = "keep"`
  - `action = "remove"`
- refuses destructive removal when there are changes unless explicitly forced

### Runtime Additions

- persist `worktree_session` inside session state
- expose current worktree metadata in prompt/context
- include worktree metadata in audit and brief records when relevant

### Definition Of Done

- a session can enter a worktree and keep working normally
- a session can exit and return to the original workspace
- a worktree created by the session can be kept or removed explicitly
- this state survives resume when the worktree still exists

## Layer 2: Worktree-Isolated Subagent Tasks

### Goal

Let background subtasks run inside isolated worktrees without changing the
parent session cwd.

### Why

This is the most important worktree feature for real coding workflows.

It enables:

- isolated experiments
- parallel implementation attempts
- safer subagent execution

without forcing the primary session to leave its own workspace.

### Task Representation

- `child_agent_task` with `workspace_mode = worktree`

### Proposed CreateTask Shape

Example conceptual input:

```json
{
  "kind": "child_agent_task",
  "workspace_mode": "worktree",
  "summary": "Implement bounded synthesis metrics export",
  "prompt": "Add benchmark metrics for total token usage and per-tool latency.",
  "branch_name": "holon-metrics-export",
  "keep_on_success": true
}
```

### Proposed Semantics

- create a dedicated worktree for the task
- run the subagent inside that worktree
- if there are no changes, remove the task-owned worktree and ephemeral branch
  automatically during task cleanup
- if there are changes, return:
  - `worktree_path`
  - `worktree_branch`
  - changed files summary
  - cleanup status
  - task result summary

### Important Boundary

The parent session should not automatically merge anything.

The result of a worktree task is:

- an inspectable isolated artifact

not:

- an automatic integration decision

### Definition Of Done

- a subagent task can run in an isolated worktree
- parent session receives task status/result with worktree metadata
- unchanged worktrees are cleaned up
- changed worktrees are preserved for inspection

## Layer 3: Parallel Worktree Development Orchestration

### Goal

Allow `Holon` to coordinate multiple isolated development attempts in parallel.

### Why

This is the workflow the user ultimately wants:

- split a larger task into smaller parts
- run each part in its own worktree
- review results
- keep the good ones
- remove unwanted retained artifacts with ordinary git commands

### Proposed Runtime Shape

The top-level session remains the coordinator.

It should be able to:

- create multiple worktree-mode `child_agent_task`s
- track their worktree paths and status
- summarize their results
- tell the operator which worktrees are worth reviewing

### Explicit Non-Goal

This layer should not auto-merge into `main`.

The first production-worthy version should stop at:

- create isolated attempts
- surface clear summaries
- let the operator review and decide

### Possible Future Extensions

- generate patch or diff summaries
- score candidate attempts by verification success
- later, optionally add explicit merge/cherry-pick tools

### Definition Of Done

- one session can coordinate multiple worktree-isolated subtasks
- each task is independently inspectable
- failed or poor attempts can be retained without blocking task cleanup
- good attempts can be reviewed and integrated by the operator

## Recommended Implementation Order

Implement in this order:

1. `EnterWorktree` / `ExitWorktree`
2. `child_agent_task` with `workspace_mode = worktree`
3. coordinator workflow for multiple worktree tasks

Do not start with layer 3.

Layer 3 depends on the lifecycle guarantees established by layers 1 and 2.

## How This Fits Holon's Broader Roadmap

This work belongs primarily inside:

- `S3` coding loop hardening
- `S4` structural refactor without semantic rewrite
- `S5` tool surface decision

from `docs/semantic-vs-structural-roadmap.md`.

More specifically:

- Layer 1 strengthens coding-loop control
- Layer 2 introduces a higher-value tool/task abstraction
- Layer 3 becomes the first serious "Holon helps write real code in parallel"
  workflow

## Current Recommendation

The current recommendation is:

- do not rely only on prompt instructions to use `git worktree`
- add worktree support as explicit runtime primitives
- stop short of automatic merging in the first version
- treat preserved worktrees as reviewable development artifacts

That gives `Holon` a path to real parallel development without pretending that
integration decisions are already solved.
