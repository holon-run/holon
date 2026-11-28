# Wall-clock fixture budgets

## Choice

Test fixtures that bound or measure real wall-clock time — an absolute
`std::time::Instant` deadline handed to a read scope, or a readiness
`recv_timeout` that waits on an OS thread and a storage checkout — reserve an
explicit host-starvation budget of at least 60s instead of assuming a bounded
scheduling delay. Assertions that prove expiry keep driving time explicitly
through `tokio::time::advance` on the paused clock.

When the readiness path can also miss a budget that the product path under test
needs kept small — the reader pool's 100ms checkout budget is what makes a
fully held pool fail fast instead of blocking — the fixture re-attempts the
checkout inside its own wall-clock budget rather than widening the budget the
product path observes.

## Reason

The `crates/holon-http-api` full parallel `--lib` run starves individual test
threads for seconds at a time on a loaded host (96% disk, several tests over
60s). Two fixtures turned a 5s wall-clock assumption into hard failures that
never reproduce in isolation —
`capability_read_and_build_share_the_callers_absolute_deadline` and
`HeldReaders::new` — even though the product contracts they verify are correct.
`RuntimeDbReadScope` deliberately stores a real `std::time::Instant` derived
from `Instant::into_std()`, so a paused Tokio clock cannot govern it, and the
held-reader fixture waits on thread spawn plus a SQLite read checkout rather
  than on Tokio time. Under the same load the held-reader fixture also observes
  the pool's small checkout budget expiring before any reader is held, which is
  a fast failure the readiness wait alone cannot absorb.

## Preserved boundary

- Enlarging a budget must not weaken a business assertion: the deadline test
  still proves that read and build share one absolute deadline and that an
  expired deadline is rejected, and the reader-pool test still proves that a
  fully held pool yields a retryable storage error (503 `storage`).
- Budgets stay test-local; no product timeout, reader-pool behavior, or CI
  configuration changes.
- Re-attempting a fixture checkout must not hide a slow product path: the retry
  only covers holding the pool, and the pool keeps the small checkout budget
  that the requests under test assert on.
- The rejected alternatives stay rejected: no load pre-gate and no
  low-parallelism suite. Both only widen configuration surface without fixing
  the implicit-budget assumption.
- A fixture that deliberately bounds how long a test may hold a resource keeps
  its own independent bound; that bound protects liveness rather than readiness.
