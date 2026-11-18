# Runtime DB reader-pool fairness (#4169 P2)

These opt-in tests exercise the ordinary and preparation SQLite reader pools.
They use fresh temporary homes, synthetic records, an in-process HTTP router and
a stub model provider. They do not launch a daemon, bind a port, or use a live
provider or a deployed database. No pool defaults or scheduling policy change.

| Test filter | Fixed workload | Progress oracle |
| --- | --- | --- |
| `measure_dual_reader_pools_saturation` | One cold 1/1 fixture, 12 epochs, one held lease and four queued SQL reads per role, 100 ms injected hold after both backlogs are observed | Alternate release order. Every read in the first role must finish while the other role is still held. Then the other role must drain. Check return and reuse of both connections before the next epoch. |
| `measure_dual_reader_pools_http_and_tick` | Eight fresh default-capacity fixtures; one real `system_tick` in each, with all preparation leases held, then eight concurrent ordinary HTTP work-item reads | Observe a preparation checkout and prove all eight HTTP responses contain the fixture's work item before releasing preparation. Then require the tick to finish, the leases to return and exactly one tick message to exist. |

Phase waits are bounded; the injected hold is workload, not the signal for
detecting a waiter. This measures role-level progress, not FIFO guarantees
within a pool, maximum capacity, network overhead, or production latency.
Routine, non-ignored regressions exercise smaller interleavings, the operator
input settlement path, and reader deadline, cancellation, transaction and
sidecar contracts. Their checks are not performance samples.

## Controlled run

Build before the measurement window in a clean checkout of the pinned candidate,
with an explicit stable toolchain and an exclusive `CARGO_TARGET_DIR`. Record the
full commit, `rustc +stable --version`, flags and SHA-256 of the resulting binary.
This recipe uses the debug test profile with debug info disabled; it does not
represent a release build.

```bash
set -euo pipefail
export CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export RUSTFLAGS='-D warnings'
cargo +stable test --locked --lib --no-run --message-format=json > "$BUILD_OUT/build.jsonl"
test_bin=$(jq -er 'select(.reason == "compiler-artifact" and
  .target.name == "holon" and .profile.test and .executable != null) |
  .executable' "$BUILD_OUT/build.jsonl")
sha256sum "$test_bin" > "$BUILD_OUT/binary.sha256"
```

After explicit approval, run serially on an isolated runner with networking
disabled and the repository mounted read-only for the measurement. Use a private
scratch filesystem (256 MiB total for fixture and evidence), cap the group to two
CPU cores, 1.5 GiB memory and 64 tasks, and avoid concurrent builds or other
benchmarks. The runtime reader limit is 1+1 in saturation and 16+4 in the
HTTP/tick case; these limits exclude the writer and other fixture databases.
If the resource limits or isolation cannot be verified, do not start. Enforce
a 150-second group deadline with runner cleanup. Use a fresh `OUT` on the
scratch mount and the same pinned binary for both cases:

```bash
mkdir -m 700 "$OUT/home" "$OUT/tmp"
for case in \
  runtime_db::reader_pool_tests::measure_dual_reader_pools_saturation \
  runtime::state_mutation::work_planning::tests::measure_dual_reader_pools_http_and_tick; do
  name=${case##*::}
  status=0
  env -i PATH=/usr/bin:/bin HOME="$OUT/home" TMPDIR="$OUT/tmp" \
    /usr/bin/time -v -o "$OUT/$name.resources" \
    timeout --kill-after=5s 60s "$test_bin" "$case" \
      --ignored --exact --test-threads=1 --nocapture > "$OUT/$name.log" 2>&1 ||
    status=$?
  printf '%s\t%s\n' "$name" "$status" >> "$OUT/exit-status.tsv"
  if (( status != 0 )); then exit "$status"; fi
done
```

Stop on the first nonzero exit, resource limit, loss of isolation, or concurrent
workload. Do not retry or silently drop a failed epoch. Keep the raw logs,
resource records, exit status, binary identity, any missing case, and evidence
of runner limits.

Each `reader_pool_sample` line is JSON with a phase, a monotonic elapsed time
and the two role snapshots. Use within-process counter and histogram bucket
deltas; gauges describe a point in time and may change between snapshots. Report
per-role checkout wait, lease duration and connection setup separately, as well
as successful reads/responses and resource cost. Lease duration includes the
injected hold; it is not SQL execution time. Report observed progress in both
release orders, any failed or missing evidence, and tail limits from the sample
counts. Do not infer a default-capacity change from these runs.
