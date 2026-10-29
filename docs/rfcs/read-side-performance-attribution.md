# Read-side performance attribution

Status: diagnostic implementation, not a scheduling or persistence change.

## Contract

The performance diagnostics snapshot adds `attribution`, a fixed-size set of
named stage counters. Each reports attempts, total/max nanoseconds and rows.
Existing millisecond metrics retain their old meaning. Nanoseconds avoid
per-operation millisecond truncation; they do not imply clock accuracy.

Timers include failed/cancelled attempts. Row counts report successfully
materialized rows (latest work items), or rows entering wait filtering.
The existing wait/work-item query stages include connection acquisition, SQL
execution and decoding; they are not SQL-engine-only timers. The live-scope
filter and per-item lookup timers make global scan and N+1 amplification observable.

Connection stages distinguish SQLite opening, configuration and the two Linux
sidecar consistency checks. Safety checks remain unchanged. Agent lock wait and
state cloning are distinct; lightweight state projection additionally times
posture, closure, children, identity and active task reads.

Reader-pool stages additionally distinguish asynchronous semaphore admission,
blocking-task admission, checkout (including protection checks), lease holding
through connection return, and the synchronous read closure. These count
attempts, including errors/cancellation. The closure includes transactions and
row decoding; neither it nor lease holding is a SQL-engine-only measurement.
Synchronous callers do not pass through asynchronous admission. A cancelled
queued blocking closure settles its timer when its captured state is dropped;
started reads keep their existing acquisition-only timeout semantics.

Roster and per-Agent anchor reads have operation-level timers that include
checkout. Their latest-Brief query timers, and the index-outbox pending count
and oldest query timers, start *inside* the read closure and exclude checkout.
Capability verification, gate decision, follower waiting and leader building
are distinct stages. Existing gate hit/miss/leader/stale counters distinguish
request volume from actual builds; none of these diagnostics adds a deadline,
interrupts SQLite, changes cache freshness or changes reader capacity.
Preflight admission waiting is its own fixed stage: it starts when the single
capability-preflight worker slot is busy and stops when the caller receives
the worker permit, so it excludes verification itself and never carries agent,
principal or key labels.

Counters are process-global relaxed atomics, not a request-level transaction.
Nested stages overlap and must not be summed as independent wall time.
Concurrent stage totals can exceed interval wall time. Snapshot subtraction
should use count, total_ns and rows, not subtract cumulative maxima.

`holon::performance=trace` emits only fixed stage names, elapsed nanoseconds
and numeric row counts, in the caller's tracing context where available.
No message bodies, SQL values, agent IDs or database paths are emitted.
Per-operation trace logging is opt-in and can perturb measured performance.
There is no promise of end-to-end request correlation yet.

## Verification and limits

Isolated benchmarks report raw operation samples and quantiles, together with
diagnostic deltas. Live counters intentionally do not maintain a percentile
reservoir: no unbounded cardinality, storage or background sampler is added.
Small benchmark sample p95/p99 values are descriptive order statistics, not
reliable production tail estimates.

Scale one dimension per fixture before combining growth and contention.
Temporary databases must not read production records. A synthetic result can
validate a mechanism but cannot establish its contribution to live latency.

The bounded latest-Brief workload uses the production SQL and bundled SQLite:

```bash
cargo test --lib runtime_db::brief_latest_tests::brief_latest_history_benchmark \
  -- --exact --ignored --nocapture
```

It emits a `brief_latest_benchmark=` JSON line. `HOLON_BENCH_SAMPLES` selects
20–200 measured executions per arm (default 40). Three A/B/A rounds each reset
the same synthetic database snapshot, with or without the candidate nonunique
`(agent_id, created_at DESC, evidence_id DESC)` index. SQL preparation is
excluded from stepped/decoded execution timing; sort and VM-step counters,
roster reads through the default reader pool, serial writer commits, index
build time and database/WAL end snapshots are reported separately.

The separately runnable `brief_latest_migration_benchmark` uses the same bounded
history in a real predecessor schema and measures complete upgrade opens and
successful reopens. See the [index decision](../implementation-decisions/roster-brief-index-measurement-gate.md)
for the accepted write cost, migration semantics and downgrade boundary.

Neither is a production migration/capacity benchmark, peak
temporary-disk measurement, HTTP load test or frontend request-count test.
Run it alone before interpreting global diagnostic deltas; shared-host noise
and warm filesystem caches remain even with an identical logical snapshot.
