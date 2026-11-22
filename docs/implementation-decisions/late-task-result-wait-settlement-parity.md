# Late task-result wait settlement parity

> Partially superseded by #4353: `WorkItemRecord.blocked_by` is now a
> descriptive note and `WaitFor` no longer mirrors blocker text, wait identity,
> or recheck deadlines into the WorkItem. The execution-wait settlement parity
> decision below still applies; the blocker-preservation bookkeeping does not.

## Decision

The terminal fast path of `WaitFor(wake=task_result)` (the task is already
terminal when the wait is registered) settles with the same durable execution
wait semantics as a normal registration: the canonical execution state advances to
`Waiting` on the exact task wait via the shared `execution_wait_settlement_transition`.
This also applies to conversation and agent-lifecycle waits without a WorkItem:
the original attempt settles as `Conversation::Wait` on the exact wait, not
`Replied`. The scheduler retains its distinct `TriggeredWait` and `TaskResult`
source identities.

Before this parity fix, an agent-scope late wait could commit `Replied` and then
resume through `TriggeredWait`. Supervision cannot discharge that result as
same-turn consumption. Recovery recognizes this historical edge only through
the original turn's exact task wait, result identity and consuming execution;
it does not rewrite old outcomes or select a later agent-wide brief.

## Evidence

Issue #3463: with a same-WorkItem late wait registered from a
conversation-scoped turn while the WorkItem held an independent external
blocker, the fast path replaced the previous same-scope wait without touching
the WorkItem record or the canonical execution state. The canonical state kept
waiting on the replaced wait, so `resolved_task_wait_is_current` excluded the
exact task wait from claim matching and the re-queued result was consumed as
`reducer_only/task_result_without_model_reentry` with no model re-entry; the
orphaned `blocked_by` then kept the WorkItem ineligible for the settlement
recovery recheck.

`validate_set_work_item_waiting` accepts a `Triggered` task wait (with its
exact `trigger_message_id`) as the atomic wait condition for the waiting
transition, because the late fast path registers the wait already triggered by
the re-admitted result message; `Active` remains the normal-path status and
resolved/cancelled waits stay rejected.

## Preserved boundary

`Triggered -> Resolved` happens in the canonical consuming claim
(`wait_resolution_transition_for_message`). A valid exact wait resumes its
model continuation even when an independent business blocker remains. The
previous blocker-based deferred-settlement exception is removed: it made a
successful `yield_and_reenter` depend on manual blocker clearance and made
ordinary and late registration behave differently.

The canonical execution must still be waiting on that exact wait identity.
Stop, completed owners, replaced/cancelled waits, and result-consumption
fences remain authoritative. Unwaited results receive no new bypass. A
business blocker is not a hard pause; the resume does not satisfy it or make
ordinary WorkItem work runnable.

An independent `blocked_by` no longer affects that continuation: as of #4353
registration and resolution do not rewrite the WorkItem blocker or its legacy
wait identity, and the blocker is descriptive text only.

Interrupted exact consuming claims recover to the same wait identity, not a
general runnable WorkItem. If the resumed turn ends without registering another
wait, settlement follows the ordinary turn outcome; a remaining `blocked_by`
text does not force `Pause`. This keeps normal admission, late registration,
startup recovery, and post-turn eligibility aligned.
