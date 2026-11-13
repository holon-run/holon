---
title: Schedule recurring agent checks and timers
summary: Configure durable recurring timers and calendar schedules in conversation, enforce read-only audit boundaries, inspect wake reports, and cancel schedules safely.
order: 110
---

# Schedule recurring agent checks and timers

Whether you need to monitor repository CI runs, audit open issue triage queues, or verify server health metrics every morning, running an AI agent on a schedule should not require brittle crontab scripts or external cron services.

Holon provides first-class event-driven timers: you instruct your agent in natural conversation, and it registers a durable timer in its local SQLite ledger, goes to sleep without consuming tokens, wakes up on schedule to run the check, and logs structured reports to its work record.

This guide shows how to set up one-off and recurring schedules, establish strict read-only execution boundaries, inspect wake reports in the Web GUI, and safely cancel timers when no longer needed. For the underlying runtime mechanics, see [Events and timers](../reference/events-and-timers.md).

## Before you start

- A running Holon instance with the [Web GUI](./use-web-gui.md) open (default `http://localhost:8080`) and a [configured model](./configure-models.md).
- An existing agent (such as `developer`, `community-steward`, or `main`).
- If the check involves a remote service or code repository (such as GitHub Actions or server endpoints), make sure the agent has read-only access (e.g. via pre-configured environment credentials or an attached workspace).
- No need to write cron syntax or send raw JSON tool calls: your agent registers and manages its own timers through natural conversation.

## Steps

### 1. Instruct your agent to configure a scheduled check

In the Web GUI chat, ask your agent to set up a scheduled audit. Specify the target, the schedule cadence, and the permitted scope:

> "Every weekday at 8:00 AM, audit open GitHub issues in our repository. Check for untriaged bugs or stale issues, and write a summary report into a dedicated WorkItem. This is strictly read-only: do not edit code, push commits, or close issues."

For a one-time check after a delay (e.g. checking a long CI build after 10 minutes):

> "Check CI run #12845 in 10 minutes. When the timer wakes you, read its current status and report whether it passed. Do not trigger a rebuild or modify code."

**What happens behind the scenes:**
- For calendar-based schedules, the agent calls `CreateTimer` with a five-field cron expression and IANA timezone (for example `calendar_expression: "0 8 * * 1-5"`, `timezone: "Asia/Shanghai"`).
- For delayed checks, the agent uses `duration_ms` (e.g. `600000` for 10 minutes), optionally with an `interval_ms` for fixed periodic repeats.
- The runtime stores the timer durably in SQLite WAL storage. A timer description or summary alone does not grant permissions—it is an operational label.

### 2. Verify safety boundaries and zero-token sleep

Once the timer is registered, ensure that your agent establishes safety boundaries and puts itself into a low-resource sleep:

> "Confirm that you recorded these read-only boundaries in your AGENTS.md or WorkItem, and show me the active timer details."

**What happens behind the scenes:**
- **Zero-token sleep:** Creating a timer alone does not block an agent turn. To wait for the timer, the agent invokes `WaitFor(wake="timer", resource="TIMER_ID", delivery="silent")`. This yields the turn immediately. The daemon puts the agent into dormant state until the timer fires, consuming zero model tokens and minimal system memory while waiting.
- **Explicit boundaries:** Responsible agents append operational constraints (e.g., read-only tokens, forbidden write commands, maximum run retries) into `AGENTS.md` or the active WorkItem plan.
- **Isolation:** Timers are strictly scoped to the agent that created them. An agent cannot query or manipulate timers belonging to other agents.

### 3. Inspect wake reports and audit history

When the scheduled time arrives, the Holon runtime automatically wakes the agent. The agent executes the inspection, records its findings, and settles the turn:

1. Open the **Web GUI** and select your agent.
2. Check the **WorkItems** tab or chat transcript to review the latest wake report.
3. Verify that the agent executed the check against primary sources (e.g. GitHub issue list or CI status API) and that the findings match your expectations.

A timer firing only indicates that the trigger arrived on schedule; verify the actual evidence in the report:

