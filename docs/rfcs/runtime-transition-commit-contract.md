---
title: RFC: Runtime Transition Commit Contract
date: 2026-07-15
status: accepted
handle: rfc-runtime-transition-commit-contract
---

# RFC: Runtime Transition Commit Contract

## Summary

Holon runtime business transitions must separate durable commit from
post-commit effects.

The commit phase validates the command's expected state and writes every
durable fact that belongs to one invariant in one `runtime.sqlite`
transaction. The post-commit phase updates rebuildable in-memory projections,
publishes process-local events, and notifies workers only after that
transaction succeeds.

The first transition families covered by this contract are:

- WorkItem lifecycle updates;
- wait registration, replacement, cancellation, and resolution;
- queue claim and terminal settlement;
- task lifecycle transitions, including terminal wait release.

[Agent Activation, Settlement, and Dispatch](./agent-activation-settlement-and-dispatch.md)
extends this contract with activation admission, claim, wait consume, and
terminal settlement. Those commands use the same restricted transaction,
expected-state, replay, commit-result, and post-commit effect rules.

This contract does not change operator-facing tools or transport payloads.

## Problem

The runtime currently composes business transitions from storage methods that
each own a separate transaction. A single logical operation can therefore
write its canonical row and later fail while writing an audit event, related
wait, agent state, queue settlement, or index outbox row.

Examples include:

- WorkItem updates persist the WorkItem before the lifecycle audit event;
- wait registration cancels old waits, updates the WorkItem blocker, clears
  turn binding, inserts the new wait, and appends audit events separately;
- task terminal state is persisted before matching waits and WorkItem blockers
  are resolved;
- queue status changes and their processing or failure facts are persisted
  independently.

These intermediate states are durable and survive restart. Reordering
in-memory writes or adding compensating writes does not repair the contract;
the related durable facts need one transaction boundary.

## Goals

- make each runtime business transition declare its complete durable write set;
- validate expected revision or state before applying any write;
- atomically allocate audit sequences and runtime-index outbox rows with the
  canonical state change;
- return a typed commit result that distinguishes applied, idempotent, and
  conflicting commands;
- run cache, event-bus, indexer, and scheduler effects only after commit;
- make commit failures and post-commit effect failures independently testable;
- preserve restart recovery from canonical database facts.

## Non-goals

- do not expose `rusqlite::Transaction` to runtime services;
- do not make cache, event delivery, or scheduler notification authoritative;
- do not introduce a general distributed transaction or plugin framework;
- do not persist every process-local notification in a new generic outbox;
- do not redefine the canonical WorkItem focus schema, which is specified by
  [WorkItem Current Focus Canonical Fact](./work-item-current-focus.md);
- do not physically split all large runtime modules in the first change.

## Fact Layers

### Canonical durable facts

These rows participate in transition invariants and must be committed
atomically when a command changes them:

- `work_items`;
- `wait_conditions`;
- `queue_entries`;
- `tasks`;
- `agent_states` when the transition changes durable agent or turn binding;
- `work_item_continuations` when the transition creates, resolves, or cancels a
  continuation;
- `audit_events`;
- `runtime_sequences` used by committed ledgers;
- `runtime_index_outbox`.

### Durable derived intent

`runtime_index_outbox` is a durable instruction to refresh the rebuildable
memory index. It belongs in the same transaction as its source row.

Future reliable asynchronous work may add another purpose-specific outbox, but
process-local event publication and `Notify` calls are not durable facts.

### Rebuildable projections and liveness effects

The following run only after commit:

- `RuntimeProjectionCache` updates;
- event-bus publication of committed audit events;
- memory-index worker notification;
- scheduler/run-loop notification;
- observational logs and metrics.

Failure of these effects cannot change the committed result. Restart, database
polling, or idempotent effect retry must restore projection and liveness.

## Command Contract

Runtime services construct domain-specific commands rather than sharing one
unbounded command enum:

