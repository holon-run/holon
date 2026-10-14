---
title: RFC: Agent output observation
date: 2026-10-14
status: accepted
---

# Agent output observation

## Scope

Issue #3534 separates observation from task completion. Command receipts expose
the existing confirmed output artifact (or explicit unavailability and bounded
fallback). Agent progress is observed by agent identity, not by task identity.
No task-to-message attribution, output ACL, second event ledger, or agent
isolation mechanism is introduced. Existing authentication, invocation provenance
and read-route access checks remain authoritative.

Operator explicitly authorized removing the conversation public-agent filter
on 2026-10-14. Conversation reads now accept active private identities too;
brief/status host reads already did. No new caller ACL is introduced. This is
read access by explicit agent id, not roster exposure or permission to activate,
control or delete another agent.

This first delivery adds replacement observation capabilities. `TaskOutput` and
`holon task output` remain available until the separate retirement step. Retirement
must explicitly resolve their synchronous short-wait behavior: `tail --follow`
does not consume task results and is not an equivalent task-completion wait.
Use `TaskStatus` for task lifecycle and `WaitFor(task_result)` for completion
and re-entry. This work does not implement #3530/#3523 input batching or Monitor.

## Agent tail

The existing default is unchanged:

```sh
holon tail --agent child --limit 20
```

It returns the existing JSON brief array. Opt-in observation returns NDJSON:

```sh
holon tail --agent child --include-progress
holon tail --agent child --include-progress --follow --timeout-ms 5000 \
  --max-events 100 --max-bytes 65536
holon tail --agent child --include-progress --follow --cursor '<checkpoint>'
```

`--follow` without `--include-progress` follows briefs only. `--cursor` requires
follow and resumes after an opaque conversation stream checkpoint; it is never
interpreted as a history/detail cursor. A resumed read does not replay the
initial text snapshot. Each record declares `scope: agent` and `agent_id`.
Progress carries stable activity identity and revision; briefs carry canonical
brief identity and text. The final `end` record reports cursor, stop reason,
record count, history truncation and minimal runtime status.

The projection includes only assistant text activity summaries, briefs and
minimal execution/runtime status. It excludes operator/pending messages, tool
arguments/results, reasoning, prompts and raw transcript/events. This is content
selection, not a secrecy, redaction or isolation guarantee. One agent can have
multiple tasks; observing a brief or an idle agent never completes a task.
Activity text retains the conversation read model's existing summary limit;
brief text comes from the canonical brief endpoint, not a summary preview.

## Existing read model and budgets

Reuse the [conversation read model](conversation-read-model.md):
capability preflight (`agents.conversation-read.v1`), latest turn snapshot,
bounded per-turn activities and conversation SSE. No additional server
endpoint, subscription state or output database is needed. Absent capability
is an explicit error for opt-in observation; default brief reads still work.

The initial snapshot is a bounded recent window, not complete history.
Activities and turns with omitted older records are marked as truncated.
The cursor is a conversation notification checkpoint, not a lossless text
cursor. Resume may revisit stable identity/revision upserts. A truncated
resumed window without a prior local activity watermark explicitly reports
`resume_coverage_unknown`; it does not invent a watermark or promise exact
replay. No second cursor protocol or durable client output store is introduced.
Identity/revision deduplication handles snapshot/detail reads racing live
updates. Briefs without a turn remain visible through the initial brief read.
Read failures and canonical binding mismatches are not silently substituted
with raw trace or another agent's output.

Follow has a whole-operation deadline (including bootstrap, hydration and stream
attachment), default 5 seconds and maximum 30 seconds. Default output budgets
are 100 records (excluding the end receipt) and 64 KiB; maximums are 1000 records
and 1 MiB. Byte accounting
includes serialized NDJSON and the end record, not just text. Observation is
bounded even when no useful text arrives.

Stream changes are staged until their matching checkpoint. A batch is delivered
only if its complete selected projection fits the remaining output budget;
otherwise the preceding checkpoint is retained for resumption. Timeout or
interruption in an unfinished batch never advances the returned cursor.
Version, runtime, epoch, scope or checkpoint mismatch fails closed. Server
`reset_required` terminates with an explicit reset. Detail invalidation hydrates
the bounded activity window. If older omitted activities leave a gap after the
last observed activity watermark, terminate with an explicit gap and retain the
previous checkpoint, rather than silently skipping progress. A fresh snapshot
is required.
Partial/unavailable canonical activity coverage is reported in snapshots and
terminates follow without advancing its checkpoint. HTTP cursor/reset errors
on initial stream attachment produce an explicit reset receipt.

Cancellation, timeout and budget exhaustion close only this read. They do not
prompt/wake/stop the producer, mark briefs read, change task ownership, register
a waiter or claim a TaskResult. CLI reads retain their inherited caller context.
Delivery uses asynchronous writes within the same deadline. On Unix, pipe and
terminal stdout use nonblocking readiness rather than a blocking worker. If
backpressure prevents completing a record or final receipt, the command fails
with a delivery error instead of hanging or appending a misleading receipt to
a partial record. Non-Unix stdout and regular file writes use Tokio's platform
adapters; kernel/blocking-adapter shutdown is not promised to be cancellable.

## Command artifacts and migration

Command artifacts keep the existing head/tail retention, terminal rewrite,
capture failure and bounded fallback contracts described in
[command task output safety](command-task-output-safety.md). A confirmed path
is a host file location, not a promise of continuous append, indefinite
availability, or task completion. Read files in bounded ranges; for stable final
logs wait for task termination. Never reconstruct an artifact path from an
agent home or task id.

The later retirement step updates model tools, CLI metadata, templates, skills
and task documentation together. It does not remove the internal result/capture
services needed for task settlement. No deployment is implied by this RFC.
