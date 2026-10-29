# Real-Repo Benchmark Tasks

This directory contains repo-local benchmark examples and replay inputs.

Real-repo benchmarks are modeled as issue-driven operator assignments:

- Runner input is built from a shared issue template, not from a task-specific long prompt body.
- The template tells agents to use `gh` to inspect the issue and related GitHub context.
- Verification and scope rules remain defined in manifest metadata, with `evaluation.scope_policy` used during result scoring.
- Exact verifier commands remain benchmark-only and are not injected verbatim into the runner prompt.
- `benchmark.mode` distinguishes `live` from `replay`.
- `evaluation.scope_policy` defaults to a diagnostic metric when set to `soft`, and only becomes a hard failure for constrained tasks when set to `hard`.
- `evaluation.expected_outcome` defines whether a task requires a diff, expects a grounded no-op, or allows either.
- Suite `pr` config defines publish behavior structurally:
  - `submit_pr`
  - `draft_pr`
  - `push_branch`
- The runner renders PR policy into natural language in the issue template.
- Runner IDs are suite-defined unique lowercase slugs. Optional `transport` and
  `endpoint` fields label route-specific comparisons.
- Suites may set `repetitions` and deterministic `execution` controls:
  - `runner_order: configured | paired_randomized | alternating`
  - `random_seed`
  - `max_parallel_runners`
  - `cooldown_ms`
- Every task/repetition is one paired group. Paired order modes require serial
  runner execution. Run, branch, worktree, artifact,
  and Holon agent identities include the repetition so state cannot leak
  between samples.
- Non-PR runs execute the manifest verifier after the runner. PR runs continue
  to use GitHub CI as their verification source.
- Raw per-run summaries remain under each `run-NN` directory. Suites also emit
  `paired-summary.json` and `paired-summary.md`.
- `holon-openai` live benchmark runs now set `HOLON_DISABLE_PROVIDER_FALLBACK=1` so deterministic live comparisons do not silently switch to a fallback provider/model.
- Codex live runs now use the configured or default shared `CODEX_HOME`/user environment by default rather than an isolated benchmark-specific home.

Live head-to-head benchmark tasks are intentionally kept outside the tested repository so agents cannot recover the task goal by reading benchmark manifests from the workspace. In this environment they live under:

- `workspace/projects/holon-run/holon/benchmarks/`

Repo-local layout:

- `tasks/<task_id>.yaml`
- `suites/<suite_id>.yaml`

Manifest and suite validation is implemented in
`benchmark/lib/manifest.mjs`. Task manifests require the repository, issue,
base revision, benchmark mode, task kind, evaluation policy, budget, review,
and metadata groups; suites require task references, runner definitions, PR
policy, and timeout configuration. Use `validate-manifest` before a real-repo
run when checking a new task.

Use:

```bash
node benchmark/run.mjs validate-manifest --manifest benchmarks/tasks/holon-1611-tool-guidance-markdown.yaml
node benchmark/run.mjs validate-manifest --manifest benchmarks/tasks/holon-1764-runtime-performance-diagnostics.yaml
node benchmark/run.mjs real --manifest /absolute/path/to/workspace/projects/holon-run/holon/benchmarks/tasks/holon-0050-runtime-result-closure.yaml --runner holon-openai --runner codex-openai --label bench-live-0050
node benchmark/run.mjs suite --suite benchmarks/suites/openai-phase1.local.yaml --label bench-openai-phase1
node benchmark/run.mjs suite --suite benchmarks/suites/performance-diagnostics.local.yaml --label bench-perf-diagnostics
```

The checked-in DeepSeek pilot suite compares the two canonical routes with five
seeded, serial repetitions:

```bash
node benchmark/run.mjs suite --suite benchmarks/suites/deepseek-transport-pilot.local.yaml --label deepseek-transport-pilot
```

This command uses paid provider traffic. Run it only with operator authorization
and required credentials. The suite disables provider fallback and does not
create PRs or change the default DeepSeek route.

Node harness results are rooted at `.benchmark-results/`. Individual
task/runner repetitions are stored below the selected label, and suite runs
also emit `paired-summary.json` and `paired-summary.md`. The default CI
workflow does not execute these real-repo commands or upload this directory;
its benchmark job uploads the Rust performance JSON files under
`target/performance/`.

To push benchmark branches and create draft PRs, either:

- set `pr.submit_pr: true` and `pr.draft_pr: true` in the suite file, or
- override on the CLI with `--push-branch --github-pr`.

## Local runtime performance baseline

The Rust runtime database and summary-storage projection baseline is local and
does not use provider traffic:

```bash
cargo bench --bench runtime_db
cargo bench --bench scheduler
cargo bench --bench http_control
cargo bench --bench memory_lifecycle
```

