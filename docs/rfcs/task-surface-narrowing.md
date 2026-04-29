---
title: RFC: Task Surface Narrowing
date: 2026-04-21
status: draft
---

# RFC: Task Surface Narrowing

## Summary

This RFC proposes how Holon should narrow the public `Task` surface over time.

The central direction is:

- keep internal runtime execution records flexible
- make the public `Task` surface more specific and less overloaded
- move delegation and waiting away from `CreateTask` as long-term public
  creation semantics

In practical terms, this means Holon should gradually make public task control
mean "managed execution job, primarily command-backed execution" rather than
"any asynchronous thing the runtime can do."

`Task` is therefore an execution receipt produced by operation verbs such as
`ExecCommand` and `SpawnAgent`, not an independently creatable control-plane
object.

## Problem

Today `CreateTask` mixes several different kinds:

- `sleep_job`
- `child_agent_task`
- `command_task`

These are not peers at the same conceptual layer.

For example:

- `command_task` is about managed command execution
- `child_agent_task` is about supervised child context creation
- `sleep_job` is about delayed reactivation

When they all live under one public task entry point, several things happen:

- `Task` becomes too broad to guide the model well
- task prompt guidance has to explain multiple different mental models at once
- `CreateTask` schema becomes flat while true requirements are variant-shaped
- future tool evolution risks preserving historical names instead of clearer
  runtime semantics

## Goals

- narrow the public meaning of task-oriented tools
- reduce overload in `CreateTask`
- make `TaskList` / `TaskStatus` / `TaskOutput` / `TaskStop` center on the cases
- make `TaskList` / `TaskStatus` / `TaskOutput` / `TaskStop` center on the
  cases
  where task control is most natural
- separate task semantics from agent-plane and waiting-plane semantics

## Non-goals

- do not forbid the runtime from internally tracking non-command jobs
- do not force immediate removal of current task kinds
- do not require every migration step to be breaking

## Core Judgment

Public task semantics should be narrower than internal runtime job semantics.

That means Holon can still keep a general internal execution record if useful
for:

- persistence
- recovery
- cancellation
- audit

But the model-facing task plane should not remain the default home for every
runtime-owned asynchronous capability.

## Why Command Execution Is The Best Center For Task Control

The current `Task*` family is most natural when applied to managed command
execution:

- list running background execution
- inspect one execution handle
- fetch output
- stop the job

These are exactly the questions a command-backed task needs to answer.

They are much less natural as the primary abstraction for:

- child-agent context creation
- timer-backed waiting
- callback-backed waiting

This suggests Holon should let `command_task` become the center of gravity for
public task semantics.

## Proposed Public Direction

The intended public direction is:

- `ExecCommand` starts command execution and returns a task handle when
  execution becomes managed
- long-running command execution becomes managed task state
- `TaskList` / `TaskStatus` / `TaskOutput` / `TaskStop` primarily inspect and
  control that execution
- `TaskList` / `TaskStatus` / `TaskOutput` / `TaskStop` / `TaskInput`
  primarily inspect and control that managed execution

Meanwhile:

- child context creation moves toward the agent plane
- waiting moves toward the waiting plane

This also leaves room for one important supervised-child pattern:

- `SpawnAgent` remains an agent-plane operation
- the runtime may return a task handle as a side effect when the spawned child
  is a bounded parent-supervised execution

That handle does not make the child context itself a task.

It only means the task plane may supervise some child-agent executions when
they intentionally share the same operational semantics as other managed
execution handles.

The shared model-visible receipt shape is `TaskHandle`:

```rust
pub struct TaskHandle {
    pub task_id: String,
    pub task_kind: String,
    pub status: TaskStatus,
    pub initial_output: Option<String>,
}
```

Both `ExecCommand` promotion and `SpawnAgent(private_child, ...)` should return
this shape under the `task_handle` field. For `ExecCommand`, the model-visible
receipt is typed by the `disposition` discriminant: direct completion uses
`disposition = completed`, while promotion uses
`disposition = promoted_to_task` and guarantees `task_handle` is present.
`TaskStatus`, `TaskOutput`, `TaskStop`, and `TaskInput` still accept the
contained `task_id`; the wrapper exists to make the returned execution receipt
self-describing.

This does not require `Task` to become shell-only as an internal truth. It
only requires the public surface to stop pretending that all asynchronous
runtime actions should be created the same way.

## Task Handle Versus Context Owner

This RFC depends on one hard boundary:

- `Agent` owns context
- `Task` owns a managed execution handle

That means:

- `SpawnAgent` creates an `Agent`
- `ExecCommand` starts managed command execution
- either operation may produce a task handle when the runtime wants to expose
  bounded supervision through the task plane

The task handle is therefore not the child agent itself.

It is a parent-visible supervision capability for a managed execution unit.

## `TaskStatus` Instead Of `TaskGet`

Holon should prefer `TaskStatus` over a heavyweight generic `TaskGet`.

The reason is that task control needs a compact lifecycle snapshot more than it
needs an overloaded detail object.

`TaskStatus` should answer questions such as:

- is this task still running?
- is it terminal?
- can it accept input?
- can it be stopped?
- is output available?

This keeps the task plane readable across both:

- command-backed execution
- supervised child-agent handles

