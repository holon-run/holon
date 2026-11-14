# Opt-in runtime baseline

This entry point makes **data growth and omitted coverage visible**, without
adding work to pull-request CI. It reuses the State HTTP and D0 drivers rather
than giving reduced fixtures their names. The machine-readable
[coverage map](coverage.json) distinguishes implemented scenarios, limitations
and unmeasured work. No workflow, timer or production probe is installed.

The [2026-11-14 scale run](results/2026-11-14-scale.md) records a failed,
partially measured run, including a timeout and overlapping host build load;
it is not a passing or isolated performance baseline.

## Workloads

| Profile | Workload | Independent process repeats |
| --- | --- | --- |
| `scale` and `full` | Task-result Message history: 0, 2k, 20k, 100k extra rows, fixed selected/active set | `--repeats`, default 2 |
| `scale` and `full` | State scale: loaded/cold × 1/10 clients × 0/200/2000 Work, one Agent | AA, 2 per cell |
| `full` only | Original State full matrix: 24 idle + 4 mutation cells | AA, 2 per cell |
| `full` only | Complete D0 normal and pressure | `--repeats`, default 2 |

Task history growth and State resident Work growth are different dimensions.
Task coverage is **partial**: public evidence-by-ID/source/record reads only.
Output-head preparation and `RuntimeHandle::task_output` selected-output reads
remain `not_measured`, including #4105's historical scan. Raw storage writes
cannot establish the runtime's native ACK-qualified fixture; this suite neither
fabricates that state nor exposes private production APIs to bypass it.
The State response wire window is bounded, but constructing it need not have
constant cost as the resident population grows. Both setup and warmup stay
outside request measurements; setup is still charged to the outer time budget.

D0 retains 100k historical messages, 64 linked turn bundles, 100 queued 16KiB
inputs and a 10MiB tool record **per Agent**. Normal is 8 loaded/4 concurrent;
pressure is 32/16. The suite fixes normal warm/measured sampling to 13/32 and
pressure to 4/8 and requires at least 100 actual context timing samples.
Admission must use Ready without SQLite or owner reservation.
See [D0's contract](../agent-state/README.md).

`scale` is a separately named population baseline, **not** a passing full matrix.
It does not silently substitute a smaller D0. `full` describes the listed
workloads, not universal runtime coverage: idle maintenance, real client/socket
soak, foreground TaskResult fairness and historical positive controls remain
explicit gaps. See [State measurement limitations](../state-http.md) and
[Task history contract](../task-history-scale.md).

## Freeze, build, run

Use a purpose-named worktree, a synthetic fixture home and a coordinated window
with no competing build/benchmark. These commands must not target a production
home or production database. The driver uses stub/scripted providers only.
Use absolute fresh artifact directories; never overwrite a failed experiment.

```sh
python3 benchmarks/runtime-suite/build.py \
  --toolchain 1.100.0 --target-dir /absolute/private-target \
  --output /absolute/fresh-build

python3 benchmarks/runtime-suite/run.py \
  --build-manifest /absolute/fresh-build/manifest.json \
  --profile full --output /absolute/fresh-baseline \
  --requests 300 --repeats 2 --samples 3 \
  --cell-timeout 240 --d0-timeout 1800 --timeout 7200
```

The build uses the requested native rustup Cargo/rustc, release profile,
`RUSTFLAGS="-D warnings"`, release debug info disabled, and two jobs. It records
Cargo's executable paths and optimized profiles, full compiler version, source
fingerprint and binary hashes. It never picks an arbitrary cached executable.
Inherited profile overrides are recorded; a debug/unoptimized executable fails.
The source identity includes HEAD, tracked diff and untracked implementation
files, so new benchmark files cannot disappear from a development build's
provenance. Do not stage, commit or edit source between build and run.

The runner refuses changed source/binaries, an unsuccessful build manifest or
an existing result directory. Every process gets a fresh synthetic home under
the output directory. Timeout kills that driver process group, preserves
remaining fixture homes, marks the experiment invalid and stops further cells.
State and D0 inner timeouts return reserved exit code `124`; the enclosing suite
treats it as a timeout, kills the invocation's process group and stops the batch,
even with `--keep-going`. Ordinary workload failures still return `1`.
It never stops unrelated work. Set bounds to fit the authorized host window;
the default suite deadline is 45 minutes, not a promise that full D0 fits it.
These tools require POSIX process-group support.

All requested cells and independent repeats run once, without request/cell
retries. After an ordinary process failure, independent cells continue to expose
coverage, but no replacement green sample is selected. State warmup's existing
qualification retries are recorded separately; measured failures remain failures.
The exit status is nonzero if any process/artifact fails or the suite is
incomplete. State mutation 503s retain their original artifact and censored
request counts; do not turn those counts into a steady-state error rate.

## Interpreting results

`manifest.json` is an index, not a replacement for raw driver manifests,
per-process stdout/stderr and fixtures. It records commands, source/binary
provenance, per-cell status and metrics, omitted families and the final
measurement status. `measured` means a qualified complete measurement, not a
latency regression verdict. Timing is `reported_only` until unchanged AA noise
and historical before/after sensitivity have justified a family-specific budget.

State percentiles are medians of per-batch percentiles, not pooled request
percentiles. With 300 requests/batch the driver deliberately omits p99. D0
context timing is not enqueue-to-Brief latency; in-process State is not real
socket or client performance. Allocation counts are not retained heap.
OS page cache and external host load are not isolated. One candidate baseline
and its AA repeats cannot establish a historical performance improvement.

For later comparisons, reuse the original D0/State AA/ABBA comparison contracts
on identically instrumented frozen executables. Historical positive controls
#4105, #4095 and #4059 require compatible frozen before/after source, not a slow
mock advertised as the historical implementation. The coverage map does not
claim those controls were run.

## Verify the runner without running a benchmark

```sh
python3 -m unittest discover -s benchmarks/runtime-suite -p 'test_*.py'
python3 -m unittest discover -s benchmarks/state-http -p 'test_*.py'
python3 -m unittest discover -s benchmarks/agent-state -p 'test_*.py'
cargo fmt --all -- --check
```

Keep heavy execution manual or attach this entry point to a separately
authorized periodic/release host. Do not add it to a PR job simply because the
coverage map lists a gap.