It emits one JSON artifact to stdout. Set `HOLON_BENCH_SAMPLES` to change the
default ten measured repetitions; each benchmark also performs one warmup.
The multi-workload targets accept `HOLON_BENCH_FILTER` as a comma-separated
list of exact benchmark IDs, and do not execute unselected workloads. The
scheduler suite covers SQLite-backed work-queue projection, due recheck and
active wait scans, plus concurrent projection reads with FIFO-order validation.
The HTTP suite constructs 101 visible agents and 200 work items before timing,
then covers the runtime performance snapshot, agent roster, and single or
repeated large work-item responses through in-process `Router::oneshot`
requests. It does not bind a socket or include network transfer time.

The HTTP suite also has explicitly selected child-state workloads. They use a
temporary SQLite database, `StubProvider`, and real uncached parent `/state`
requests; fixture setup is outside the request timer:

```bash
HOLON_BENCH_FILTER=http.child_state.loaded.0,http.child_state.loaded.1,http.child_state.loaded.50 \
HOLON_BENCH_SAMPLES=3 HOLON_BENCH_CHILD_REQUESTS=3 \
cargo bench --bench http_control > child-state.json
```

Each child owns three active tasks (queued/running/cancelling). History defaults
to zero; `HOLON_BENCH_CHILD_HISTORY=100000` explicitly adds unrelated terminal
tasks. Requests are serial and bounded by `HOLON_BENCH_CHILD_BUDGET_MS` (default
60000 per cell) and `HOLON_BENCH_CHILD_TIMEOUT_MS` (default 15000 per request).
These are outer harness limits, not overrides of production projection gates.
The harness stops at the first unsuccessful request and skips all subsequent
workloads, including non-child workloads in a mixed filter; check `warmup`,
`outcomes`, and `load.aborted`, not just the command exit status.
Each fixture host is shut down before its temporary home is removed, including
setup/measurement failures and early aborts. Shutdown is outside request timing;
failure to finish within 15 seconds aborts the suite rather than starting
another cell. Completed cells record this in `fixture_cleanup`.

Beyond the serial fresh-AppState workload, the `child-state.v3` fixture also
supports shared-gate cells: `HOLON_BENCH_CHILD_CONCURRENCY` closed-loop workers
share one AppState and projection gate; `HOLON_BENCH_CHILD_ROUTE_MIX=state|snapshot|mixed`
selects routes (the snapshot routes exercise the per-request capability
preflight lane, `/state` does not); `HOLON_BENCH_CHILD_PARENTS` creates distinct
parent state keys. Saturation cells set `HOLON_BENCH_CHILD_ARRIVAL_PER_SEC`
with a `HOLON_BENCH_CHILD_MAX_INFLIGHT` bound; there, classified queue-full 503
responses are expected outcomes recorded per reason under `failure_reasons`
instead of aborting the cell, while any other failure still stops it.
`HOLON_BENCH_CHILD_LOAD_MODE=loaded|unloaded|mixed` selects whether children
are runtime-loaded or storage-backed, and benchmark ids are
`http.child_state.<load_mode>.<children>`. Concurrent cells report one
before/after cell-level stage and gate delta instead of summing overlapping
per-request stage counters.

`child_state` results include HTTP/child-summary p50 and p95, CPU and peak RSS,
inclusive attribution-stage deltas, and independent read-only SQL probes with
query plans, fullscan/VM steps, and returned/decoded row counts. Nested stages
overlap; SQL probes are not request timings. p99 stays null unless every one of
at least three batches has 1000 successful requests.
For comparisons, save old/new executables, run A-A then A-B-B-A with the same
fixture/settings, and label each invocation with `HOLON_BENCH_COMPARISON=AA`
or `ABBA` and `HOLON_BENCH_POSITION` (one-based). Labels do not switch code.
Avoid concurrent benchmarks, preserve failures, and report missing concurrency
or `/snapshot` coverage rather than inferring production capacity or 503 fixes.
See the [stage 1 child-state results](child-state-stage1.md) for the bounded
#3745 comparison, deadline-censored baseline, and unmeasured matrix.
See the [preflight admission queue results](projection-preflight-queue-3745-3800.md)
for the shared-gate #3745/#3800 acceptance cells and their uncovered matrix.

The memory suite isolates every sample in a child process, grows a session with
1,000 events and 100 briefs, repeatedly reads bounded history windows, and
records current/peak RSS plus SQLite directory growth before and after dropping
the runtime storage handle.

`Benchmark CI` runs a stable subset with broad catastrophic-regression limits.
It excludes the scheduler `concurrent_8x25` workload because its runtime and
variance are strongly machine-dependent. JSON artifacts are uploaded for trend
analysis; initial hard limits catch order-of-magnitude wall-time,
retained-memory, or database-size regressions rather than small cross-run
changes.
