# Preflight admission queue acceptance (#3745 / #3800)

## Scope and method

This record covers the joint non-production acceptance for #3745 (child task
read amplification, fixed by PR #3751) and #3800 (single-slot preflight
rejection). Both issues share one acceptance base but keep separate
conclusions. The `child-state.v3` fixture (see [README](README.md)) adds
shared-Router/AppState cells, snapshot routes, distinct parents, loaded /
unloaded / mixed children, and bounded arrival-rate saturation.

- New build: this branch (single preflight worker plus a bounded FIFO
  admission queue, `MAX_WAITERS = 16`), release binaries.
- Baseline build: `origin/main` (`d7a1b79b`) with only the identical
  benchmark harness copied in, so both binaries run the same workload code.
- Temporary synthetic SQLite databases and `StubProvider`; no production
  data, no socket, in-process `Router::oneshot` requests.
- Same host as unrelated builds/tests; results are relative comparisons on
  one machine, not absolute capacity numbers.
- Raw artifacts and per-run exit codes: agent session evidence under
  `notes/issue3745-3800/bench/` (not part of the repository).

## #3800 admission queue results

| Cell | Build | Outcomes | Gate delta | ok p50/p95 |
| --- | --- | --- | --- | --- |
| shared c=8, snapshot, warm | baseline | 1 ok, 5×503 `projection_preflight_saturated`, cell aborted | rejected=5 | 2.34 ms (single ok) |
| shared c=8, snapshot, warm | queue | 500/500 ok | leaders=1, cache_hits=492, joined_waiters=7, rejected=0 | 1.16/1.76 ms |
| shared c=4, mixed, 4 parents | queue | 250/250 ok | leaders=3, joined_waiters=4, rejected=0 | 0.29/0.65 ms |
| saturation 8000 req/s, inflight≤64 | queue | 1659 ok, 1341×503 `projection_preflight_saturated` (classified, cell continued) | rejected=1341, max_active_permits=1 | 3.56/4.29 ms (ok), failure p50 0.05 ms |

Interpretation: the baseline reproduces the #3800 loss system under a short
controlled overlap on a shared gate; with the bounded queue the same overlap
fully succeeds. Sustained arrivals above the single-worker service rate are
rejected explicitly and classified rather than queued without bound; the real
worker high-water mark stays at 1 (supervised work stays at B+1).

## #3745 acceptance results

| Cell | Build | Outcomes | ok p50/p95 |
| --- | --- | --- | --- |
| serial `/state`, 1 loaded child | baseline | 100/100 ok | 2.48/2.58 ms |
| serial `/state`, 1 loaded child | queue | 100/100 ok | 2.46/2.57 ms |
| serial `/state`, 10 unloaded children | queue | 100/100 ok | 10.99/11.14 ms |
| serial `/state`, 10 mixed children | queue | 100/100 ok | 10.83/11.37 ms |

Interpretation: the previously fixed owner-scoped task read keeps unrelated
history out of child summaries (stage-1 record); the serial workload is
unchanged by the queue change, and the unloaded/mixed contract checks pass
with the same child identity/active-count expectations. Unloaded storage
reads are heavier per child than loaded handles, as expected.

## Not covered by these cells

- No production latency probe, production 503 rate comparison, or deployment
  claim; the live 2.0% rejection rate in #3800 is field evidence only.
- p99 is null everywhere: no cell reached three batches of 1000 successes.
- Full tiered matrix (history 10k/100k under concurrency, active-set growth
  sweeps, per-parent payload growth, leader/follower cold-cache sweeps) is
  intentionally not run yet; run it against an approved time budget before
  any capacity or SLO conclusion.
- `/state` does not pass through the preflight lane; queue conclusions apply
  to the snapshot routes.
- A-A repeats showed stable medians (35.1 vs 35.2 µs state warm p50) but the
  sample counts are small; treat absolute numbers as order-of-magnitude only.
