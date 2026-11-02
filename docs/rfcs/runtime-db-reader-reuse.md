# Bounded RuntimeDb reader reuse

Status: stage 1 merged in #3662; the remaining-read P0/P1 implementation was
approved on 2026-10-21. The explicit pool-level connection-creation retry
contract below remains unchanged. No production rollout authorization.

## Current structure and scope

`RuntimeDbWriter` already reuses its dedicated connection and serializes writes.
The batched agent-list projection collects all roster facts in one deferred
read transaction and now reuses a bounded reader. Before P1, most other
repositories still used the mixed-role `RuntimeDb::connection()` API.

Stage 1 adds an independently bounded read pool and migrates only the batched
list projection. P1 extends the same pool to remaining runtime pure reads;
the [method inventory and raw exceptions](../implementation-decisions/runtime-db-read-coverage.md)
record its scope. Neither phase changes SQL selection, canonical fences, poll
intervals, write ordering, schema, or the raw connection API. Query reduction
and Linux sidecar-check optimization remain separate, unimplemented work.

### P1 call-site integration

Repository methods collect owned results inside short reader closures. Shared
helpers take an existing connection rather than recursively checking out.
Multi-chunk lookups release each chunk before unrelated repository calls.
The outbox produced-watermark fallback uses its existing connection; the
acknowledged-row preflight returns its lease before the writer transaction.

Async monitor evidence and foreground task/wait reads need explicit blocking
boundaries; daemon outbox enumeration uses asynchronous reader admission.
Pool saturation must propagate before interpreting missing/consumed records.
Invocation and message monitors retain responsibility on retryable reader
failure instead of producing a terminal failure or initiating child cleanup.
Activation preserves the underlying retryable error type and constructs its
runtime storage on a blocking worker. A host-owned cold-activation task retains
the handoff and registry guards through construction and registration, even when
the requester is cancelled: construction can write storage. Temporary runtime
handoff owns a cleanup guard, so cancellation also stops the unclaimed runtime
and archives its ephemeral identity.
A delivery monitor validates identity once, then retains that identity
while observing canonical delivery evidence: a later deletion fence must produce
the recorded cancellation rather than replace it with an identity error.
Daemon identity discovery, reply reconciliation, and startup template candidate
selection collect owned snapshots on blocking workers before continuing async
work. Reconciliation settles its already-matched prefix before reporting a later
read error or missing receipt, preserving the original per-task ordering.
Template failure recording must preserve an existing catalog on read/decode
failure, releasing the read lease before its normal upsert.

P1 correctness, async saturation, independent review, and fixed-work A/B/A
verification are required before publication. The synchronous microbenchmark
alone does not validate monitor semantics or production performance. P2 is
observation only: no sidecar protection or cache change is authorized here.

## Read contract

Clones of a RuntimeDb share one lazily used reader pool. Independently opened
RuntimeDb handles and raw maintenance connections are separate owners and must
not be counted as part of the same pool limit. The initial candidate capacity is
four readers with a 250 ms acquisition budget, not a claim of optimal sizing.
There is no overflow connection or fallback to unbounded raw opens.

Internal closure APIs return owned values, never a lease, transaction, or cursor.
The synchronous API is for synchronous/blocking boundaries. The async API waits
for bounded, cancellable admission before `spawn_blocking`. The same acquisition
deadline covers unstarted blocking-queue work, which returns a retryable error
without running its read closure after expiration. The started closure
retains admission and its connection until it actually finishes, even if its
awaiting caller is cancelled. Checkout budget does not bound SQLite execution
or change the writer's busy timeout. Recursive checkout, awaiting with a lease,
or writing while holding a read transaction is prohibited.

Read scopes use the real monotonic clock. Async admission translates the
remaining allowance once to the executor's clock and shares that deadline
between permit acquisition and blocking-queue admission; a shifted executor
clock must not reinterpret an absolute real-clock timestamp as already expired.

Readers use the established opener and protection checks, followed by
`query_only=ON`. This is an application contract, not an OS read-only boundary.
Persistent WAL configuration occurs at physical open, not on every checkout.
No SQLite shared-cache or global mutex setting is changed.

Each list operation owns one deferred transaction, ended before returning the
connection. An idle connection has no read snapshot, so subsequent requests
observe new commits and idle reuse does not pin WAL checkpoints.

## Failure and lifecycle boundaries

Protection is checked before executing a read and again before accepting its
result. Quarantine and sidecar identity errors retain their existing evidence;
reuse must never accept stale-file results or repair/delete sidecars.
Connections and creation work retain the protection owner until they close.

Panic, an unfinished transaction, or a connection that cannot be proved clean
causes discard. Recoverable query errors do not automatically force reconnect.
Business closures must not change connection hooks, authorizers, attachments,
temporary schema, or connection PRAGMAs. Pool internals must not capture the
complete RuntimeDb or append sender and create an ownership cycle.

No request can outlive its owner while leaving an unowned connection behind.
The last owner stops new use; existing read/creation owners keep protection
alive through actual completion and close. No pool timer may periodically
replace healthy idle connections. Mature pool integration is preferred only
if its creation, retry, error, and final-drop behavior meets this contract;
otherwise implementation stops for review rather than relaxing protection.

