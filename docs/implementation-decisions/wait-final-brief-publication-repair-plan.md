# Resumable WaitFor final Brief repair plans

The `wait-final-brief-publication` debug repair uses a two-stage workflow so
historical scans do not extend the maintenance window:

- prepare scans the runtime database online in bounded keyset batches and
  records checkpoints, hashes, reference counts, source high-water marks, and
  diagnostics in a versioned SQLite sidecar;
- `--resume` continues a compatible incomplete sidecar without rescanning
  committed batches;
- `--apply --plan <path>` requires a completed plan, acquires the maintenance
  lock, checks post-prepare Turn and event increments plus previously inflight
  Turns, creates a verified backup by default, and revalidates all repairable
  candidates in one write transaction before committing any repair.

The sidecar stores identity and validation metadata rather than Brief text or
other operator-facing content. The default dry-run uses the same scanner with a
temporary sidecar. Progress is emitted only to stderr so `--json` stdout remains
a single final report.

This keeps the conservative repair semantics established by #3034 while
removing the old full JSON join, per-candidate event scans, and hour-scale
maintenance-lock scan.
