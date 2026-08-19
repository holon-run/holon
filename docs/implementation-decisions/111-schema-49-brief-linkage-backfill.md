# Schema 49 brief-linkage backfill

## Choice

Schema 49 materializes all historical `brief_created` audit events into a
temporary candidate table in one scan, indexes that table by Brief and agent
identity, and aggregates candidates for every unlinked Brief through the
temporary index.

The Rust classification loop consumes only the aggregate result. It preserves
the existing unique, missing, ambiguous, and missing-sequence outcomes and
drops both temporary tables before the migration transaction completes.

## Reason

The previous backfill queried `audit_events` once per Brief. The candidate
predicate included JSON extraction and had no matching persistent index, so
SQLite performed a full audit-event scan for every historical Brief. Large
runtime databases therefore made daemon startup effectively unbounded.

A persistent expression index would change the runtime schema for a one-time
migration. Temporary materialization keeps the optimization local to the
backfill while reducing its work to one audit-event scan plus indexed
candidate lookups.

## Preserved boundary

The backfill remains atomic and conservative: it never guesses an ambiguous
link, never rewrites Brief content or timestamps, and still rejects negative
event sequences before storing the immutable `created_event_seq`.