| Audit Item | What to Look For |
|---|---|
| **Source authenticity** | Report cites real issue numbers, commit SHAs, or CI run IDs. |
| **Execution timestamp** | Fires at or shortly after the scheduled minute. |
| **Action compliance** | Confirmed read-only execution; no unexpected branch commits or file mutations. |
| **Next fire time** | For recurring timers, the next scheduled fire time is updated automatically in the timer ledger. |

### 4. Safely adjust or cancel a recurring schedule

When a project milestone is reached or monitoring is no longer required, tell the agent to cancel the schedule:

> "The release is complete. Please cancel the weekday 8:00 AM issue audit schedule and confirm that no further timers remain active."

**What happens behind the scenes:**
- The agent calls `CancelTimer` with the active `timer_id`.
- Any unadmitted wait linked to that timer terminates atomically with reason `timer_cancelled`.
- Historical reports, WorkItem briefs, and logs generated by previous runs remain intact for future auditing, but no new wakeups will occur.

## Key concepts

### Holon timers vs. system cron scripts

| Dimension | Holon Durable Timers | System Cron / OS Crontab |
|---|---|---|
| **Context persistence** | Preserves agent memory, active WorkItems, and task state across wakes | Starts a stateless process with no conversation history |
| **Process lifecycle** | Managed within Holon daemon; survives client disconnects and daemon restarts | Requires separate OS-level process monitoring and wrapper scripts |
| **Resource efficiency** | Agent sleeps via `WaitFor`; zero LLM token usage during wait periods | Often involves polling loops that consume CPU or repeated prompt tokens |
| **Security boundaries** | Constrained by agent permissions, attached workspaces, and `AGENTS.md` | Runs with full user shell permissions unless manually sandboxed |

### Interval schedules vs. Calendar expressions

- **Interval schedules (`duration_ms` / `interval_ms`):** Ideal for fixed-frequency polling, such as checking a build every 5 minutes until complete (`interval_ms: 300000`).
- **Calendar schedules (`calendar_expression` + `timezone`):** Uses standard five-field cron expressions with IANA timezones (e.g. `0 9 * * 1-5` with `America/New_York`), ideal for business-hour syncs, daily standup summaries, or weekly hygiene audits.

### Silent wake vs. Final brief

- **`delivery: "silent"`:** Used when the agent performs an internal check and only records state if issues arise, avoiding unnecessary notification noise.
- **`delivery: "final"`:** Used when every wake must produce an operator-visible summary report in the WorkItem brief.

## Troubleshooting

### The timer did not fire or the check was skipped
- **Check daemon status:** Timers require the Holon daemon to be running. If the host machine rebooted, start the daemon; persisted timers will resume based on current time.
- **Verify timer ownership:** An agent can only wake on its own timers. If you created or modified an agent under a new ID, timers are not automatically migrated.
- **Inspect pending blockers:** A timer wake resumes the agent execution turn, but if the agent's WorkItem has an unresolved business blocker (e.g. waiting for operator input on an unrelated decision), it may remain paused until the blocker is cleared.

## Optional: CLI commands for headless automation

For headless servers or CI environments where you want to inspect or manage timers from a shell:

```bash
# List all active and recent timers for an agent
holon timer list --agent developer

# Create a one-off timer from the CLI (fires after 60 seconds)
holon timer create --agent developer --after-ms 60000 \
  --summary "One-off CI check"

# Create a recurring weekday morning schedule
holon timer create --agent developer \
  --calendar-expression "0 8 * * 1-5" \
  --timezone "Asia/Shanghai" \
  --summary "Weekday morning repo check"

# Cancel an active timer by ID
holon timer cancel TIMER_ID --agent developer
```

## Next steps

- [Run a long-lived task](run-long-lived-task.md) — manage durable multi-step work across terminal sessions.
- [Connect external events](connect-external-events.md) — wake agents using webhooks and external triggers.
- [Events and timers reference](../reference/events-and-timers.md) — full API schemas, parameter types, and runtime lifecycle contracts.
- [Work items reference](../reference/work-items.md) — structure and track multi-session goals with durable checklists.
