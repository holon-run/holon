---
title: "RFC: Atomic Wake Writer Contract"
date: 2026-09-18
status: draft
---

# RFC: Atomic Wake Writer Contract

## Summary

Every contentful runtime wake uses one transaction-scoped contract:

`TriggerWaitAndEnqueue(message, queue_entry, exact_correlation)`

The contract persists the message and queue entry and, when an exact durable
wait matches, advances that wait from `Active` to `Triggered` in the same
SQLite transaction. General message admission, terminal `TaskResult`
reduction, and timer fire compose this primitive inside their existing outer
transactions. Callback, contentful system wake, recheck, and internal
follow-up writers reach the same contract through normal enqueue.

Pure liveness hints remain bookkeeping signals. They do not manufacture a
contentful message or satisfy a wait.

## Invariants

- The message, queue entry, and wait have one agent identity.
- WorkItem-bound messages may trigger only a wait owned by the same WorkItem;
  agent-lifecycle messages may trigger only an agent-scoped wait.
- A trusted `wait_id` is exact. If it is stale, the writer records the stale
  correlation and must not fall back to another matching wait.
- A supplied WorkItem wait generation must match the canonical execution
  protocol's current `Waiting` generation and wait identity.
- A message from the wait's registration turn cannot trigger that wait.
- An uncorrelated wake may trigger only when exactly one wait matches. Multiple
  candidates are rejected instead of choosing the newest row.
- Duplicate delivery preserves the first trigger message identity.
- SQL faults, OCC conflicts, and producer failures roll back producer state,
  message, queue, wait, audit, and index writes together.

## Writer Composition

### General message admission

The queue transition owns the primitive. Delivery admission keeps its
idempotency ledger, but accepted delivery, message, queue, and wait trigger
share one transaction.

### TaskResult

Terminal task state, result-settlement obligation, result message, queue entry,
and exact task wait trigger commit together. The task's immutable owner and
the message/wait owner must agree; focus never supplies ownership.

### Timer

Timer advancement, pending timer wake, message, queue entry, agent-state
projection, and exact timer wait trigger commit together. Existing timer
expected-record and pending-wake coalescing fences remain producer-specific.

### Callback, system, recheck, and follow-up

Contentful messages use normal enqueue and therefore the same primitive.
Pending callback or wake-hint bookkeeping may precede enqueue, but its durable
retry record is the crash boundary: restart retries message materialization
and atomic enqueue. Liveness-only hints do not enter the primitive.

## Admission And Reconciliation

Scheduler queue selection treats only `Triggered` waits as durable delivery
obligations. Claim resolves the exact wait whose
`trigger_message_id == message.id` and commits that resolution together with
queue consumption, canonical execution admission, WorkItem revision updates,
and execution fences.

Dispatch reconciliation is audit-only on normal traffic. It may emit recovery
signals for historical or damaged state, but it cannot advance an `Active` or
`Triggered` wait, clear a WorkItem blocker, or revise a WorkItem. Any future
repair path must be explicitly named, expectation-fenced, and independently
audited.

## Replay And Recovery

Replaying an already committed writer is idempotent: it does not create a
second queue row, replace the trigger identity, or advance another wait.
Stale exact correlations remain observable and are handled by scheduler
admission policy; they never consume a newly armed wait. Restart reconstructs
queue and wait obligations from the same committed transaction.
