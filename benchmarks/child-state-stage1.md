# Child-state task reads: stage 1 (#3745)

## Scope and method

This change replaces the loaded child's global `tasks.latest_all()` read and
in-memory sort with `active_for_agent(child_id, usize::MAX)`. It reuses the
owner/status index without changing schema, timeouts, output, or lifecycle
protection. Active counts and per-child closure/brief reads remain separate.

Measurements on 2026-10-25 used base `339b87c5`, release builds with
`RUSTFLAGS="-D warnings"`, and the same `child-state.v2` fixture in both binaries.
The old binary includes the attribution and benchmark additions but retains the
old query. The final regression-only correction does not change either measured
production path. See [the harness instructions](README.md).

- Temporary synthetic database, StubProvider, three active tasks per loaded child.
- Real Router `/state` requests, fresh AppState/gate per request, concurrency one.
- Children: 0/1/50; unrelated terminal history: 0/100,000.
- Each cell: one warmup, then three batches of ten requests, closed loop.
- A-A first, then old/new/new/old separately at each history size.
- Cell budget 60 seconds, outer request timeout 15 seconds, existing internal
  projection deadline 10 seconds; no protection settings increased.
- Host setup, isolated SQL probes, and bounded shutdown excluded from HTTP timing.
- Shared host, not isolated hardware: environment snapshots include other
  compilation/test processes and background services. Process CPU percentages
  are snapshots, not attributable interval CPU measurements.

## Observations

Each range below contains the two separate run-level nearest-rank p95 values;
it is not a pooled percentile or confidence interval. Units are milliseconds.
Each successful cell has 30 measured successes and no measured failures.

| Loaded children | History | Old child-summary p95 | New child-summary p95 | Old HTTP p95 | New HTTP p95 |
| --- | --- | --- | --- | --- | --- |
| 0 | 0 | 0.026–0.033 | 0.025–0.030 | 2.24–4.36 | 1.89–2.69 |
| 1 | 0 | 1.14–1.40 | 1.18–1.20 | 2.96–3.35 | 3.01–6.42 |
| 50 | 0 | 68.62–101.30 | 55.47–56.31 | 70.58–103.58 | 57.71–58.26 |
| 0 | 100,000 | 0.026–0.051 | 0.042–0.050 | 2.48–3.15 | 2.96–3.07 |
| 1 | 100,000 | 303.05–361.12 | 2.05–2.69 | 305.36–363.66 | 5.14–6.27 |
| 50 | 100,000 | unavailable | 48.27–49.11 | unavailable | 50.14–50.93 |

Old 50-child/100,000-history cells failed during warmup at the existing
10-second deadline with HTTP 503 (`projection_deadline_exhausted`), twice in A-A
and twice in ABBA. Each has one failed warmup, zero measured attempts, and
30 unattempted requests. Their p95/p99 values are null, not zero. No latency
improvement percentage can be computed for that cell.

The earlier A-A child-summary p95 values were 1.96–2.11 ms for one child with
zero history, 57.20–59.48 ms for 50 children with zero history, and
300.96–307.40 ms for one child with 100,000 history. Later small-history old
ABBA runs varied more; the shared-host noise band must not be widened
automatically to declare a threshold passed.

For the 50-child new implementation, increasing unrelated history did not
increase the observed p95; the observed 20% growth ceiling and small-history
non-regression target were met in these short runs. The requested 30% reduction
against the old 50-child large-history p95 remains **indeterminate** because
that baseline is deadline-censored. The one-child large-history result supports
removal of history amplification, not a production latency guarantee.

GNU time process measurements (including setup and SQL probes) for large-history
ABBA runs: old user/system CPU 15.29–16.39/9.26–9.29 seconds and peak RSS
147,112–157,552 KiB; new 4.15–4.35/2.72–2.82 seconds and 89,804–90,904 KiB.
The old runs complete less HTTP work, so these are not equal-work CPU comparisons.

## SQL attribution and remaining limits

The independent read-only probe uses bundled SQLite 3.45.0 and the repository
SQL, not production data or a separately installed SQLite executable:

| Probe, one child | Zero history | 100,000 history | Query plan |
| --- | --- | --- | --- |
| Global reference: returned/decoded rows | 3 | 100,003 | SCAN tasks |
| Global reference: fullscan steps | 2 | 100,002 | temporary ORDER BY B-tree |
| Owner/active: returned/decoded rows | 3 | 3 | SEARCH idx_tasks_owner_active by owner/status |
| Owner/active: fullscan / VM steps | 0 / 93 | 0 / 94 | temporary ORDER BY B-tree over active rows |

Probe duration is not exclusive SQL time inside the HTTP request. Attribution
spans are inclusive and must not be summed as disjoint costs. New 50-child
requests record 50 task-read spans and zero task-sort spans; row counts above
come from the SQL probe, not from span counters.

No cell meets the three-batches-of-1,000-successes p99 reporting threshold.
Unmeasured: 10-child cells, intermediate history sizes, large target-owner
terminal history, varied active-set/payload sizes, unloaded/mixed performance,
inactive lineage scaling, `/snapshot`, concurrency 4/8, fixed-rate saturation,
and preflight rejection/capacity curves. Correctness coverage for unloaded
children and terminal-owner isolation is separate from performance coverage.
This experiment does not establish a general 503 fix or implement stage 2.
