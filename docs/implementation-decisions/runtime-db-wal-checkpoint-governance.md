# Runtime DB WAL checkpoint governance runs on the maintenance daemon

## Choice

The maintenance daemon (the same one that owns the fleet-wide runtime DB
maintenance lock) runs a best-effort `PRAGMA wal_checkpoint(TRUNCATE)` every
15 minutes, with a 500 ms busy timeout, independent of the retention policy
toggle. The pass is always best-effort: a busy database reports `busy` and
retries on the next round; it never blocks foreground work longer than the
short busy timeout. The last report is exposed through the performance
diagnostics snapshot (`runtime_db_wal`) so patrol tooling can track the WAL
watermark.

## Reason

`wal_autocheckpoint=10000` only recycles WAL frames: the file keeps its
high-water mark and the wal-index lookup cost grows with checkpoint history.
On a 27.8 GB production copy, read latency degraded 12-19x once the WAL grew
to hundreds of MB, and the only code path that truncated the WAL
(`compact_offline`) requires the daemon to be stopped. Retention is disabled
by default and runs at hour cadences, so long-running `holon serve`
processes had no mechanism that ever shrinks the WAL file. Even after the
idle-audit write amplification fixes, a residual ~2.75 KB/s writer
(`task_result_settlements` recheck updates) grows the WAL indefinitely.

## Preserved boundary

Checkpoint governance bounds the WAL file; it does not reduce write volume.
Writers that churn pages (audit storms, settlement rechecks, rebuild scans)
still need their own write-path fixes. TRUNCATE requires a moment without
concurrent WAL readers; during sustained read pressure the pass defers and
the WAL grows until a quiet round, which is acceptable because
autocheckpoint still recycles frames in the meantime.
