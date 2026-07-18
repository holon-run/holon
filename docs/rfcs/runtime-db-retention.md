---
title: RFC: Runtime SQLite Retention And Space Reclamation
date: 2026-07-18
status: accepted
handle: rfc-runtime-db-retention
---

# RFC: Runtime SQLite Retention And Space Reclamation

## Summary

Holon provides an opt-in, bounded maintenance policy for the largest
append-oriented tables in `runtime.sqlite`:

- `audit_events`;
- `transcript_entries`;
- `tool_executions`.

Retention is disabled unless the operator explicitly sets
`runtime.retention.enabled = true`. When enabled, a row is eligible only when
it is older than the table's age window and is outside the configured minimum
row floor. Active runtime evidence, active WorkItem references, context
episode references, pending memory-index outbox sources, and a recent completed
turn tail are protected by typed business logic.

Online maintenance uses bounded transactions and optional incremental vacuum.
Full `VACUUM` is an explicit offline operation.

## Goals

- stop unbounded growth after an operator enables a retention policy;
- never prune canonical current state;
- preserve evidence needed by active lifecycle state;
- preserve pending memory-index outbox sources;
- prune audit streams only as continuous sequence prefixes;
- keep write-lock duration bounded;
- make candidate, protected, deleted, and reclaimed-space results observable;
- support existing databases without export, re-import, or payload backfill.

## Non-goals

- no cross-table SQLite foreign keys;
- no normalization of `payload_json`;
- no generic plugin-style garbage collector;
- no automatic full `VACUUM`;
- no retention for other evidence tables in this change;
- no filesystem lifecycle cleanup.

## Configuration

The persisted surface is:

```toml
[runtime.retention]
enabled = true
audit_events_days = 30
transcript_entries_days = 90
tool_executions_days = 90
audit_events_min_rows_per_scope = 4096
transcript_entries_min_rows = 20000
tool_executions_min_rows = 15000
interval_hours = 6
incremental_vacuum_pages = 256
```

`enabled` defaults to `false`. All other values have validated defaults so an
operator may enable the policy with only the boolean setting. Age and count
values are positive. The audit floor cannot be lower than the HTTP event
replay window.

The age and count conditions are conjunctive. A table or audit scope at or
below its floor is skipped even when old rows exist. A maintenance pass must
not delete below the floor.

## Evidence Reference Contract

Retention distinguishes strong roots from soft historical references.

Strong roots are:

- incomplete turns and the most recent 64 completed turns for each agent;
- open WorkItems;
- queued, running, or cancelling Tasks;
- active waits and queued or interrupted messages;
- `WorkItemRef.status == active` runtime source refs;
- `ContextEpisodeRecord.source_refs`;
- every unconsumed `runtime_index_outbox` source;
- the configured table and audit-scope row floors.

The collector reads typed records and normalized evidence columns. It must not
use substring matching against JSON.

Old completed `TurnRecord.tool_execution_ids` and resolved, stale, or archived
WorkItem refs are soft references. After retention, their target may resolve
as missing. This is an intentional historical-boundary contract, not
referential corruption.

## Table Rules

### Audit events

Agent scopes and the host scope are planned independently. The first retained
sequence is the earlier of:

- the first sequence whose timestamp is inside the age window;
- the first sequence in the configured newest-row floor.

Only rows before that sequence may be deleted. Timestamp disorder therefore
widens retention rather than creating a sequence hole. `runtime_sequences`
and `event_log_epoch` are not modified. A cursor older than the retained
prefix continues to use the existing `cursor_not_found` recovery contract.

### Transcript and tool evidence

Rows must be older than the age cutoff, outside the global table floor, and
unreferenced by the strong-root set. Candidate discovery is deterministic and
oldest-first. Each transaction deletes at most a fixed internal batch.

Deleting a tool execution also writes memory-index delete intents in the same
transaction for every index source derived from that record. A rollback
therefore preserves both the evidence and its current index state.

## Online Maintenance

The daemon owns at most one maintenance loop. The loop:

1. reads the latest effective config;
2. does nothing while retention is disabled;
3. plans and executes one bounded pass;
4. optionally runs `PRAGMA incremental_vacuum(N)` only when the database is
   already in incremental auto-vacuum mode;
5. waits for the configured interval or shutdown.

Failures are logged and retried only on a later scheduled round. They do not
make the runtime unhealthy and do not trigger a tight retry loop.

New empty runtime databases select incremental auto-vacuum before schema
creation. Existing databases are not silently rewritten.

## Offline Compaction

`holon debug runtime-db compact` is an explicit offline command. It acquires a
daemon-held maintenance lock non-blockingly, reports pre-operation page and
freelist statistics, sets incremental auto-vacuum, and runs full `VACUUM`.

The command fails closed while the daemon holds the maintenance lock. It does
not delete or replace the original database file itself; SQLite owns the
atomic rewrite behavior.

## Observability

The dry-run and execution paths share a typed report containing:

- effective policy and cutoffs;
- observed, candidate, protected, and deleted rows by table;
- audit-scope skip counts;
- page size, page count, freelist pages, and estimated reclaimable bytes;
- incremental-vacuum result;
- elapsed time.

Structured logs use the same report fields. Maintenance does not add an
unbounded history table.

## Compatibility

No existing business table is rebuilt or backfilled. Existing JSON, HTTP, and
tool response schemas are unchanged. Missing historical evidence remains an
explicit supported result on existing lookup surfaces.
