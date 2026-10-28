---
title: Schedule agent work
summary: Schedule an authorized read-only check, verify the resulting work, and cancel the timer.
order: 110
---

# Schedule agent work

Timers wake an agent after a delay or at a recurring interval. Before creating
one, tell the agent what action is authorized (such as checking a specific CI
run) and when the schedule should end.
A timer summary describes the reminder; it does not grant new permissions.

## Before you start

Use an existing agent with a working model and a running daemon. This example
uses `builder`. Agree on a harmless first action, such as reading the status of a
known CI run and reporting it without making changes.

```bash
holon agent status builder
holon prompt --agent builder \
  "Track the agreed CI check. When its timer fires, read that run's current status and report it. Do not push, merge, or start another build."
```

Agree on the following inputs before scheduling:

| Input | Example | Note |
|---|---|---|
| Target source | Specific CI URL or run ID | Must be readable without new credentials |
| Read-only access | Pre-configured token or public API | Do not paste plaintext secrets in prompts |
| Permitted actions | Read status, produce concise brief | No code edits, push, merge, or rebuild |
| Stop condition | Run reaches terminal status (`passed`/`failed`) | Also cancel if timeout is reached |
| Destination | Active WorkItem completion brief | Reviewable across sessions |

Wait for the agent to confirm the source and scope before scheduling the check.
The CLI commands below are operator actions. Agent tool equivalents are shown
separately.

## 1. Schedule one check

```bash
holon timer create --agent builder --after-ms 60000 \
  --summary "Check the agreed CI run once"
holon timer list --agent builder
```

Save the returned timer ID. Check that the list shows the intended agent,
schedule, and summary. Creating a timer does not pause an agent's current turn.

If the agent itself must wait for this timer, it uses `CreateTimer`:

```json
{ "duration_ms": 60000, "summary": "Check the agreed CI run once" }
```

It then passes the **returned** `timer_id` to `WaitFor`:

```json
{
  "wake": "timer",
  "resource": "TIMER_ID_FROM_CREATE_TIMER",
  "reason": "Wait until the agreed CI check is due",
  "delivery": "silent"
}
```

A description such as `"one minute"` is not a timer resource. Agent timer tools
operate only on the current agent's timers.

## 2. Verify the result

After the timer is due:

```bash
holon timer list --agent builder
holon tail --agent builder --limit 10
holon work-item list --agent builder
```

Check both timer state and the resulting report. Success means the agent checked
the agreed source and reported the result. A timer firing alone does not prove
that the model ran successfully or the CI run passed.

Verify the check details against your agreed inputs:

| Verification item | Expected evidence |
|---|---|
| Source & run ID | Matches the agreed target run |
| Check timestamp | Occurs at or shortly after the scheduled fire time |
| Observed status | Primary source status (e.g., `success`, `in_progress`) |
| Timer ID | Matches the saved ID and shows completed or next fire time |
| WorkItem report | Updated in the WorkItem brief via `holon work-item get` |

## 3. Repeat only when needed

For an authorized recurring check every ten minutes:

```bash
holon timer create --agent builder --after-ms 600000 --every-ms 600000 \
  --summary "Recheck the agreed CI run until it finishes"
```

The agent-tool equivalent uses `interval_ms`, not `every_ms`:

```json
{
  "duration_ms": 600000,
  "interval_ms": 600000,
  "summary": "Recheck the agreed CI run until it finishes"
}
```

Record the timer ID and stop condition in the WorkItem. Tell the agent to cancel
the timer when the run reaches a terminal state. Prefer an external callback for
event-driven updates; for a one-time fallback on an external wait, use
`WaitFor(recheck_after_ms=...)` instead of creating another schedule.

### Calendar-based schedules

For checks aligned to calendar rules rather than fixed millisecond intervals,
use a standard five-field cron expression and an IANA timezone:

```bash
holon timer create --agent builder \
  --calendar-expression "0 9 * * 1-5" \
  --timezone "Asia/Tokyo" \
  --summary "Weekday morning code health check"
```

The agent-tool equivalent:

```json
{ "calendar_expression": "0 9 * * 1-5", "timezone": "Asia/Tokyo", "summary": "Weekday morning code health check" }
```

## 4. Cancel and confirm

```bash
holon timer cancel TIMER_ID --agent builder
holon timer list --agent builder
```

Replace `TIMER_ID` with the saved ID. The agent equivalent is
`CancelTimer({"timer_id":"..."})`. Confirm the cancelled state and check that no
further recurring firings appear. Cancellation does not undo work already
started by a previous firing.

## If the check is late or missing

- Inspect the daemon and agent status before creating a replacement timer.
- Check the timer ID, owner, and cancelled/completed state. Do not use another
  agent's timer in `WaitFor`.
- Inspect the WorkItem and recent report for a provider failure or unresolved
  decision. A timer wake does not clear every blocker.
- After a restart, inspect persisted timer and task state. Do not assume exact
  wall-clock execution or that an interrupted command survived.

For field names and endpoints, see
[Events and timers](../reference/events-and-timers.md).

## See also

- [Run a long-lived task](run-long-lived-task.md) — track objectives across sessions
- [Connect external events](connect-external-events.md) — wake agents from webhooks
