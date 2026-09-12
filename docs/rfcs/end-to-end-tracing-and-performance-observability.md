---
status: accepted
date: 2026-09-12
---

# RFC: End-to-End Tracing And Performance Observability

## Summary

Holon will use Rust `tracing` as the instrumentation API for a bounded,
content-free observability plane that connects ingress, queueing, turns,
provider calls, tools, local runtime work, persistence, projections, and
delivery.

The runtime will support local diagnosis without an external service. Optional
OTLP and OpenMetrics adapters may export the same instrumentation later.
Tracing is diagnostic evidence, not runtime state, audit evidence, or
operator-facing presentation.

Implementation is split into serial phases. Each phase must land independently
and preserve a useful runtime when later phases are absent.

## Current Structure

The current runtime already has:

- `tracing`-based structured logs and an HTTP `TraceLayer`;
- bounded process-global count, total, average, and maximum metrics;
- detailed read-side attribution counters;
- persisted message, turn, provider-attempt, and tool-execution identifiers;
- opt-in provider HTTP request/response capture for protocol diagnosis.

These facilities are not an end-to-end trace. Core spans do not share an
explicit persisted trace context, percentile metrics are unavailable, local
attribution cannot be assigned to one request, and successful provider calls
do not expose consistent TTFB and streaming phases.

Provider body capture remains a separate sensitive diagnostic facility. It is
not the basis of the normal performance trace.

## Goals

- correlate an ingress event with its asynchronous runtime work and delivery;
- separate queue wait, context build, provider, tool, transition, projection,
  and cleanup time;
- query by trace, turn, message, run, work item, and task identifiers;
- provide p50, p95, and p99 metrics without unbounded label cardinality;
- retain recent, slow, cancelled, and failed traces locally;
- allow optional standard export without coupling runtime code to a vendor;
- keep default instrumentation bounded, content-free, and non-blocking.

## Non-goals

- recording prompts, responses, tool input/output, file contents, or HTTP
  bodies in normal traces;
- replacing audit, transcript, turn, tool evidence, or runtime state tables;
- automatically tracing every Rust function;
- requiring Jaeger, Tempo, Prometheus, or another external backend;
- allowing exporter or diagnostics-writer failure to fail runtime work.

## Trace Context

Holon introduces an explicit persisted context:

```rust
struct TraceContext {
    trace_id: TraceId,
    span_id: SpanId,
    trace_flags: TraceFlags,
    trace_state: Option<String>,
}
```

IDs and header encoding follow W3C Trace Context. External `traceparent` and
`tracestate` values are correlation hints only: they never change origin,
trust, authority, priority, agent scope, or workspace scope.

`MessageEnvelope` carries an optional `TraceContext` so context survives
queueing and daemon restart. Activation, turn, provider, tool, and delivery
work derive children from that context. Fan-out and lifecycle boundaries that
do not have one strict parent use span links.

A WorkItem is not one multi-day open span. Each activation or turn creates a
bounded span. `work_item_id` and `run_id` aggregate related traces across time.

## Stable Span Contract

Initial stable names are:

```text
holon.http.request
holon.ingress.admit
holon.message.enqueue
holon.scheduler.queue_wait
holon.turn
holon.turn.context_build
holon.provider.round
holon.provider.request_build
holon.provider.http
holon.provider.response_headers
holon.provider.stream
holon.provider.parse
holon.provider.retry_backoff
holon.tool.execute
holon.tool.child_process
holon.tool.output_collect
holon.tool.persist
holon.tool.render_for_model
holon.runtime.transition
holon.turn.cleanup
holon.delivery
holon.projection.agent_state
holon.projection.agents_list
holon.db.operation
```

Names are low-cardinality runtime responsibilities. Short, high-frequency
operations may emit a typed event or histogram observation instead of a span.

## Attribute And Metric Boundaries

Trace attributes may contain opaque identifiers such as `agent_id`,
`message_id`, `turn_id`, `run_id`, `work_item_id`, `task_id`, and provider
request IDs.

Metric labels are limited to registered low-cardinality dimensions:

- matched route, HTTP method, and status class;
- stable phase or span name;
- registered tool name;
- provider family and transport;
- outcome or error category;
- retry/fallback disposition;
- origin and authority class.

High-cardinality identifiers, raw model references, URL values, and error text
must not become metric labels.

Normal trace fields use an allowlist. They may contain status, duration,
counts, bytes, tokens, row counts, queue depth, retry counts, and stable
enums. They must not contain prompts, assistant text, tool bodies, HTTP bodies,
URL queries, authorization data, cookies, environment variables, or file
contents.

## Provider And Tool Phases

Each provider attempt records request build, send-to-headers, TTFB, streaming,
parse/validation, retry/backoff, and total duration where the transport exposes
those boundaries. Token/cache usage, attempt number, outcome, provider request
ID, and stable failure category are attributes.

Provider full/failure HTTP trace artifacts may reference trace/span/attempt
IDs. Querying the performance trace returns only an artifact reference and
sensitivity classification; it does not read or expose captured bodies.

Tool spans record registered tool name, outcome, duration, byte counts, and the
persisted tool execution ID. Command tools additionally distinguish admission,
spawn, child execution, output collection, promotion/wait, cancellation,
artifact persistence, and model rendering.

Local spans are added only at stable responsibility boundaries such as runtime
transition, DB operation, projection, scheduler claim/wait, context projection,
memory indexing, and workspace/worktree operations.

## Local Recent And Persistent Storage

The first local layer is a bounded in-memory recent-trace ring. It limits trace
count, spans per trace, events per span, and attribute bytes. Truncation and
dropped-span counts are explicit.