Agent-specific detail should not be forced into a task-detail object.

That should live under `AgentGet` instead.

## `TaskInput` As A Generic Supervision Input Surface

`TaskInput` should remain a generic input tool for interactive managed
execution.

That means:

- for command tasks, `TaskInput` writes stdin or PTY input
- for supervised child-agent tasks, `TaskInput` sends parent follow-up input

This is more consistent than splitting interactive task input into separate
command-only and agent-only surfaces while the operational semantics are still
the same from the parent's point of view.

## `CreateTask` As A Transitional Surface

`CreateTask` should be treated as transitional and eventually split by plane.

It can remain useful while Holon is migrating, but its long-term problems are
already clear:

- one flat object does not express variant boundaries well
- different task kinds imply different ownership models
- schema precision is weaker than the real semantics

The medium-term direction should be:

- reduce what new work goes into `CreateTask`
- migrate delegation and waiting semantics elsewhere
- leave command-backed execution as the strongest fit for the remaining public
  task plane

## Supervised Child Handles And Worktree Artifacts

`Task` should not remain the public creation word for child agents, but the
task plane may still expose a supervision handle returned as a side effect of
`SpawnAgent(private_child, ...)`.

When that child is spawned with `workspace_mode=worktree`, the runtime-created
worktree should be treated as task-owned artifact state:

- the child agent uses the worktree as its active execution projection while it
  runs
- the supervising task records the worktree path, branch, and cleanup state
- completion of the child turn does not automatically transfer ownership to the
  child agent
- cleanup responsibility belongs to task cleanup or later artifact GC

This means `worktree_subagent_task` should not remain a runtime-created task
kind. Holon now uses a unified `child_agent_task` plus
`workspace_mode=worktree` metadata for supervised worktree-isolated child
delegation.

Holon no longer exposes a dedicated destructive worktree-discard public tool.
Manual early cleanup can use ordinary git commands inside the local
environment. Runtime-owned cleanup is best-effort and tied to task cleanup or
artifact GC.

Task-owned worktree cleanup should be tolerant of operator or agent manual
cleanup:

- if the worktree was already removed, cleanup should treat it as already
  cleared
- if branch or path state no longer matches, cleanup should record an audit
  event and avoid blocking task cleanup indefinitely
- runtime-created task branches may be deleted by runtime cleanup because they
  are ephemeral artifacts

## Migration Phases

## Phase 1: Clarify Current Semantics

Without breaking the surface yet:

- describe `command_task` as the primary task-control object
- describe `child_agent_task` as the unified supervised child-agent task
- describe `subagent_task` and `worktree_subagent_task` as legacy record names
  that map to `workspace_mode=inherit` and `workspace_mode=worktree`
- describe `sleep_job` as transitional delayed-reactivation language
- describe `TaskStatus` as the preferred task metadata snapshot
- describe `TaskGet` as a removable legacy detail shape rather than the long-
  term task inspection center

## Phase 2: Narrow Task Guidance

Prompt guidance and docs should increasingly teach:

- use task control mainly for command-backed managed execution
- use `TaskInput` as the generic continuation primitive when a managed task
  explicitly needs follow-up input
- use dedicated waiting or agent-plane tools when available
- use task handles for bounded supervised execution when the runtime chooses to
  expose them
- use `TaskInput` as the generic continuation primitive when a managed task
  explicitly needs follow-up input
- use dedicated waiting or agent-plane tools when available

This reduces model confusion before public renames are complete.

## Phase 3: Split Creation Surfaces By Plane

Once replacement surfaces exist:

- agent-plane tools replace subagent task creation
- waiting-plane tools replace delayed-wait creation
- public task creation narrows toward managed execution jobs
- task inspection converges on `TaskStatus` rather than `TaskGet`
- legacy `worktree_subagent_task` records converge into unified
  `child_agent_task` metadata
- destructive worktree-discard tools stay retired in favor of task cleanup or
  ordinary git worktree management

## Phase 4: Revisit Internal Naming

Only after public migration stabilizes should Holon decide whether its
internally generalized execution record should remain named `TaskRecord` or
move to something more neutral such as `JobRecord`.

That rename should follow semantic cleanup, not lead it.

## Relationship To Tool Contract Consistency

This RFC is complementary to the contract consistency RFC.

The consistency RFC says:

- schemas should reflect real variant boundaries
- outputs should be more structured

This RFC says:

- the public task plane itself should become less overloaded

Together, they point toward:

- fewer mixed-shape public tools
- clearer plane ownership
- more predictable model behavior

## Open Questions

The following questions remain open after this RFC:

- should Holon keep a public task creation surface at all once command
  auto-promotion is strong enough, or should explicit task creation become
  rarer?
- should some non-command runtime jobs still be inspectable through `TaskList`
  for operator debugging even if they are no longer first-class model tools?
- should public task control eventually distinguish command tasks from other
  job kinds more explicitly?

## Summary

Holon should narrow the public task surface over time.

The intended end state is:

- command-backed managed execution remains the core task-control use case
- child context creation moves to the agent plane
- bounded child supervision may still use task handles
- delayed reactivation moves to the waiting plane

This gives Holon a clearer public contract without forcing premature
simplification of the internal runtime execution substrate.
