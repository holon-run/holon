# State HTTP byte-retention comparison

`cargo bench --bench state_http` is an opt-in offline fixture for the State
route. It creates a temporary Holon home with `StubProvider`, synthetic
non-runnable Work items and one or more public agents. It never uses the running
daemon, its database, credentials or a real provider. Fixture setup, warmup and
host shutdown are outside the measured windows. Cold cells shut down and reopen
the fixture host without loading its agent runtimes.

The [2026-11-13 experiment report](state-http/results-2026-11-13.md) records
absolute costs and the incomplete concurrent-mutation matrix, not a performance win.

Build and save the exact executable printed by Cargo:

```sh
HOLON_BENCH_SOURCE_SHA256=<source-fingerprint> \
cargo bench --locked --bench state_http --no-run
```

Use an isolated Cargo target if another worktree is building the same package.
Freeze the Rust toolchain, release flags, source fingerprint and fixture settings
for the comparison. Save A with the original State retention and B with only its
completed-byte retention removed; do not add a production benchmark switch.
The fixture source must be identical in both arms. Run A-A calibration before
A-B-B-A, in separate processes and without concurrent benchmarks.

The driver preserves stdout/stderr per cell, records executable hashes and
summaries, and stops the entire invocation on the first unsuccessful cell:

```sh
python3 benchmarks/state-http/run.py \
  --binary-a /absolute/path/to/saved-state-http-A \
  --matrix pilot --order AA --output /absolute/path/to/new-calibration-directory

python3 benchmarks/state-http/run.py \
  --binary-a /absolute/path/to/saved-state-http-A \
  --binary-b /absolute/path/to/saved-state-http-B \
  --matrix full --order ABBA --requests 1000 --batches 3 \
  --output /absolute/path/to/new-comparison-directory
```

`full` has 24 idle cells and four loaded-mutation cells. Its synthetic homes are
under the output directory; the driver refuses to overwrite an existing output.
`--matrix scale` is an additional population matrix, not a change to `full`:
loaded/cold × 1/10 clients × 0/200/2000 synthetic Work items with one observed
Agent. It tests assembly costs beyond the bounded State wire window; it is not
Task history or a real client/socket benchmark. The opt-in
[runtime suite](runtime-suite/README.md) indexes both matrices and D0 without
adding a pull-request gate.
An outer process timeout is a failed/censored cell, not a request percentile or
successful fixture cleanup. Preserve its remaining home for diagnosis.
Timeout stops the driver even with `--keep-going` and returns reserved exit
code `124`, allowing an enclosing suite to terminate the invocation's process
group. Ordinary unsuccessful cells return `1`.

`--matrix idle` and `--matrix mutations` select these groups independently.
For availability diagnosis, `--keep-going` continues with other independent
processes after a nonzero process exit. Failed runs retain their exit status
and streams but have no successful latency summary; the invocation still exits
with status 1. It never retries a request or a failed cell. An outer timeout or
malformed successful artifact still stops the invocation. In particular,
concurrent writes can legitimately expose a typed retryable 503 during the
commit window; report this separately rather than presenting a partial sample
as an all-success latency comparison.

Pilot example (invoke the saved executable, not `cargo bench` for every cell):

```sh
HOLON_STATE_LABEL=A-pilot HOLON_STATE_MODE=loaded \
HOLON_STATE_CLIENTS=1 HOLON_STATE_AGENTS=1 HOLON_STATE_WORK_ITEMS=50 \
HOLON_STATE_REQUESTS=100 HOLON_STATE_BATCHES=3 \
/absolute/path/to/saved-state-http-A > pilot.json
```

| Variable | Default | Range / meaning |
| --- | --- | --- |
| `HOLON_STATE_MODE` | `loaded` | `loaded` or `cold` agent runtimes |
| `HOLON_STATE_AGENTS` | 1 | 1–16 distinct agent keys |
| `HOLON_STATE_CLIENTS` | 1 | 1–10 closed-loop workers sharing one gate |
| `HOLON_STATE_WORK_ITEMS` | 0 | 0–200 per agent |
| `HOLON_STATE_REQUESTS` | 100 | 1–10,000 total per batch, not per worker |
| `HOLON_STATE_BATCHES` | 3 | 1–10 sequential measured windows |
| `HOLON_STATE_PAUSE_MS` | 10 | 0–1000 delay after each worker response |
| `HOLON_STATE_MUTATION_MS` | 0 | Delay after each committed Work update; 0 disables |
| `HOLON_STATE_LABEL` | empty | Explicit arm/run label, not a policy selector |

Mutation cells require loaded agents and at least one Work item. They update a
Work objective round-robin through runtime writer APIs. Record the actual
committed mutation count: the configured delay is not a guaranteed arrival
rate. No cold-runtime mutation coverage is implied.
Each mutation batch must include at least one committed update; otherwise it is
an unsuccessful/coverage-insufficient cell, not a successful idle substitute.

Successful batches report p50/p95 and response byte lengths. p99 is omitted
unless there are at least three batches with 1000 successful requests each.
Gate counters and projection/attribution metrics are before/after process
snapshots; subtract cumulative totals, and do not sum inclusive nested stages.
CPU and Rust requested-allocation deltas include in-process client work and
fixture background work. They are not server-exclusive CPU, SQLite C allocation
or retained-heap measurements. Peak RSS is a process-lifetime high-water mark
including setup, normalized to KiB on macOS and Linux.

Requests use real `Router::oneshot` handlers and collect the entire body, but
exclude socket/transfer cost. The warmup checks the actual active Work count.
Before any gate is warmed, setup waits for each runtime's first Asleep
`agent_state_changed` Audit, then awaits a subsequent original fixture Audit ACK
to serialize behind its publication. A five-second deadline fails setup rather
than accepting a merely transient HTTP 200 or a fixed settling delay. These
setup reads/writes are outside measurement and identical in A and B; cold cells
then reopen their host without loading runtimes.
Before measurement, warmup retries only typed retryable 503 responses within a
five-second setup deadline; the artifact records `warmup_retries`. Measured
requests are never retried.
Each request has a 15-second outer deadline, without changing production gate
budgets. A non-200 response stops new work in that cell; its status and incomplete
sample count remain in the artifact and the process exits unsuccessfully.
Unexpected transport/setup errors are reported on stderr; preserve both streams.
The fixture host is drained before its temporary home is removed, including
measurement errors. If shutdown fails or exceeds its internal deadline, the
home is retained and its path is reported on stderr.

Run the driver acceptance tests with
`python3 -m unittest discover -s benchmarks/state-http -p 'test_*.py'`.

Acceptance must cover loaded/cold, 1/10 clients, single/multiple keys, idle and
loaded mutations, and representative 0/50/200 Work queues. Report any missing
matrix, unsuccessful cells, timeout/cancellation coverage and environmental
interference. This harness does not authorize removing production retention,
set a regression tolerance, demonstrate network capacity or replace focused
State and ProjectionGate fault/lifecycle regressions.