## Verification and comparison

`cargo bench --bench runtime_db` includes
`HOLON_BENCH_FILTER=projection.agent_list.reader_reuse`. It creates isolated
synthetic databases, uses 1/60 agents and 1/4/16 workers, and runs equal read work
with either no writes or a fixed burst of dedicated-writer transactions.
`HOLON_BENCH_READER_OPERATIONS` controls operations per case (default 256,
divisible by 16); `HOLON_BENCH_SAMPLES` controls repeated cases.

The same benchmark source must run on a fixed pre-pool revision containing
#3649 and on the implementation. Preserve raw samples, build/fixture identity,
CPU user/system work, read and writer latency quantiles, physical-open counter
deltas, descriptors, RSS, and WAL snapshots. Fixture first-read timing includes
lazy initialization but is not a cold filesystem-cache experiment. Repeat A/B/A
to assess host noise; do not add overhead from unrelated builds to measured runs.

Structural acceptance requires bounded physical opens under healthy warm
reuse, independent of request count. Correctness gates cover exhaustion,
cancellation before and after start, panic/error return, short transactions,
concurrent committed views, protection loss, and final owner teardown. Existing
list field/filter/degradation semantics and writer/task/wait fences must remain.
Capacity one tests lock-order and nested-acquisition risks.

Synthetic results validate a mechanism, not live CPU attribution or elimination
of snapshot failures. Reader pool and query-reduction benefits must be measured
separately. No production database or service is stressed by this benchmark.
Rollback removes the reader integration without reverting the earlier batched
list fix; no data migration or WAL deletion is needed.

### Phase 1 list benchmark (2026-10-21)

The fixed pre-pool revision is `ee283cfd5990a570700c341d2f36c045d55e9e27`.
The unchanged benchmark source ran five repetitions of all 12
agents/workers/writer combinations on each side: 15,360 reads and 1,920 writes
per artifact. Against both pre- and post-implementation controls, pooled
process CPU fell 71.2–99.5% and read p95 fell 62.6–97.4%. Healthy warm reads
opened zero new physical connections, versus 15,360 in each control.

All groups met the predeclared CPU and latency gates. Writer p95 increased by
up to 0.044495ms; some relative ratios exceeded 110%, but remained inside the
predeclared absolute +1ms tolerance for sub-millisecond baselines. This is not
evidence that writer latency is unchanged. Control CPU medians drifted from
-15.8% to +13.8%; even comparing pooled maximum CPU with control minimum CPU
within each group gave a worst ratio of 34.0%.

Pool capacity was fixed at four; this does not establish the optimal capacity.
The controls bracket the experiment but the earlier control preceded intervening
builds/tests by about an hour. Measurements exclude concurrent builds and do
not establish cold-cache, HTTP, production, memory, or long-term WAL benefits.
Raw samples, source/binary hashes, resource snapshots, and both controls are
retained in the local comparison artifacts.

The unchanged `performance_attribution` benchmark also ran 15 cases and 600
operations per side. Fixture counts, wait linkage, raw percentile recomputation,
worker/operation identities, stable per-case FD counts, and attribution
count/row deltas passed validation. Its unmigrated legacy connection and
work-queue paths still opened 40 and 240 connections per 40-operation case,
respectively. These are scope/counter controls, not evidence of pool benefits:
their fixed-order, small-sample timing differences are not causal conclusions.

## Approved connection-creation retry contract (2026-10-21)

The synchronous adapter is r2d2 0.8.10 with a local rusqlite manager, not an
adapter that introduces another rusqlite/SQLite dependency. Failed physical
connection creation may retry at pool scope, independently of the request's
checkout timeout. Existing plus pending connections remain bounded by pool
capacity. This never retries business SQL, repairs data, or bypasses protection.

Set the pool's connection timeout separately from each request's 250 ms checkout
budget: r2d2 also uses half that pool timeout as its creation-backoff ceiling.
Keep the pool timeout at 30 seconds, with no minimum idle reserve, no idle
timeout, no maximum lifetime, and no checkout schema probe. These settings
avoid periodic churn but do not disable failed-creation retries; r2d2 exposes
no separate retry-count or backoff switch. Creation attempts/failures and
checkout timeouts must be observable, with rate-limited failure logging.

Creation work and physical connections must retain the protection owner even
after the last public RuntimeDb clone is dropped. That owner must not retain
the append sender or the pool. In-flight creation is not forcibly cancellable;
queued retry work must not keep a dead pool alive. Tests must establish the
actual connection-close/guard-release order, including persistent creation
failure, quarantine, and teardown concurrent with creation.

Source: [r2d2 0.8.10 connection creation](https://raw.githubusercontent.com/sfackler/r2d2/v0.8.10/src/lib.rs).

The operator explicitly approved this revised retry boundary at 13:14 UTC.
Unbounded overflow, a custom general-purpose pool scheduler, and production
deployment remain outside scope.
