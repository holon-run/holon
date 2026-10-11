# Late task-result wait settlement parity

## Decision

The terminal fast path of `WaitFor(wake=task_result)` (the task is already
terminal when the wait is registered) settles with the same durable WorkItem
wait semantics as a normal registration: both preserve independent business
blockers, and the canonical execution state advances to
`Waiting` on the exact task wait via the shared `execution_wait_settlement_transition`.
The previous `Continue`/`HandoffToWorkItemContinue` settlement is kept only for
agent-scope late waits with no WorkItem waiter.

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

Blocker ownership is decided by registration provenance
(`WorkItemRecord::blocked_by_wait_id`), never by display-text equality: the
claim clears a WorkItem blocker only when the settling wait derived it, so a
same-text independent blocker stays authoritative together with its recheck
deadline. Both task-wait registration paths preserve such an independent `blocked_by`
value; a blocker derived by a replaced task wait is taken over by the new
wait so its settlement can still clear it. A blocker owned by the task wait
remains eligible for the canonical claim, as does an exact task wait with
an independent blocker. The task-wait replacement bookkeeping is not an
admission policy; no classification of historical blocker prose is needed
to authorize a current exact continuation.

Interrupted exact consuming claims recover to the same wait identity, not a
general runnable WorkItem. If the resumed turn ends without registering another
wait and an independent blocker remains, canonical settlement is `Pause`.
This keeps normal admission, late registration, startup recovery, and post-turn
eligibility aligned without erasing a business dependency.
