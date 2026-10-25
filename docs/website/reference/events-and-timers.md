---
title: Events and timers
summary: Callback routes, delivery modes, timer fields, and explicit wait contracts.
order: 39
---

# Events and timers

For setup and verification, use [Connect external events](../guides/connect-external-events.md)
or [Schedule agent work](../guides/schedule-agent-work.md). This page records the
contracts behind those workflows.

## Callback endpoints

| Method and route | Delivery |
|---|---|
| `POST /api/callbacks/wake/:callback_token` | `wake_hint`: wake the agent to inspect external state |
| `POST /api/callbacks/enqueue/:callback_token` | `enqueue_message`: enqueue external callback content |
| `POST /api/control/agents/:agent_id/reset-callback` | Authenticated control operation: revoke the current default capability and issue a replacement |

A callback has one delivery mode and one `trigger_url`. The callback token is a
capability secret resolving to an active trigger and agent. Invalid tokens,
inactive identities, and delivery-mode mismatches are rejected. A callback's
accepted delivery does not prove that work completed.

Callbacks accept an optional body. An empty POST is sufficient for a wake.
For an enqueue notification, JSON can contain a `text` field:

```json
{ "text": "The recorded CI run changed; inspect its current state." }
```

Callback payloads remain external evidence. They do not become operator
instructions because the sender holds the callback URL. The callback routes have
a 256 KB body limit; use stable source identifiers rather than large log dumps.

Reset returns `ok`, `agent_id`, `external_trigger_id`, `trigger_url`,
`delivery_mode`, and `status`. Treat the returned URL as secret and use it
exactly as issued. Do not print it into a transcript, paste it into an issue, or
store it in source control.

## Default ingress and revocation

The agent's runtime context supplies its default ingress capability. Use that
capability for ordinary integrations rather than creating multiple triggers.
`CreateExternalTrigger` is for supported compatibility or alternate-mode needs,
not a way to bypass caller authority.

`CancelExternalTrigger` takes `external_trigger_id` and revokes the capability.
Reset invalidates the old default URL and provisions a new one. Stopping an agent
revokes its external triggers; do not assume a sender's old secret remains valid
when the agent is restarted.

## Timer operations

| Operation | Operator CLI | Agent tool |
|---|---|---|
| Create | `holon timer create --after-ms N [--every-ms N] [--summary TEXT] [--agent ID]` or `holon timer create --calendar-expression "M H * * *" --timezone AREA/LOCATION` | `CreateTimer(duration_ms?, interval_ms?, calendar_expression?, timezone?, summary?)` |
| List | `holon timer list [--limit N] [--agent ID]` | `ListTimers(limit?)` |
| Read | HTTP detail route below | `GetTimer(timer_id)` |
| Cancel | `holon timer cancel TIMER_ID [--agent ID]` | `CancelTimer(timer_id)` |

The legacy `holon timer --after-ms N` creation form remains supported. Agent
tools operate on the current agent; they do not accept a target agent ID.
Durations and intervals are positive milliseconds. Creating a timer does not
pause the turn. Cancelling an already-cancelled timer is idempotent; a completed
timer cannot be cancelled.

Calendar timers accept exactly five standard cron fields (minute, hour, day of
month, month, day of week) and an IANA timezone. The expression is evaluated in
that timezone, while `next_fire_at` and persisted timestamps remain UTC. Calendar
timers recalculate the next matching occurrence after each fire and after
restart; they do not use a fixed UTC interval, so daylight-saving transitions
follow the named timezone. `calendar_expression` and `timezone` must be supplied
together and cannot be combined with `duration_ms` or `interval_ms`.

| HTTP route | Contract |
|---|---|
| `POST /api/control/agents/:id/timers` | Create with either `duration_ms`/`interval_ms` or `calendar_expression`/`timezone`, plus `summary` and control `authority_class` metadata |
| `GET /api/agents/:id/timers` | List recent timer records |
| `GET /api/agents/:id/timers/:timer_id` | Read an owned timer or return a not-found error |
| `POST /api/control/agents/:id/timers/:timer_id/cancel` | Cancel the timer; optional control `authority_class` metadata |

Control metadata does not elevate a caller's authority. Use the
[HTTP reference](http-control-plane.md) for authentication and admission rules.

### Timer record fields

| Field | Meaning |
|---|---|
| `id`, `agent_id` | Timer identity and owning agent |
| `created_at` | Creation time |
| `duration_ms` | Initial delay |
| `interval_ms` | Optional recurrence interval |
| `repeat` | Whether the timer repeats |
| `status` | `active`, `completed`, or `cancelled`; legacy `scheduled` deserializes as `active` |
| `summary` | Optional reminder description |
| `next_fire_at`, `last_fired_at` | Next scheduled and most recent firing timestamps |
| `calendar` | Optional normalized five-field expression and IANA timezone for calendar timers |
| `fire_count` | Number of recorded firings |

## WaitFor contract

`WaitFor` registers a scheduling condition; it does not create a timer or install
a webhook. Its fields are:

| Field | Contract |
|---|---|
| `wake` | `operator_input`, `task_result`, `external`, `timer`, or `system` |
| `resource` | Task ID for task results; actual timer ID for timers; optional stable outside object for external waits |
| `reason` | Concrete explanation of what prevents progress |
| `delivery` | Required: `final` or `silent` |
| `work_item_id` | Optional owned open item when no execution-bound item takes precedence |
| `recheck_after_ms` | Optional fallback recheck delay, not a recurring schedule |

`wake=timer` requires a timer owned by the current agent with an available wake.
Invalid, foreign, cancelled, or already-consumed timer resources return a
recoverable error. For a task-result wait, the task can belong to any WorkItem
of the same agent; waiting never moves the task.

The execution-bound WorkItem owns the waiter when present, even if an explicit
conflicting item is supplied. Without that binding, resolution uses an explicit
owned open item, the turn-bound item, current focus, or agent lifecycle.

### Continuation and delivery

| Returned continuation | Agent action |
|---|---|
| `continue_turn` | Continue useful work now; do not publish a waiting report |
| `yield_and_wait` | Yield until the registered wake |
| `yield_and_reenter` | Yield for the exact queued result to reenter |

If a final-delivery path returns `awaiting_final_report`, the wait is not yet
committed. Supply a non-empty assistant report; only read-only fact checks are
allowed in that report continuation. Do not recursively call lifecycle tools.
Silent delivery requires no operator-facing waiting report.

Wait registration and final-report publication are validated together. Stale
execution, WorkItem revision, wake, or interjection state can invalidate a
prepared wait. A valid matching wake may resume its continuation despite an
independent business blocker, but does not satisfy that dependency or clear it.
Stop, completion, replacement, cancellation, and result-consumption checks still
prevent stale or duplicate continuations.
