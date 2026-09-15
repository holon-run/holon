---
title: RFC: Command Task Output Safety
date: 2026-09-15
status: accepted
issue:
  - 2984
---

# RFC: Command Task Output Safety

## Summary

Command task output has three independent safety boundaries:

1. a finite combined on-disk retention limit;
2. a higher execution output quota that terminates runaway producers;
3. the existing model-visible `max_output_tokens` projection budget.

These limits must not share one value. Retention protects host storage,
execution quota limits producer behavior, and token projection limits model
context.

## Bounded artifact

Each command task freezes its effective output policy when it starts. The
default policy is:

- `8 MiB` combined retained bytes;
- `64 MiB` combined emitted-byte execution quota;
- a free-space waterline of the greater of `512 MiB` and `5%` of the output
  filesystem.

Before truncation, stdout and stderr chunks are appended in collection order.
After the retention limit is reached, the runtime continues draining both
pipes but keeps only a bounded head and rolling tail. At terminal settlement it
rewrites the artifact as:

```text
bounded head
explicit truncation marker with dropped_bytes
bounded tail
```

The marker is reserved inside the retention limit, so the complete artifact
never exceeds that limit. The retained artifact stores bounded raw command
bytes. Text projections decode those bytes lossily at the API boundary.

## Typed evidence

Command task status and output projections expose an optional
`output_capture` snapshot:

- `emitted_bytes`: original bytes drained from stdout and stderr;
- `decoded_bytes`: bytes in the UTF-8 lossy text used for summaries;
- `retained_bytes`: original command payload bytes retained in the bounded
  artifact, excluding the runtime truncation marker;
- `dropped_bytes`: `emitted_bytes - retained_bytes`;
- the frozen retention and execution limits;
- an explicit truncation flag;
- an optional typed failure code and disk-waterline evidence.

The `TaskOutput.output_truncated` field continues to describe only API preview
truncation. Capture-time loss is reported independently through
`output_capture.truncated`.

## Failure behavior

Crossing the retention limit does not terminate the command. The runtime keeps
draining output and allows the process to finish normally.

Crossing the execution quota terminates the producer and settles the task with
`output_limit_exceeded`.

Falling below the configured free-space waterline terminates the producer and
settles the task with `low_disk_space`. An output open, write, seek, or flush
failure settles it with `output_persistence_failed`, except that `ENOSPC` is
classified as `low_disk_space`.

Once an output failure occurs, the runtime stops further persistence attempts
but continues draining already-open pipes for the bounded post-exit drain
period. Process exit status and output persistence evidence remain separate.
If the artifact is unavailable, `TaskOutput` falls back to the bounded
in-memory command summary instead of failing the retrieval.

## Configuration

The runtime-mutable configuration keys are:

- `runtime.command_task_output_retention_bytes`;
- `runtime.command_task_output_quota_bytes`;
- `runtime.command_task_min_free_disk_bytes`;
- `runtime.command_task_min_free_disk_percent`.

The effective execution quota is never lower than the retention limit.
Configuration reloads affect future command tasks only; an in-progress task
keeps its frozen policy.
