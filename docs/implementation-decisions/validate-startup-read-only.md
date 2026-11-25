# `daemon validate-startup` read-only inspection

Decision:

- Read-only mode opens the existing runtime database with
  `SQLITE_OPEN_READ_ONLY` and `PRAGMA query_only=1`, and never writes the data
  directory. A stopped runtime can still leave pending `-wal` frames; SQLite can
  only read those by mapping and rewriting `-shm`, so in that case the command
  copies `runtime.sqlite` plus its sidecars into the system temp directory and
  inspects the copy. The copy is dropped with the process.
- When no `-wal` exists (or it is empty), the original file is opened directly
  with `immutable=1`, which also creates no sidecars.
- `--migrate` is an explicit write mode gated on the runtime maintenance lease
  and a non-live control socket. It uses the existing `RuntimeDb::migrate`
  operation (schema migrations, backfills, verification) instead of the server
  bootstrap path, so it never materializes workspace/identity rows, normalizes
  wakes or queues, or appends `timer_wakes_recovered`.

Reason:

- The acceptance contract for read-only mode is a byte- and mtime-identical
  data directory including `-wal` and `-shm`. A plain read-only SQLite session
  fails that: it creates `-shm` when missing and updates read marks when the WAL
  is pending.
- Refusing to validate any library that still has a WAL would reject normal
  daemon-stopped libraries, which defeats the pre-upgrade and self-diagnosis
  use cases.

Boundaries:

- Read-only mode is a snapshot check, not a consistency proof under concurrent
  writers. Run it with the daemon stopped for a stable view.
- The eligibility phase reports only persisted snapshot metadata
  (`snapshot_present`, `snapshot_missing`, or `stale` when current records exist
  without capture bookkeeping). It never recomputes eligibility or admits
  input.
- `--migrate` mutates only the library it is pointed at; it does not detect or
  redirect a live daemon's database beyond the lease and socket gate.
