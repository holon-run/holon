---
title: RFC: Interactive Command Continuation
date: 2026-04-21
status: draft
---

# RFC: Interactive Command Continuation

## Summary

This RFC proposes how Holon should support interactive terminal commands
without introducing a second background-command model.

The key direction is:

- `ExecCommand` remains the public entry point for command startup
- long-running interactive execution remains owned by the runtime
- continuation should be task-oriented rather than raw-session-oriented

## Problem

Holon already supports:

- foreground `ExecCommand`
- automatic promotion of long-running non-interactive commands into
  `command_task`

But it does not yet support interactive continuation such as:

- commands that require a TTY
- REPL-like flows
- programs that prompt after startup

Without a continuation model, `tty=true` is only partially useful. Holon can
allocate a terminal-like environment, but the agent cannot reliably keep
driving the process after the initial call.

At the same time, Holon should avoid reintroducing:

- `background=true` on `ExecCommand`
- raw process id exposure
- a second, parallel session protocol next to the runtime task model

## Goals

- support interactive command execution in a Holon-native way
- keep `ExecCommand` as the startup primitive
- keep runtime-owned lifecycle management
- avoid raw process or raw PTY handle exposure
- preserve stop, output, and audit semantics under the task model

## Non-goals

- do not expose raw operating-system process ids
- do not make the model manage PTY details directly
- do not adopt a separate unmanaged shell-session surface
- do not require all command tasks to become interactive

## Proposed Model

## 1. Startup Still Uses `ExecCommand`

The agent starts command execution with `ExecCommand`, including
`tty=true` when appropriate.

The first call should still behave like normal command startup:

1. start command
2. collect output until exit or `yield_time_ms`
3. if the command exits quickly, return a normal command result

## 2. Long Interactive Execution Becomes Managed Runtime State

If the command is still running and interactive continuation is needed, the
runtime should attach the execution to managed command-task state.

The returned handle should be:

- runtime-owned
- task-addressed
- auditable
- stoppable

The public continuation target should be the task identity, not a raw session
identifier.

## 3. Continuation Should Be Task-Oriented

The continuation model should look conceptually like:

- start with `ExecCommand`
- continue by referencing the managed command task

This implies a future continuation tool family such as:

- send input to a managed command task
- read incremental output from a managed command task
- stop the managed command task

The preferred candidate naming direction is:

- `TaskInput`
- `TaskOutput`
- `TaskStop`

with `TaskInput` as the likely input-side continuation primitive for managed
interactive command execution.

In the first implemented version, `TaskInput` should deliver structured text
input only to a managed command task that was explicitly created with stdin
continuation enabled. Rejections that reflect current task state should remain
structured `TaskInput` receipts instead of degrading into transport errors.
PTY-native continuation can layer onto the same task-oriented surface later
without changing the ownership model.

The exact final names can still be adjusted, but the important design choice is
that the continuation surface belongs to the command/task plane rather than
exposing a separate raw-session protocol.

## Why Task-Oriented Continuation Fits Holon Better

Holon already has runtime concepts for:

- persistence
- stop semantics
- task result delivery
- recovery decisions
- audit events

If interactive execution is modeled as a managed command task, those properties
remain aligned with the current runtime instead of creating a second ownership
model.

This is more consistent with Holon than:

- `ExecCommand` returning a raw session id
- a separate `write_stdin(session_id)` protocol that bypasses task ownership

## Task Ownership Rules

Interactive command state should remain runtime-owned.

That means:

- the runtime decides whether a command is still active
- the runtime decides when terminal state is persisted
- the runtime remains responsible for cleanup
- stop behavior still flows through task stop semantics

The model should not need to reason about:

- process groups
- PTY teardown details
- reattachment primitives

## Relationship To Existing Command Model

This RFC extends but does not replace the current command execution direction.

The intended relationship is:

- non-interactive short commands: synchronous `ExecCommand`
- non-interactive long commands: auto-promoted managed command task
- interactive commands: start with `ExecCommand`, then continue through
  managed command-task ownership

This preserves one command startup model while allowing richer continuation.

## Open Design Questions

The following questions remain open after this RFC:

- should interactive continuation reuse `TaskOutput` or have a dedicated
  incremental output surface?
- should continuation be represented by `TaskInput` as a generic task-input
  tool, or by a more command-specific input tool?
- should interactive command tasks be recoverable across restart, or only
  recoverable as terminal failure state in the first version?
- how much PTY transcript should be persisted in task detail versus separate
  output artifacts?

## Recommended Rollout

1. add PTY-capable process substrate support
2. make managed command-task state capable of owning interactive execution
3. add continuation and incremental output surfaces
4. refine prompt guidance so the model understands when `tty=true` is
   appropriate

## Summary

Holon should support interactive terminal execution by extending the command
plane, not by creating a second background-command model.

The correct direction is:

- `ExecCommand` starts
- managed command-task state owns lifecycle
- task-oriented continuation drives the interactive session
