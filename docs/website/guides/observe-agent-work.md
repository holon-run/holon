---
title: Observe agent work
summary: Locate an agent's objective, inspect task progress, and retrieve the completion report and artifacts.
order: 250
---

# Observe agent work

Start with the agent and WorkItem, then inspect a task only when you need execution
details. You do not need a metrics collector to check one piece of work.

## Before you start

Know the target agent ID and run these operator commands on the runtime host,
for example through an authorized SSH session. A remote TUI connection does
not retarget later shell commands. The examples use `builder`; replace it with
your agent. Include `--agent` consistently so you do not read the default agent
by mistake.

## 1. Locate the objective

```bash
holon agent status builder
holon work-item list --agent builder
holon work-item get WORK_ITEM_ID --agent builder
```

Copy `WORK_ITEM_ID` from the list. Check the objective, plan status, todo list,
wait reason, and result fields. `current` means focus, not proof that the item
is running. A `yielded` item is parked while another item runs.

If no item matches, the work may have been a short untracked request. Inspect
recent output and the request's message or task ID rather than creating a new
WorkItem merely to observe it.

## 2. Read the latest report or progress

```bash
holon tail --agent builder --limit 10
holon tail --agent builder --include-progress --follow \
  --timeout-ms 5000 --max-events 100 --max-bytes 65536
```

The first command reads recent briefs. The second opts into persisted assistant
progress and returns bounded NDJSON ending in an `end` receipt. Save its cursor
if you need to continue:

```bash
holon tail --agent builder --include-progress --follow --cursor 'RETURNED_CURSOR'
```

A follow timeout means the observation window ended, not that the work failed.
On a reset or gap, take a fresh snapshot without a cursor. If the runtime does
not advertise `agents.conversation-read.v1`, use the default brief snapshot.

Progress may be absent even while a command runs. These reads do not wake the
agent, consume a task result, or isolate output by task; one agent's output can
include several tasks.

## 3. Inspect a specific task

Use the task ID from the execution receipt or task list:

```bash
holon task list --agent builder
holon task status TASK_ID --agent builder
holon task output TASK_ID --agent builder
```

Match the task owner and `work_item_id` with the objective. The task keeps its
original owner even after the agent switches focus. Inspect the exit result and
output for failures; a terminal task is not necessarily successful.

For an agent waiting on task completion, use `WaitFor(wake=task_result)` with
that ID. Repeated status reads are for diagnosis, not a replacement for the
agent's durable wait.

## 4. Retrieve the accepted result

Read the WorkItem again after it completes. Confirm `state=completed` and use
`result_brief_id` to identify the canonical completion report. Recent briefs may
contain acknowledgements and waiting reports as well as results; do not assume
the latest text belongs to your objective.

The report should identify changes, verification, remaining limits, and artifact
locations. Inspect those artifacts at the returned workspace or worktree path.
A file with the same name in another checkout is not necessarily the deliverable.
For exact report retrieval over HTTP, see
[Automate over HTTP](automate-over-http.md#check-agent-status).

## 5. Escalate only if needed

If status and reports do not explain the problem, inspect a bounded transcript:

```bash
holon transcript --agent builder --limit 30
```

Record the agent, WorkItem, task, and message IDs plus the observed failure.
Redact secrets and unrelated content before sharing transcripts or logs.

Success means you can identify the objective's current state or completion
report, locate its artifacts, and distinguish unfinished work from an observation
timeout. For stalled execution, use [Troubleshooting](troubleshooting.md). For
runtime-wide telemetry, use [Configure observability](configure-observability.md).
