# Runtime DB sidecar consistency cache

## Choice

`open_connection` no longer runs the full `/proc/self/fd` sidecar scan on
every connection open. A process-lifetime cache keyed by canonical db path
records the `-wal`/`-shm` `(dev, inode)` identities verified by the last
full scan; opens re-stat the canonical sidecars and skip the scan while the
identities are unchanged.

## Reason

The scan reads the whole fd table twice per suffix per open, so connection
latency grows linearly with the process-wide fd count (#2888: 512 unrelated
fds raised connection p50 from 0.222 ms to 8.261 ms). Read paths open many
short-lived connections, so this cost multiplied across every projection.

## Preserved boundary

The #2850 fail-closed divergence detection is unchanged: deleting or
replacing a sidecar normally changes its canonical `(dev, inode)` identity,
which misses the cache and re-runs the full scan, so deleted-open fds and
open/canonical inode mismatches are still refused. The remembered identities
are sampled before the scan, so any change during a scan forces a rescan on
the next open. One narrow residual gap remains: if a sidecar is deleted and
immediately recreated while this process still holds the old fd and the
filesystem reassigns the same `(dev, inode)`, the cache stays trusted even
though the original scan would have rejected the deleted-open fd. Scan
executions are recorded as `db.sidecar_consistency_scan` diagnostics with the
measured scan duration.
