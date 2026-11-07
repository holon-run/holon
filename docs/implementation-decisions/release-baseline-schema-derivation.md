# Share the immutable release baseline schema

The release baseline derives final schema SQL by replaying the compiled migration
set on an in-memory scratch database. This derivation has no target-database
inputs. Repeating it for every new database causes redundant schema parsing and,
under concurrent startup, contention on SQLite's process-wide allocator mutex.

Retain the successfully derived SQL once per process, with a single initializer.
Return initialization errors without publishing them so a later call can retry.
Do not retain a SQLite connection or any target-database data in this cache.

Every database still runs its own baseline transaction, data checks, backfills,
schema fingerprinting, migration history writes, and commit. The compatibility
migration chain remains the schema-equivalence oracle; this optimization does
not change migration versions or the on-disk schema.
