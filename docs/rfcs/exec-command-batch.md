---
title: RFC: ExecCommand Batch
date: 2026-04-26
status: draft
---

# RFC: ExecCommand Batch

## Summary

This RFC defines `ExecCommandBatch`, a model-visible command-family tool for
running multiple `ExecCommand`-like startup requests in one tool call.

V1 is deliberately narrow:

- batch only
- sequential execution
- `ExecCommand` startup semantics only
- no parallelism
- no arbitrary nested tool calls
- no new read-only promise beyond the existing execution boundary

The goal is to reduce model rounds and prompt churn during clustered command
work without changing Holon's command execution safety model.

## Problem

Benchmark runs show that Holon often performs many small shell inspections one
tool call at a time:

- search one symbol
- read one file slice
- inspect status
- search another symbol
- read another slice

This has three costs:

- extra model rounds
- repeated context re-injection
- higher latency from serial model/tool/model loops

The obvious workaround is to concatenate commands through shell separators, but
that weakens structure:

- item boundaries become ad hoc text
- per-command status is harder to inspect
- metrics cannot distinguish model-visible tool calls from actual shell
  commands
- failure handling becomes a shell-script concern instead of a runtime concern

Holon should provide a structured command-family batching primitive.

## Goals

- reduce model-visible tool calls for clustered command work
- preserve per-command boundaries in canonical and model-visible output
- reuse the existing `ExecCommand` execution boundary
- avoid adding a second command safety model
- keep benchmark metrics explicit about model calls versus shell command items

## Non-goals

- do not add parallel execution in V1
- do not implement a general `multi_tool_use` equivalent
- do not allow batching `ApplyPatch`, `Sleep`, `Enqueue`, `SpawnAgent`,
  `Task*`, `UpdateWorkItem`, or any other non-command tool
- do not introduce a new read-only command guarantee
- do not support interactive command continuation inside a batch
- do not start or manage long-running background command tasks from a batch

## Tool Name

The V1 tool name should make the boundary explicit.

Preferred name:

- `ExecCommandBatch`

Acceptable alternatives:

- `BatchExecCommand`
- `ExecBatch`

Names to avoid:

- `BatchInspect`, because V1 does not provide a read-only inspection guarantee
- `RunReadOnlyBatch`, for the same reason
- `MultiToolUse`, because V1 is not a general nested-tool batch surface

## Input Contract

`ExecCommandBatch` accepts a bounded list of command items.

Each item is a restricted `ExecCommand` startup request:

```json
{
  "items": [
    {
      "cmd": "rg -n \"ToolExecutionRecord\" src",
      "max_output_tokens": 1200
    },
    {
      "cmd": "sed -n '1,180p' src/tool/tools/exec_command.rs",
      "workdir": "src",
      "yield_time_ms": 10000,
      "max_output_tokens": 1600
    }
  ],
  "stop_on_error": false
}
```

Allowed item fields:

- `cmd`
- `workdir`
- `shell`
- `login`
- `yield_time_ms`
- `max_output_tokens`

Disallowed item fields:

- `tty`
- `accepts_input`
- `continue_on_result`

The disallowed fields are command-task lifecycle fields. They are appropriate
for `ExecCommand`, but not for a batch whose V1 contract is "run these bounded
commands and return one grouped receipt."

V1 should also enforce a small maximum item count. The exact value is an
implementation choice, but it should be low enough to keep one batch receipt
readable and bounded.

## Execution Semantics

V1 executes items sequentially in item order.

For each item, the runtime should reuse the same effective execution boundary
that `ExecCommand` would use for a non-interactive command:

- current agent trust
- current workspace / execution root
- workdir validation
- shell selection
- output truncation
- timeout behavior
- structured command result envelope

`ExecCommandBatch` should not introduce a new command allowlist or denylist as
its primary safety boundary.

If an item fails validation, fails to spawn, exits non-zero, or times out, the
batch records that item as failed and continues by default.

`stop_on_error = true` changes only control flow:

- run items in order
- stop after the first rejected or failed item
- mark later items as skipped

## Command Task Boundary

`ExecCommand` may promote long-running commands into `command_task`.

`ExecCommandBatch` V1 should not do that.

Batch items are intended for bounded command execution with a grouped receipt.
If a command needs:

- interactive input
- tty behavior
- background execution
- blocking continuation on later task result

then the model should call `ExecCommand` directly and use the command-family
task tools (`TaskStatus`, `TaskOutput`, `TaskInput`, `TaskStop`) as needed.

## Output Contract

The canonical result should preserve per-item metadata.

Suggested shape:

```json
{
  "item_count": 3,
  "completed_count": 2,
  "failed_count": 1,
  "skipped_count": 0,
  "stop_on_error": false,
  "items": [
    {
      "index": 1,
      "cmd": "rg -n \"ToolExecutionRecord\" src",
      "status": "completed",
      "exit_status": 0,
      "stdout_preview": "...",
      "stderr_preview": null,
      "truncated": false,
      "duration_ms": 42
    }
  ],
  "summary_text": "ExecCommandBatch completed 2/3 items"
}
```

The model-visible receipt should be compact text, not a raw JSON dump:

```text
ExecCommandBatch completed 2/3 items

[1] rg -n "ToolExecutionRecord" src
exit=0
stdout:
...

[2] sed -n '1,180p' src/tool/tools/exec_command.rs
exit=0
stdout:
...

[3] cargo test slow_suite
exit=124
stderr:
...
```

The receipt should keep item boundaries visible even when output is truncated.

## Metrics

`ExecCommandBatch` changes tool metrics because one model-visible tool call may
represent many shell command executions.

Metrics should distinguish:

- model-visible tool calls
- direct `ExecCommand` calls
- batched command items
- total command executions

Suggested metric names:

- `tool_calls`
- `shell_commands`
- `exec_command_items`
- `batched_exec_command_items`

Benchmark reporting should not count one `ExecCommandBatch` call as one shell
inspection. It should count the batch as one model-visible tool call and count
each item as one command execution.

## Prompt Guidance

Tool guidance should teach the model to use `ExecCommandBatch` for clustered
bounded commands whose outputs are useful before the next reasoning step.

Guidance should not encourage shell command concatenation as the primary
solution.

Suggested wording:

```text
Use ExecCommandBatch when several bounded shell commands should run before the
next decision and do not require interactive input, background task management,
or command-task continuation. Each item uses restricted ExecCommand startup
fields and returns an itemized receipt. Do not use ExecCommandBatch for edits,
interactive commands, long-running commands, or arbitrary nested tools.
```

`ExecCommand` guidance should remain primary for:

- one-off commands
- verification commands where a single result drives the next step
- interactive or tty commands
- long-running commands that may need task supervision

## Future Work

Future versions may add separate capabilities, but they should not be smuggled
into V1:

- parallel command execution
- read-only sandbox-backed inspection batches
- configured parallel-safe tool metadata
- arbitrary tool batching
- richer per-item approval or policy hooks

Those require their own design because they change safety, ordering, and
lifecycle semantics.

