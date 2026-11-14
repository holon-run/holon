---
title: Observe, track, and verify agent progress
summary: Track an agent's live execution status, inspect dynamic Todo checklists in the Web GUI, diagnose active waits in conversation, and verify completion reports and artifacts without tailing raw terminal logs.
order: 250
---

# Observe, track, and verify agent progress

When an AI agent takes on long-running work—such as auditing an unfamiliar codebase, refactoring a subsystem, or waiting for test suites to run—you need continuous visibility into what it is doing. You shouldn't have to SSH into a remote host, tail NDJSON logs, or guess whether the agent is making progress, paused for approval, or stuck in a loop.

In Holon, agents operate on durable **WorkItems** backed by structured SQLite state. You can monitor live progress, check off milestones, review generated file artifacts, and diagnose active blockers directly in natural language through the Web GUI or mobile interface.

This guide demonstrates how to query an agent's progress in conversation, interpret its scheduling states, inspect deliverables in the Web GUI, and review the canonical completion report. For underlying architectural mechanisms, see [Work items reference](../reference/work-items.md) and [Context continuity](../concepts/context-continuity.md).

## Before you start

- A running Holon instance with the [Web GUI](./use-web-gui.md) open (default `http://localhost:8080`).
- An active agent (such as `developer` or `main`) executing or waiting on a task.
- An assigned objective tracked in a durable WorkItem (see [Run unattended long-lived tasks](./run-long-lived-task.md)).
- No server shell or SSH session required: everyday status checks, checklist monitoring, and artifact reviews happen directly in the Web GUI.

## Steps

### 1. Query progress and active waits in conversation

The fastest way to understand what your agent is doing is simply to ask in the Web GUI chat:

> "What is your current progress, and are you waiting on any blockers or tasks?"

The agent inspects its current WorkItem, checks its active wait conditions, and returns a concise status update:

- **Current milestone:** What part of the plan it is currently executing.
- **Active scheduling state:**
  - **Runnable / Executing:** The agent is actively reading files, generating diffs, or executing local commands.
  - **Waiting on Task (`wake=task_result`):** A long-running background command (e.g. compiling a build, running test suites, downloading assets) is executing in the background. The agent is in zero-token sleep and will resume automatically when the command finishes.
  - **Waiting for Operator (`wake=operator_input`):** The agent has reached a decision boundary, requires user confirmation for a high-impact operation (e.g. Git push or database migration), or needs missing credentials.
  - **Waiting on Timer or External Event (`wake=timer` / `wake=external`):** The agent is parked waiting for an scheduled interval (see [Schedule periodic tasks](./schedule-agent-work.md)) or an incoming webhook callback.
- **Remaining milestones:** Outstanding items in the progress checklist.

### 2. Inspect live Todo checklists and artifacts in the Web GUI

You do not have to rely only on conversational summaries. Holon projects the agent's internal state directly into the Web GUI:

1. **Open the WorkItems sidebar panel:** Click the **Work Items** icon in the Web GUI navigation bar.
2. **Review the active checklist (`todo_list`):**
   - View items marked `completed` (checked), `in_progress` (currently active), or `pending`.
   - As the agent works through files and runs tests, these items update in real time without refreshing the browser.
3. **Review the plan artifact (`plan.md`):**
   - Click the plan link to read the durable Markdown document outlining the scope, acceptance criteria, and architecture decisions established for the task.
4. **Examine generated artifacts:**
   - When the agent produces code patches, test scripts, or documentation drafts, it registers them as deliverables. Click on artifact links in the conversation stream or sidebar to view inline diffs or formatted previews.

### 3. Diagnose and resolve blockers when paused

If an agent enters a paused state (`waiting_operator` or `plan_status="needs_input"`), it cannot proceed until the blocker is cleared. You can resolve the condition directly in chat:

1. **Review the blocker explanation:** The agent will state why it stopped (e.g. *"A migration requires dropping an unused column; please confirm whether to proceed or keep the column."*).
2. **Provide your decision or approval:**
   > "Proceed with dropping the unused column and update the schema tests accordingly."
3. **Clearing the wait:** Providing the requested input satisfies `WaitFor(wake="operator_input")`. The agent marks the plan ready, wakes up, and continues implementation without losing context.
4. **Intervening or stopping runaway work:** If you notice an agent taking an undesirable approach or repeating unsuccessful commands, tell it to halt:
   > "Stop the current test loop. Revert the last patch in `auth.rs` and let's rethink the token validation logic."

### 4. Verify the canonical completion report

When the agent finishes all checklist items and completes verification, it settles the WorkItem and publishes a canonical **Completion Report** (Brief):

Check the final brief in the Web GUI chat for four key elements:

1. **Summary of changes:** Concrete list of modified files, added modules, or updated configurations.
2. **Verification proof:** Exact commands executed (e.g. `cargo test --all`, `npm run build`) and their exit status (`0`). Never accept a completion report that claims success without demonstrable verification evidence.
3. **Delivered artifacts:** Verified local paths to changed files or worktrees.
4. **Caveats and next steps:** Any remaining follow-ups, pending reviews, or environment assumptions noted by the agent.

Once satisfied, you can review the Git worktree diff (see [Work in a project workspace or Git worktree](./work-with-workspaces.md)) and merge the changes into your main branch.

### 5. Optional: Inspect via CLI or HTTP for automation

If you are building CI/CD pipelines, automated patrol scripts, or headless monitoring dashboards, Holon provides equivalent non-interactive inspection surfaces.

**Inspect recent briefs and progress via CLI:**
```bash
# View the latest operator-facing briefs
holon tail --agent developer --limit 10

# Stream progress events with bounded NDJSON
holon tail --agent developer --include-progress --limit 20
```

**Inspect active WorkItems and background tasks:**
```bash
# List all open work items and their status
holon work-item list --agent developer

# Inspect a specific work item and its checklist
holon work-item get WORK_ITEM_ID --agent developer

# Inspect a background command task
holon task status TASK_ID --agent developer
```

For HTTP endpoints, see [Automate over HTTP](./automate-over-http.md).

## Next steps

- [Run unattended long-lived tasks](./run-long-lived-task.md): Learn how WorkItems sustain long-running tasks across disconnects.
- [Work in a project workspace or Git worktree](./work-with-workspaces.md): Review and clean up isolated worktree branches created during task execution.
- [Schedule periodic tasks](./schedule-agent-work.md): Configure automated timers for recurring agent health checks and audits.
- [Troubleshooting](./troubleshooting.md): Diagnose stalled processes or provider timeout issues.

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
