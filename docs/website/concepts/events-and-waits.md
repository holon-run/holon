---
title: Events, waits, and wakeups
summary: How messages, task results, timers, and external events let an agent pause and resume authorized work.
order: 70
aliases:
  - /concepts/external-triggers
  - /concepts/external-triggers.md
---

<a id="external-triggers"></a>
<a id="the-trigger-model"></a>

# Events, waits, and wakeups

An event gives the runtime something to handle. A wait records what an agent
expects before it can continue. A wake lets the runtime reconsider or resume
work. These are separate parts of the same workflow.

For example, an agent fixing a build can start CI, record a wait for that run,
and pause. A CI notification can wake it. The agent must still inspect the
run's status and decide whether the original objective is satisfied.

## Sources of new work and evidence

An operator message can start or redirect work. A command result can resume
work that was waiting for a test. A timer can make a scheduled check due.
An external callback can announce a change outside Holon. Agents can also
enqueue their own follow-up work.

These inputs retain their origins. A timer firing is not a new human request;
a webhook does not replace the operator's scope. Scheduling urgency and
instruction authority remain separate.

<a id="the-agents-waitfor-cycle"></a>

## Registering a wait

The agent uses a wait operation to identify the expected condition: a task
result, a timer, an external object, or operator input. For task and timer
waits, the resource is the actual runtime handle. For an external wait, it
identifies the outside object, such as a pull request or CI run.

Registering a wait does not subscribe an external service, start a timer, or
make a task run. Those resources must exist separately. It also does not mean
the agent must always yield: if the requested task result is already
available, the runtime can return it for use in the same turn.

An explicit wait can resume its matching continuation while the WorkItem
still has an independent business blocker. That resumption does not remove
the blocker or authorize unrelated work. Cancellation, stop, and completion
can also make an old continuation ineligible.

<a id="delivery-modes"></a>
<a id="callback-endpoints"></a>
<a id="example-wake-callback"></a>
<a id="example-enqueue-callback"></a>

## Wake hints and messages

External ingress has two delivery modes:

| Mode | What the agent receives |
|---|---|
| `wake_hint` | A signal to recheck outside state. The callback payload is not delivered as a model-visible message. |
| `enqueue_message` | A queued message containing the external payload, with its provenance preserved. |

Use a wake hint when the source of truth can be queried, such as CI status.
Use a contentful message when the payload is needed for the work. In either
case, receiving the notification is evidence of delivery, not proof that the
external operation succeeded.

For callback setup and examples of both delivery modes, see
[Connect external events](../guides/connect-external-events.md).

<a id="default-ingress-capability"></a>

The agent's default ingress capability can be reused across objectives.
Individual WorkItems record their own waits. There is no need to create and
revoke a callback for each ordinary wait cycle.

<a id="scheduled-timers"></a>

## Timers and fallback checks

A timer schedules a future wake; creating it does not pause the current turn.
If the current work must wait for that timer, the agent records a timer wait.
Repeating timers remain a scheduling responsibility until cancelled.

A fallback recheck deadline on a wait serves a different purpose: reconsider
that specific wait if no matching wake arrives. It is not a substitute for
an independently managed recurring schedule.

<a id="integration-patterns"></a>
<a id="cicd-pipeline"></a>
<a id="github-webhooks"></a>

## A complete CI continuation

The operator asks an agent to fix a build and verify CI. The agent records
the objective and starts a run, then arranges a callback and waits. When a
notification arrives, it checks the correct run and revision. If CI failed,
it inspects the failure within the approved scope. If CI passed, it checks
the remaining acceptance criteria before completing the objective.

No step requires the operator to keep a client window open while the daemon
continues running. A stopped daemon or interrupted process is a separate
recovery case; see [Context continuity](./context-continuity.md).

<a id="security-model"></a>
<a id="reset-callback-token"></a>
<a id="cancelling-triggers"></a>

## Access and authority

Callback URLs contain capability secrets. Possession allows the corresponding
wake or enqueue action, so keep them out of public reports and source control.
The URL does not grant the sender operator authority. Rotate or revoke it
when access should end, rather than using revocation as routine wait cleanup.

<a id="agent-stop-auto-revocation"></a>

Stopping an agent revokes its external triggers. Before reconnecting an
integration, inspect the current capability and replace any revoked URL.
See [Connect external events](../guides/connect-external-events.md) for
callback replacement and revocation.

<a id="see-also"></a>

## Next steps

- [Connect external events](../guides/connect-external-events.md)
- [Schedule one-time or recurring work](../guides/schedule-agent-work.md)
- [HTTP control plane reference](../reference/http-control-plane.md)
- [Wake and continuation specification](../spec/wake-and-continuation.md)