- `WorkItemTransitionCommand`;
- `WaitTransitionCommand`;
- `QueueTransitionCommand`;
- `TaskTransitionCommand`.

The activation protocol adds focused command families rather than widening one
generic enum:

- `AdmitActivationCommand`;
- `ClaimActivationCommand`;
- `SettleActivationCommand`; and
- `TriggerWaitCommand`.

Each command contains:

- stable record identifiers;
- expected revision or expected lifecycle state;
- the complete replacement record or explicit state delta;
- audit events that describe the same committed transition;
- runtime-index changes for affected searchable records;
- optional related canonical records owned by the same invariant;
- a stable replay identity when the caller can resubmit the command, or an
  expected revision/status that makes reapplication an idempotent no-op or a
  typed conflict.

The database exposes a restricted transition repository. Its implementation
may compose crate-private `*_tx` helpers, but callers cannot execute arbitrary
SQL inside the unit of work.

## Commit Result

The commit phase returns `TransitionCommit`:

- whether the command was newly applied or was an idempotent replay;
- committed canonical records needed by post-commit projections;
- committed audit events with database-assigned sequences;
- a typed `PostCommitEffects` description;
- durable identifiers or sequences required by the caller.

An expected-state mismatch returns the existing typed
`RuntimeStateTransitionConflict`. Business conflicts are not retried as
SQLite-busy failures.

The same replay identity with equivalent payload returns an equivalent commit
without duplicating canonical updates, audit events, result messages, or
outbox rows. Reuse with a different payload is a conflict. Commands without a
dedicated replay key use canonical record identity plus expected
revision/status; exact replays are no-ops and stale or conflicting payloads
return the existing typed transition conflict.

## Transition Inventory

### WorkItem

| Transition | Expected input | Atomic durable write set | Post-commit effects |
| --- | --- | --- | --- |
| create | record absent, revision `1` | WorkItem, lifecycle audit, index outbox | cache WorkItem, publish audit, notify indexer and scheduler |
| update/block/unblock/recheck | exact prior revision | WorkItem next revision, lifecycle audit, index outbox | cache WorkItem, publish audit, notify indexer and scheduler |
| complete | exact prior revision and open state | completed WorkItem, related wait cancellation, optional continuation resolution and caller focus restore, lifecycle audits, index outbox | cache records, publish audits, notify indexer and scheduler |

Focus-specific pick/resume and continuation-frame writes use the same
transaction primitive. Their single-source schema and atomic write sets are
defined by
[WorkItem Current Focus Canonical Fact](./work-item-current-focus.md).

### Wait

| Transition | Expected input | Atomic durable write set | Post-commit effects |
| --- | --- | --- | --- |
| register/replace | owned open WorkItem revision when scoped | cancellation of replaced waits, WorkItem blocker revision, optional agent turn binding, new active wait, audits, WorkItem index outbox | cache WorkItem, publish audits, notify indexer and scheduler |
| cancel | active status | cancelled wait set and cancellation audit | publish audit, notify scheduler |
| task resolve | active task wait and expected WorkItem blocker | resolved wait, optional cleared WorkItem blocker revision, audits, WorkItem index outbox | cache WorkItem, publish audits, notify indexer and scheduler |

No active wait may reference a WorkItem revision whose blocker update failed,
and no WorkItem blocker may claim a newly registered wait that is absent.

### Queue

| Transition | Expected input | Atomic durable write set | Post-commit effects |
| --- | --- | --- | --- |
| claim | queued or interrupted | dequeued queue row and claim audit | publish audit |
| interjected | queued interjection plus expected AgentState snapshot | interjected queue row, decremented AgentState pending count, incoming transcript evidence, and admission audit | remove the committed message from the in-memory queue, publish audit |
| processed | dequeued/interjected | processed queue row and processing audit | publish audit, notify scheduler |
| interrupted/aborted/dropped | allowed non-terminal state | terminal queue row and failure/abort audit | publish audit, notify scheduler |

