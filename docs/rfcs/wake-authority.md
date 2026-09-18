---
title: RFC: Wake Authority And Resume Authorization
date: 2026-09-02
status: draft
---

# RFC: Wake Authority And Resume Authorization

## Summary

Holon has two related but different contracts:

1. **Explicit sleep requires durable intent.** When `WaitFor` puts an agent
   into a blocked posture, the wait condition (or an equivalent durable
   continuation) must be persisted atomically with that posture.
2. **Every model re-entry requires verifiable authorization.** A missing wait
   condition does not by itself prohibit re-entry when a runtime-owned event,
   such as a terminal task result, independently proves that re-entry is
   authorized.

The wait registry is therefore authoritative for matching explicit waits, but
it is not the sole authority for every wake. Resume authorization is classified
explicitly as:

- `ExpectedWait`: the trigger matches the persisted waiting reason and carries
  the content required by that wait;
- `RuntimeEventReentry`: a validated runtime-owned terminal task result carries
  a handling obligation for its immutable owner, independently of focus and
  the prior closure summary; canonical admission still decides whether the
  owner can execute now;
- `Override`: an authenticated operator input intentionally supersedes the
  current wait;
- `LocalContinuation`: a non-waiting runtime continuation such as an internal
  follow-up or a valid local timer/external continuation;
- `LivenessOnly`: the runtime should reconsider scheduling, but must not invoke
  the model.

## Invariants

- A `WaitFor` sleep must never rely on an in-memory promise alone.
- A terminal `TaskResult` may re-enter without a matching wait when task
  terminality, immutable ownership, and correlation evidence are present.
  The owner is either an agent lifecycle or a WorkItem; lack of a WorkItem is
  not lack of an owner. Current focus never supplies or changes that owner.
- A non-terminal, forged, or stale-generation task result cannot authorize
  model entry. A valid result for a different focus is not a mismatched result.
- A wake hint without content is liveness-only; contentful external/system
  delivery may re-enter when its waiting contract matches.
- Authorization classification is centralized in one resolver rather than
  duplicated in trigger-specific continuation branches.
- Observability may report a suspicious sleep posture, but Phase 0 does not
  change scheduling behavior.

## Task Result Handling Obligations (#3004)

Keep four decisions separate:

1. **Ownership:** the task's captured owner is immutable.
2. **Wait correlation:** only an exact durable task wait may be satisfied.
   `AwaitingTaskResult` in a closure summary is not an exact wait identity.
3. **Handling:** every validated terminal outcome (success, failure,
   cancellation, interruption) has a durable result settlement obligation.
4. **Admission:** the canonical execution protocol determines whether that
   owner can run now. A continuation recommendation is not an execution grant.

Reuse the task-result settlement ledger for both owner scopes. Persist the
result and its pending obligation in the same transaction. The handling
decision must be observable as deliverable, durably deferred, already
delivered, owner unavailable, or invalid/stale. A processed queue entry alone
does not prove delivery to the model.

Deferred obligations retain a reason and a bounded, durable recovery entry
point, including across restart; they cannot rely solely on an incidental
future turn. Recovery re-evaluates canonical eligibility without consuming
unrelated waits, assigning agent-lifecycle results to focus, bypassing
paused/stopped/closed gates, or spinning on an ineligible owner. Closed or
missing WorkItem owners settle explicitly as unavailable.

Admission and delivery are distinct: binding a pending result to an attempt
does not consume it. Successful delivery settlement and the relevant terminal
transition commit together. Provider failure or interrupted execution must
leave a recoverable obligation; duplicate delivery after settlement must not
open another model turn.

## Phase 0 And Phase 1

Phase 0 documents the dual authorization contract and records an audit event
when an indefinite sleep has no currently visible wake source. This is a
diagnostic guardrail, not a new wake policy. The observation is intentionally
conservative: it considers queue entries, runnable work, active waits, active
tasks, timers, pending wake hints, and interrupted replay.

Phase 1 centralizes the existing behavior in the
`resolve_resume_authorization` decision function. The migration preserves the
existing continuation classes and trigger behavior while making the source of
authorization visible in evidence.

Later phases may move more state transitions to an explicit authorization
record, but are out of scope for this RFC's initial implementation.

The transaction and admission rules for contentful wake writers are defined in
[`atomic-wake-writer-contract.md`](atomic-wake-writer-contract.md).
