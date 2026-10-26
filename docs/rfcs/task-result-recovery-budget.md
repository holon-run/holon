---
title: Bounded task-result recovery and operator rearm
status: accepted
---

# Bounded task-result recovery and operator rearm

This extends [task-result settlement and delivery](task-result-content-and-presentation.md).
Automatic recovery must be durable and finite without treating an undelivered
result as successfully settled. Provider fallback transfers an existing
admission; it is not another recovery root.

## Already-settled owner wake consumption

A queued recovery owner wake can outlive delivery by another legitimate path.
Before creating a turn, the scheduler may consume that source as system-only
work when its durable runtime provenance, original result identity, captured
owner and task generation still match, all results for that owner are settled,
and no unresolved owner or exact task-result wait remains. Exact-wait recovery
continuations and uncertain, pending or in-flight evidence retain the existing
reducer/admission path; absence of model reentry alone is not proof of no work.

The no-work predicate is revalidated in the same transaction as the expected
queued/interrupted source CAS to processed, the agent pending projection, and a
metadata-only `task_result_recovery_source_consumed` audit event. A concurrent
result or changed source prevents consumption and causes replanning. No turn,
execution attempt, result settlement, recovery budget charge or business
completion is manufactured. Pre-commit faults leave the source retryable;
post-commit replay cannot consume it twice. Existing reducer visibility and
historical turns are unchanged.

## Durable admission budget

Each settlement obligation retains its result identity, task and owner fences.
Its recovery state also records the epoch, charged admission count, last root
attempt and admission time, pause time, next recheck, and diagnostic publication
time. Older payloads default the new fields to an uncharged, unpaused epoch.

One epoch permits five automatic recovery root admissions. Charge only when a
canonical root is actually admitted, not when a scanner runs, a wake is queued,
a provider request is retried, or a fallback successor inherits the admission.
The unique key `(result_identity, recovery_epoch, root_attempt_id)` makes
replaying an admitted root idempotent. The admission ledger is retained across
restarts and deleted with its settlement obligation.

The integration point is the canonical recovery-admission transaction:

```text
charge_recovery_admission_tx(
    tx: &Transaction<'_>,
    result_identity: &str,
    root_attempt_id: &str,
    now: DateTime<Utc>,
) -> Result<bool>
```

The charge and `caller_admitted` activation binding must commit together.
`true` permits the admission and includes an already charged root in this epoch.
`false` denies a new admission when the record is missing or settled, recovery
is paused, the budget is exhausted, or admission backoff has not elapsed.
The caller must not overwrite charged fields with a pre-charge payload, and a
subsequent admission failure must roll back the transaction.

Recovery snapshot selection checks the same durable eligibility before reserving
any result slots or bytes. Paused or backing-off identities cannot crowd out
new results for the same owner. This check does not charge; the admission
transaction revalidates eligibility and charges only selected identities.
An already charged root remains eligible for its interrupted replay, even after
the final charge pauses the identity. Normal authorized input is not restricted
by the automatic recovery budget.

When an interrupted result snapshot and provider-lineage retries coexist for
the same execution owner, select that snapshot before the retries. Otherwise
older `next` retries can indefinitely starve a `background` scanner root after
restart. This selection does not bypass canonical replay fences or change
operator/interject, exact-wait, unrelated-owner or provider-parking precedence.

A still-deliverable, runtime-owned scanner source for that same owner must also
get an admission opportunity before provider-lineage retries, even before its
first snapshot is attached. Attached interrupted snapshots retain precedence
over new roots. The search stops at operator/interject input, including input
behind the selected provider retry. A source parked by the provider gate is
excluded for that poll, so other lineages may continue without bypassing shared
cooldowns or probe leases. This is queue selection, not a priority rewrite, a
recovery charge or a consumption grant; canonical admission revalidates the
source and budget.

