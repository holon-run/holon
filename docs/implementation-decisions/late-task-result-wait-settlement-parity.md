# Late task-result wait settlement parity

## Decision

The terminal fast path of `WaitFor(wake=task_result)` (the task is already
terminal when the wait is registered) settles with the same durable WorkItem
wait semantics as a normal registration: the waiter WorkItem record carries
the wait's derived `blocked_by`, and the canonical execution state advances to
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
(`wait_resolution_transition_for_message`) and in one deferred-settlement
exception: when an exact task result meets an independent WorkItem blocker,
the scheduler applies the same transition without opening a consuming
attempt, and the task-result settlement ledger provides the owner-scoped
recovery wake after the blocker is explicitly cleared
(`docs/rfcs/scheduler-work-item-unified-execution-protocol.md`).

Blocker ownership is decided by registration provenance
(`WorkItemRecord::blocked_by_wait_id`), never by display-text equality: the
claim clears a WorkItem blocker only when the settling wait derived it, so a
same-text independent blocker stays authoritative together with its recheck
deadline. The late fast path preserves such an independent `blocked_by`
value; a blocker derived by a replaced task wait is taken over by the new
wait so its settlement can still clear it. A blocker owned by the task wait
remains eligible for the canonical claim.
