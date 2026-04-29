# Command Execution And Task Model

This document defines the next-step design for command execution in Holon after
the tool-surface cutover to `exec_command`.

The goal is to keep the public command tool simple while making long-running
processes manageable inside the runtime.

## Problem

Holon currently has two tensions:

- `exec_command` should stay simple and aligned with Codex-style command
  execution.
- long-running commands such as dev servers, watchers, and long tests still
  need:
  - lifecycle tracking
  - status inspection
  - termination
  - possible follow-up continuation

Putting both modes into a single `background=true` flag makes the command tool
carry two different semantics at once:

- foreground synchronous execution
- background orchestrated task execution

That makes policy, transcript, stopping, and task ownership harder to reason
about.

## Design Principles

1. `exec_command` stays the foreground primitive.
2. Background work is modeled as a task, not as a command flag.
3. The runtime owns process lifecycle tracking.
4. The agent should not need to manually manage raw process ids.
5. Only tasks that have orchestration value should re-enter the main loop.

## Recommended Model

### 1. `exec_command`

`exec_command` is the public tool for:

- short verification commands
- diagnostics
- one-shot shell interactions

Its default contract should remain:

- start a command
- wait up to `yield_time_ms`
- if the process exits, return normal result output

It should not expose `background=true`.

### 2. `command_task`

Long-running non-interactive commands should be represented as:

- managed `command_task` records

This task kind is the runtime-level abstraction for:

- dev servers
- file watchers
- long-running test commands
- log-following helpers

Public creation paths:

- `exec_command` can promote long-running commands into managed `command_task`
- operator control surfaces may create managed `command_task` directly
- short waits should use `Sleep(duration_ms)` instead of a separate wait task

Suggested payload:

- `cmd`
- `workdir`
- `shell`
- `login`
- `tty`
- `yield_time_ms`
- `max_output_tokens`
- `summary`
- optional `continue_on_result`

### 3. Automatic Promotion

Holon should support this behavior for non-interactive commands:

1. `exec_command` starts the command.
2. If the process exits before `yield_time_ms`, return a normal result.
3. If the process is still running after `yield_time_ms`, the runtime promotes
   it into a `command_task`.
4. The tool result returns:
   - `task_id`
   - task status
   - initial output snippet

This keeps the model simple:

- short commands remain foreground
- long commands become managed tasks automatically

This promotion should only happen for non-interactive command flows. The first
version should not try to support mid-flight promotion of interactive PTY
sessions.

## Why Not `background=true`

`background=true` is attractive as a shortcut, but it has poor boundaries:

- it mixes process execution and orchestration in one tool contract
- it encourages the model to spawn long-lived processes without task metadata
- it makes stop/cleanup semantics awkward
- it creates a second background model next to managed `command_task`

Holon should avoid having both:

- background `exec_command`
- task-based background execution

The task model should be the only background model.

## Task Lifecycle

`command_task` should use the same task lifecycle as other runtime tasks:

- `queued`
- `running`
- `completed`
- `failed`
- `cancelled`

Runtime responsibilities:

- persist task records
- keep command ownership under the session
- emit `TaskStatus` and `TaskResult`
- support `TaskStop`
- record final exit status and output summary

## Agent Continuation

Task completion should not always wake the agent into another full reasoning
loop.

Recommended rule:

- `command_task`: default no automatic continuation
- `child_agent_task`: continue by default, with `workspace_mode` preserving
  inherited versus worktree-isolated delegation

If a `command_task` is specifically created to support the next reasoning step,
its metadata may set:

- `continue_on_result: true`

In that case the runtime may enqueue a follow-up after the `TaskResult`.

## Future Interactive Extension

If Holon later needs interactive shell sessions, that should be a separate
layer, not part of the first `command_task` implementation.

Two acceptable future directions:

1. Codex-style:
   - `exec_command` returns a session handle
   - `write_stdin` continues interaction

2. Task-wrapped interaction:
   - `command_task` owns an interactive session internally
   - later tools operate on `task_id` rather than a raw `session_id`

For Holon, the second option is likely more consistent with its task-oriented
runtime design, but it should come after the non-interactive `command_task`
path is stable.

## Recommended Implementation Order

1. Add `command_task` to the task model.
2. Add runtime execution for non-interactive command tasks.
3. Make `exec_command` auto-promote long-running non-interactive commands into
   `command_task`.
4. Extend `TaskStatus` and `TaskList` with command-task status and output summary.
5. Make `TaskStop` terminate `command_task` cleanly.
6. Re-evaluate whether interactive command sessions are still needed.

## Non-Goals For The First Pass

- no interactive PTY continuation
- no `write_stdin`
- no raw process-id exposure in the public contract
- no second background command model

## Summary

Holon should use:

- `exec_command` for foreground command execution
- `command_task` for managed background command execution

This keeps the public command tool simple, keeps background work visible and
stoppable, and aligns Holon more closely with Claude-style task orchestration
without giving up Codex-style command ergonomics.
