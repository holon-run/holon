# Read-side performance attribution

Status: diagnostic implementation, not a scheduling or persistence change.

## Contract

The performance diagnostics snapshot adds `attribution`, a fixed-size set of
named stage counters. Each reports attempts, total/max nanoseconds and rows.
Existing millisecond metrics retain their old meaning. Nanoseconds avoid
per-operation millisecond truncation; they do not imply clock accuracy.

Timers include failed/cancelled attempts. Row counts report successfully
materialized rows (latest work items), or rows entering wait filtering.
Query stages include connection acquisition, SQL execution and decoding;
they are not SQL-engine-only timers. The live-scope filter and per-item lookup
timers make global scan and N+1 amplification observable.

Connection stages distinguish SQLite opening, configuration and the two Linux
sidecar consistency checks. Safety checks remain unchanged. Agent lock wait and
state cloning are distinct; lightweight state projection additionally times
posture, closure, children, identity and active task reads.

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
