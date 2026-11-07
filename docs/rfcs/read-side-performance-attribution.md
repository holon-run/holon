# Read-side performance attribution

Status: diagnostic implementation, not a scheduling or persistence change.

### Cold Work queue decode admission

Cold State keeps the original WorkItem material holder through the last response
owner. Its direct typed JSON decode counts JSON value cells (including inline
scalars, nested containers and array entries), not struct field names or the
completion aggregate/resolver copies it does not create. The allocation-free
shape scan runs before decoding and retains its peak so malformed trailing
punctuation cannot erase an allocating prefix. The existing conservative byte
envelope,
eightfold capacity allowance, default limits and failed-phase latch are unchanged;
completion and typed-clone admission retain their separate envelopes.
The execution-protocol source-map loader uses this same direct typed-decode
admission while separately charging its map-entry metadata.
The recent-brief source likewise admits direct `BriefRecord` decoding without
completion aggregate/resolver copies; raw rows and output slots remain charged.
Loaded schedulers read the original 64-brief window only when a runtime error
was observed: successful briefs can clear an error, but cannot create one.
No-error projections do not acquire unrelated brief bodies; error projections
retain the original ordering, window and decoding admission.
The cold State scheduler keeps its runnable selection as `StateWorkRecord`
loans, charging only the new selection slots rather than cloning bodies already
held by State. The loans preserve the original acquisition through their last
owner. Owned/Context selection, current/waiting clones and canonical-map slots
retain their existing admission; selection order and serialized contents do not
change.

The cold State queue indexes each complete owned Work record by its own `id`,
without allocating a duplicate key string. Its in-place id/descending-source
ordinal sort and dedup preserve last-source-wins and the original record bodies.
Entry slots are admitted before growth using their actual tuple size and the
unchanged eightfold allowance. The generic independently keyed queue adapter,
complete typed decode, grouping, selection and material-holder lifetime remain
unchanged.

The cold State adapter wraps canonical records before deriving scheduling
projections and builds the final shared-record projection vector directly. It
does not allocate an intermediate owned-record projection vector. The final
slots, shared record headers, pins and display groups retain their admission;
the original acquisition checks errors before and after the whole build.
Scheduling sort order, complete source inventory and last-holder lifetime are
unchanged.

State's HTTP DTO conversion consumes the already-owned agent/model fields rather
than cloning full source records. Internal working memory, resolved model policy
and closure evidence do not become response copies. Workspace path conversion,
the session run-id/terminal copy, terminal/failure slimming and child DTO copies
retain pre-allocation admission and the existing conservative clone envelope.
The borrowed DTO adapters and wire contents are unchanged.

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

The queue attribution fixture initializes the complete actual reader pool via
`cold_fixture::warm_readers` before taking its sole FD baseline. The typed setup
entry point retains all pool checkouts together and reads the schema on each,
so SQLite's lazy WAL open cannot first occur in the measured operation. It
exposes neither SQL callbacks nor connections, honors protection and the normal
acquisition budget, and does not change production lazy initialization. Warming
independent raw connections or only as many readers as measurement workers does
not establish full-pool steady state. Exact before/after FD equality, workload,
concurrency and time limits remain unchanged; no tolerance, sleep or post-failure
baseline reset is allowed. Historical failures remain failures.

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

## Sustained HTTP comparisons

The optional `HOLON_BENCH_WINDOW_SECS=10..30` mode of `http_control` accumulates
at least that much **timed request duration per case**, and at least 100 batches.
It first warms each case for one second of request duration. Setup, router
construction, byte-cache filling and diagnostics are excluded from request
timing; total sampling wall time is recorded separately and can be longer.
Without the variable, the existing short CI workload is unchanged.

Gate-miss observations use a fresh router and require exactly one leader and
no hits per request. Gate-hit observations prime a fresh router outside timing,
then require all 256 requests in a measured batch to hit, with no new leader.
TTL expiry fails the entire run; samples are never silently dropped or retried.
These batches amortize setup without extending TTL or changing cache behavior.
The mixed-cache roster case also uses 256-request batches; other existing batch
sizes remain unchanged. A batch mean is **not** a single-request p95/p99.
The `observation` object records actual warmup, batch and operation counts;
the older config repetition fields remain requested short-workload settings.

Build both arms with the identical observation harness and identical compiler,
flags and resource constraints. The baseline must be an explicitly recorded
source revision with a valid comparable fixture, not a historical JSON artifact
from another harness. Both binaries must implement the `v3-window` workload.
For each arm, compute and embed its source fingerprint at compile time, using
the same driver to hash that arm's HEAD, tree, dirty flag and source contents:

```bash
export HOLON_BENCH_SOURCE_SHA256="$(python3 /path/to/observe_http_performance.py \
  --source-fingerprint /path/to/arm-worktree)"
# From that arm's worktree, with the same toolchain/flags for both arms:
cargo build --bench http_control
```

Freeze sources through compilation and collection. Cargo tracks the `option_env!`
value in the benchmark; changing the fingerprint rebuilds it. Window mode rejects
builds without a fingerprint, and collection/re-analysis checks the embedded
value against the recorded source snapshot. Runtime `git rev-parse` is only an
additional working-directory check, not evidence of binary build provenance.
Short CI workloads do not require the fingerprint.
Run the checked-in driver under one common CPU/memory scope, without overlapping
builds or tests:

```bash
python3 scripts/observe_http_performance.py \
  --baseline /path/to/baseline-http-control \
  --baseline-cwd /path/to/baseline-worktree \
  --candidate /path/to/candidate-http-control \
  --candidate-cwd /path/to/candidate-worktree \
  --output /path/to/new-observation-directory --seconds 10 --rounds 6
```

Six paired rounds balance baseline/candidate order (`AB`, `BA`, repeated); at
least five pairs are required. A/A controls can use the same binary/worktree for
both arms. The runner records source contents, binary and artifact SHA-256,
environment, order, failure/timeout status and raw samples. It rejects source
or binary mutation and incompatible configurations; no failed run becomes a
passing observation. Re-analysis uses `--analyze /path/to/observation-directory`.

Reports classify each case as improvement, regression or uncertain from a
seeded paired bootstrap interval of independent run medians. Report absolute
and relative changes and all run values, rather than treating thousands of
requests as independent experimental repetitions. These small-sample intervals
are descriptive, not a production SLO or a new pass/fail performance gate.
Missing stage counters are unevaluated, not zero; zero attempts cannot estimate
per-call latency. Stage count/time/row deltas are normalized per timed request,
not compared as raw totals from differently sized samples. Nested inclusive
stages overlap, and their differences do not exhaustively explain wall time.
Real transport, concurrent throughput and per-request tails remain separate
experiments; this mode measures only warm in-process routes.