Persistent traces use a separate diagnostics database:

```text
<holon-home>/.holon/diagnostics.sqlite
```

It does not share synchronous span writes with `runtime.sqlite`. A bounded
channel feeds an asynchronous batch writer using an independent WAL. A full
channel drops diagnostic work rather than blocking a turn. Writer queue depth,
drops, and failures are themselves bounded metrics, and writer internals do
not recursively trace their own database writes.

The store contains trace summaries and spans indexed by trace ID, time,
duration, status, and relevant runtime IDs. Removing diagnostics data never
changes runtime state or evidence.

## Sampling And Retention

- aggregate metrics observe 100% of operations;
- recent memory keeps a bounded window;
- error and cancelled traces are retained by default;
- traces above a configurable slow threshold are tail-sampled;
- ordinary successful persistent sampling is configurable and defaults off;
- provider body capture remains separately opt-in and defaults off.

Retention has age and size limits, a minimum recent floor, bounded delete
transactions, and explicit offline compaction. Missing or expired traces are a
valid diagnostic result.

## Query And Export

The local control surface provides:

```text
GET /control/runtime/traces
GET /control/runtime/traces/{trace_id}
GET /control/runtime/traces/search
GET /control/runtime/metrics
```

The CLI provides a minimal waterfall and search by trace, turn, or message.
Both surfaces remain control-authenticated and authority-scoped.

Standard adapters are optional:

1. OTLP trace export;
2. OpenMetrics/Prometheus metrics;
3. JSON-formatted structured logs containing trace/span IDs.

Exporter errors use bounded queues, timeouts, backoff, and drop counters. No
external backend is required for local tracing or metrics.

## Compatibility

- existing `/control/runtime/performance` fields remain available;
- provider full/failure trace environment variables remain compatible;
- audit, transcript, turn, tool, and brief queries do not depend on trace data;
- absent trace context creates a new local root where appropriate;
- retention or exporter failure never constitutes runtime evidence loss.

## Delivery Phases

### Phase 0: contract and baseline

- accept this RFC;
- add a stable four-mode observability-overhead benchmark schema;
- record current `disabled` and `aggregate_only` overhead;
- leave unavailable future modes explicit rather than simulating them.

### Phase 1: propagation and core spans

- add W3C-compatible `TraceContext`;
- persist and propagate context across ingress, queue, restart, turn, provider,
  tool, and delivery;
- add typed attribute helpers, JSON correlation, a recent ring, and minimal
  control API/CLI queries.

### Phase 2: detailed attribution

- add provider TTFB/stream/parse/retry detail;
- add command-tool subphases and stable local runtime boundaries;
- cross-reference provider body-trace artifacts.

### Phase 3: histograms and persistent diagnostics

- add bounded histograms and p50/p95/p99;
- add independent diagnostics SQLite, asynchronous batch writing,
  slow/error sampling, retention, drop accounting, and trace search.

### Phase 4: standard export

- add optional OTLP trace export;
- add protected OpenMetrics output;
- document collector/dashboard/alert baselines;
- test label cardinality and exporter failure isolation.

Each phase is a separate PR based on the previous phase after merge.

## Verification And Performance Gates

Unit and integration tests cover ID/header parsing, parent/link semantics,
restart propagation, sensitive-field rejection, truncation, sampling,
retention, provider streaming/retry, long-running tools, and exporter/storage
failure.

The benchmark contract has four modes:

- `disabled`;
- `aggregate_only`;
- `sampled_local_trace`;
- `full_trace`.

Phase 0 implements the first two and reports the latter two as unavailable.
Later phases must populate the same schema when their runtime layers exist.
Use:

```bash
HOLON_BENCH_SAMPLES=20 \
HOLON_BENCH_OPERATIONS=100000 \
cargo bench --locked --bench observability_overhead
```

### Phase 0 measured baseline

The Phase 0 baseline was recorded on 2026-09-12 at revision `9ac7d26e`
(`x86_64-unknown-linux-gnu`, Rust 1.91.1, Linux 6.8.0, AMD Ryzen Threadripper
PRO 3975WX). The run used 3 warmups, 20 measured repetitions, and 100,000
operations per repetition:

| Mode | Median | p95 | Maximum | Peak process RSS |
| --- | ---: | ---: | ---: | ---: |
| `disabled` | 0.236 ns/op | 0.270 ns/op | 0.270 ns/op | 90.3 MiB |
| `aggregate_only` | 8.511 ns/op | 8.634 ns/op | 8.634 ns/op | 90.3 MiB |
| `sampled_local_trace` | unavailable until Phase 1 | unavailable | unavailable | unavailable |
| `full_trace` | unavailable until Phase 3 | unavailable | unavailable | unavailable |

The measured aggregate-recording increment is 8.365 ns/op at p95. Peak RSS
is process-wide high-water usage, not per-mode allocation, so this run only
establishes a runner footprint and does not claim zero incremental allocation.

The disabled workload is intentionally an almost empty loop. Its relative
percentage is therefore numerically unstable and must not be used to evaluate
the end-to-end percentage gates below. This microbenchmark establishes an
absolute atomic-recording budget and a stable artifact schema. Relative gates
must be evaluated by running each mode around the same representative runtime
workload.

Initial acceptance targets are:

- metrics-only p95 overhead at or below 3%;
- default sampled p95 overhead at or below 5%;
- no synchronous network wait when export is disabled or unavailable;
- bounded memory under a fixed workload;
- no latency growth proportional to a full diagnostics-writer backlog.

Microbenchmark percentages are compared from raw per-operation samples and are
not a substitute for end-to-end runtime benchmarks.
