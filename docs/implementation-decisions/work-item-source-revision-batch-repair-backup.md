# WorkItem source revision repair backup scaling

Issue: #3826

## Choice

`holon debug runtime-db work-item-source-revision` no longer creates one
full-database verified backup per repaired entry. Two changes, both at the
backup-creation layer so every verified-backup caller benefits:

1. `create_verified_backup` preflights disk space before `VACUUM INTO`
   using a conservative lower bound: current database file size plus the
   existing WAL size, compared against available space on the backup
   target directory. When the backup still hits ENOSPC or `SQLITE_FULL`
   mid-flight, the error is attributed as disk full (keeping the
   underlying reason) and the unverified artifact from this run is
   removed. Errors such as post-ENOSPC `xShmMap` failures are not
   reclassified.
2. An explicit `--batch` mode (mutually exclusive with `--work-item`,
   apply-only, `--agent` optionally filtering) audits first, creates no
   backup when nothing is repairable, and otherwise creates exactly one
   shared verified rollback backup before the first repair. Every entry
   still runs in its own fenced transaction with the existing evidence
   gates. The first real error stops the batch: committed repairs stay
   committed, the report lists committed, failed, and untried entries with
   the shared backup path, and the CLI exits non-zero.

## Reason

A stranded-registration repair on a 48 GB database created N × DB-size
backups and exhausted the disk; the WAL/shm could then not be recreated and
`holon serve` failed to start, while the follow-up `xShmMap` I/O errors hid
the actual ENOSPC cause. Repair eligibility is unchanged: the fix targets
backup amplification and error attribution, not the evidence fences. The
preflight is a lower bound, not a reservation — concurrent disk use can
still exhaust space — which is why the mid-flight attribution and artifact
cleanup exist alongside it.

## Preserved boundary

- Single-item apply keeps its per-call backup (now preflighted); a batch
  never reuses a `.bak` left by another invocation or an older database
  state — the shared backup is created fresh in the same run.
- Re-running a batch re-audits and is idempotent; historical backups are
  never deleted automatically, and no generic backup retention policy is
  introduced.
- Partial success is real: a batch is not atomic, does not roll back
  committed repairs, and reports the failure point plus the untried
  remainder for a deliberate retry.
- `--no-backup` remains an explicit risk choice for both modes. The
  operation stays offline: the maintenance and migration locks must be
  held (`holon serve` stopped), and in-flight, missing-evidence, or
  daemon-running refusals are unchanged.

## Operations

- Plan disk for `DB size + WAL` before any default backup run; the batch
  mode needs that once instead of N times.
- If a backup fails, verify from the printed backup path that no partial
  `.bak` remains, free space, and re-run; already-repaired entries are
  reported as consistent and skipped.
- Manual rollback from the shared backup: stop the daemon, move the
  current `runtime.sqlite*` aside, restore the printed `.bak` as
  `runtime.sqlite` (it passed `PRAGMA integrity_check` when created), and
  re-run an audit before applying again.
