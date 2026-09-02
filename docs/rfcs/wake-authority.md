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
- `RuntimeEventReentry`: a runtime-owned terminal event is correlated to the
  same work item and is allowed to re-enter even when the prior closure summary
  is incomplete or polluted;
- `Override`: an authenticated operator input intentionally supersedes the
  current wait;
- `LocalContinuation`: a non-waiting runtime continuation such as an internal
  follow-up or a valid local timer/external continuation;
- `LivenessOnly`: the runtime should reconsider scheduling, but must not invoke
  the model.

## Invariants

- A `WaitFor` sleep must never rely on an in-memory promise alone.
- A terminal `TaskResult` may re-enter without a matching wait when task
  terminality, work-item ownership, and correlation evidence are present.
- A non-terminal or mismatched task result is liveness-only.
- A wake hint without content is liveness-only; contentful external/system
  delivery may re-enter when its waiting contract matches.
- Authorization classification is centralized in one resolver rather than
  duplicated in trigger-specific continuation branches.
- Observability may report a suspicious sleep posture, but Phase 0 does not
  change scheduling behavior.

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