Eligibility filtering precedes the bounded candidate query's `LIMIT`. Snapshot
commit reloads the selected identities rather than truncating an unfiltered
owner scan again. The frozen input cutoff controls snapshot membership; budget
eligibility and admission timestamps use the runtime clock, not that cutoff.

The first charge in an epoch has no admission-backoff requirement. Subsequent
new roots wait at least 60, 120, 240, then 480 seconds after the preceding
admission. The ledger derives eligibility from the persisted last admission
time and count; a restart, repeated wake, or stale scanner check cannot bypass
the delay or reset the budget. Scanner rechecks are not admissions.

## Repeated failed exact-wait recovery

A failed recovery retains its original consumption grant only through canonical
source/attempt/turn evidence and the selected identity's durable root admission.
An admitted runtime recovery followup persists its source in an anchor-only input
snapshot before moving selected result identities. That snapshot alone, queue
progress and caller-supplied refs are not grants. If interrupted before its first
result admission, replay revalidates the immutable source and canonical exact
wait, and may admit only that source's original result identity. It cannot refill
an empty predecessor snapshot from other owner results. Once results were
admitted, replay restores only the predecessor's admitted identities.

The wake's owner key identifies the consuming owner, including a cross-WorkItem
exact waiter; it does not change the task's captured owner. Explicit rearm starts
a new budget epoch without revoking a previous canonical snapshot's selected
admission witness.

If an exact waiter B consumed a result captured by owner A, another failed root
continues B without migrating the task. Preserve the resolved predecessor wait;
atomically create its triggered successor, recovery wake and B's canonical
Waiting state. Revalidate both owners, the result/task generation, predecessor
grant, waits, source queue, agent state and consuming WorkItem revision at commit.
Do not clear an independent business blocker or fall back to A after B is closed
or replaced. Charge only when that successor's selected snapshot is admitted,
using the same cross-root budget and backoff.

## Pause and operator visibility

The fifth admitted root is allowed to finish; reaching the limit pauses only
further automatic admissions. A successful final root still settles normally.
If delivery remains unsettled, preserve its content, ownership, original
provenance, and pending obligation. Do not mark the task result delivered or
complete the owner's business work merely because recovery has stopped.

The scanner uses the shared obligation classifier. Only a recoverable obligation
can enter automatic recovery; an active claim, an invalid or stale owner fence,
or a paused obligation cannot generate another recovery wake or charge.

Once the final root is no longer open, a paused unsettled result publishes one
deterministically identified failure brief per result and epoch, with its audit
event. Publication records the task, result identity, owner, last root, exhausted
budget, and identity-targeted repair command. Restarting publication is
idempotent, and the scanner drains all pending diagnostics, not only one batch.
The diagnostic creates no model turn, completion report, or recovery wake.
Scheduler-recovery inspection exposes the durable recovery state for diagnosis.

## Identity-targeted operator rearm

After inspecting the unresolved result, an operator can explicitly start a new
recovery epoch:

```sh
holon debug scheduler-recovery --agent AGENT_ID --apply --rearm-task-result RESULT_IDENTITY
```

This is an operator-only, agent-and-result-targeted repair. Agent-originated CLI
calls are rejected; it cannot be combined with broad affected-agent or
message-targeted repair. The daemon must be stopped before apply. A verified
database backup is required by default; `--no-backup` is an explicit opt-out.

Rearm rejects a missing identity or mismatched agent, and makes no change to an
already settled or unpaused obligation. A changed record increments its epoch,
resets the admission count, last root and admission time, pause and diagnostic
markers, and schedules a scanner recheck after 30 seconds. The change and
`task_result_recovery_rearmed` audit event commit together.

Rearm does not erase prior epoch admissions, change task or WorkItem ownership,
clear an independent business blocker, revive a cancelled owner, or grant new
delivery authority. Existing classifier and generation fences still decide
whether recovery is eligible when the scanner resumes. It does not admit a
model turn itself.