A queue entry cannot be terminal while its corresponding terminal settlement
audit is absent because of a later transaction failure.

### Task

| Transition | Expected input | Atomic durable write set | Post-commit effects |
| --- | --- | --- | --- |
| create/running/cancelling | monotonic task phase/revision | Task, lifecycle audit, index outbox | cache Task, publish audit, notify indexer and scheduler |
| terminal without wait | non-terminal task or equivalent replay | terminal Task, lifecycle audit, index outbox | cache Task, publish audit, notify indexer and scheduler |
| terminal with wait | non-terminal task, active matching wait, expected WorkItem blocker | terminal Task, resolved waits, optional WorkItem blocker revision, lifecycle audits, Task and WorkItem index outbox | cache Task and WorkItem, publish audits, notify indexer and scheduler |

Terminal task persistence and wait release are one transition. A restart cannot
observe a terminal task with an otherwise matching active task wait solely
because the runtime crashed between repository calls.

An equivalent terminal replay may commit only residual wait and WorkItem
repairs. It reuses stable lifecycle audit identity and does not rewrite the
terminal Task or enqueue another Task index change.

## Commit And Effect Ordering

The required order is:

1. validate command shape;
2. begin the runtime database transaction;
3. load and validate expected revisions and statuses;
4. write canonical rows;
5. allocate and write audit sequences;
6. write runtime-index outbox changes;
7. commit the SQLite transaction;
8. update in-memory projections;
9. publish committed audit events;
10. notify indexer and scheduler;
11. record any post-commit warning.

Runtime code must not mutate shared projection state before step 7.

## Failure Matrix

| Failure point | Durable result | Returned meaning |
| --- | --- | --- |
| validation or expected-state check | no writes | typed conflict or command error |
| canonical write | full rollback | commit failed |
| audit or outbox write | full rollback | commit failed |
| SQLite commit | full rollback or SQLite-defined commit outcome | commit failed unless commit was confirmed |
| cache update | commit remains authoritative | committed with post-commit warning |
| event publication | commit remains authoritative | committed with post-commit warning |
| worker/scheduler notification | commit remains authoritative | committed with post-commit warning |

Callers must not retry the business command merely because a post-commit
effect failed. They may retry the effect or rebuild the projection.

## Fault Injection Contract

The restricted transition repository exposes a test-only fault seam at:

- after expected-state validation;
- after canonical rows;
- after audit events;
- before transaction commit;
- before cache update;
- before event publication;
- before scheduler notification.

Commit-phase injected failures must leave all participating tables and
sequences unchanged. Post-commit injected failures must leave every durable
fact present exactly once.

The seam is intentionally limited to transition tests. General deterministic
clock and ID injection remains separate work.

## Restart And Replay

Startup recovery reads canonical rows and reconstructs caches, queue replay,
active waits, and task posture without assuming that the original process
executed its post-commit effects.

Audit events and index outbox rows are durable consequences of the transition,
not compensating evidence written during recovery. Recovery may publish or
refresh projections again, but it does not duplicate the canonical transition.

## Implementation Boundary

- `runtime_db` owns the restricted transaction and crate-private SQL helpers;
- `storage::AppStorage` constructs index changes and executes storage-local
  post-commit effects;
- runtime domain services construct commands, update runtime projection cache
  from committed records, and notify the scheduler;
- existing single-record repository methods remain available for imports,
  tests, and unrelated facts, but migrated business transitions must not split
  one invariant across those methods.

## Verification

Required tests cover:

- each commit-phase fault point for every transition family;
- strict WorkItem revision conflict and concurrent winner behavior;
- idempotent replay without duplicate audit or outbox rows;
- task terminal plus wait release atomicity;
- queue terminal settlement atomicity;
- post-commit failure followed by restart/rebuild;
- existing runtime restart tests for dequeued messages, active waits, tasks,
  and WorkItems.

