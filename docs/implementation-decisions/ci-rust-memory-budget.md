# CI Rust Compile Memory Budget

Decision:

- Linux Rust jobs under evaluation set
  `CARGO_INCREMENTAL=0`, `CARGO_BUILD_JOBS=2`, and
  `CARGO_PROFILE_{DEV,TEST}_DEBUG=line-tables-only` with
  `CARGO_PROFILE_{DEV,TEST,RELEASE}_CODEGEN_UNITS=1`.
- The `lib-b` matrix job overrides test codegen units to 16 after the
  source-level test split described below; other jobs retain one unit.
- Test jobs additionally keep `CARGO_PROFILE_{DEV,TEST}_DEBUG_ASSERTIONS=true`,
  so the reduced debuginfo does not weaken test semantics.
- macOS and release jobs are outside this Linux regression experiment.
- The budget is applied through job `env`, not `.cargo/config.toml`, so local
  developer builds keep full debuginfo.

Reason:

- The public-repository Linux standard runner has 4 vCPUs and 16 GB RAM;
  this is not a specification for macOS runners.
  Measured on Rust 1.99.1 with a fresh target directory,
  the `holon` rustc peak for `cargo test --lib` grew from 11.75 GiB on the
  pre-rebase base to 20.28 GiB on this branch, which exceeds that budget and
  is consistent with the CI jobs dying mid-compilation. Runner shutdown
  (exit 143) does not itself prove a remote OOM.
- Lower debuginfo alone is insufficient: `debug=0` still used about 17 GiB.
  `CARGO_BUILD_JOBS=2` limits Cargo parallelism, not individual rustc RSS.
  The combined profile settings must be measured before claiming a fix.
- The production-only `cargo build --lib` peak is about 9.9 GiB against 6.6
  GiB on the base. This establishes a production-code increase but does not
  isolate the contribution of the roughly 2900 new inline unit tests.
  The resumed validation of all four jobs failed under a 14 GiB cgroup
  limit with swap disabled: build, both library shards, and the models.dev
  test step reached the limit and recorded OOM kills. None timed out.
  These candidate settings are therefore insufficient; a source-level
  compilation-structure fix is still required before this is a validated
  CI configuration. This local OOM evidence does not establish why the
  remote runners exited with status 143.

Boundary:

- These settings trade CI compile time and debuginfo fidelity for potentially
  lower memory use. They do not impose a memory ceiling; rustc exposes no hard memory limit,
  and cgroup limits would only turn an OOM into a failed job. If the crate
  grows past the budget again, the next step is splitting compilation units,
  not weakening the budget.

Source-level candidates:

- The first candidate made `commit_transition_inner` accept a lifetime-bound
  boxed `dyn FnOnce` callback rather than a concrete closure type. Its small
  generic adapters avoided monomorphizing the shared transaction future for
  each repository command. SQL, witness completion, publication, and
  cancellation ordering were unchanged in that candidate. Its isolated test
  enumeration still recorded an OOM kill at 14 GiB after 441.7 seconds;
  it did not complete compilation. The patch was archived and reverted.
- The second isolated candidate changed `boxed_provider_round` from `Pin<Box<F>>`
  to a lifetime-bound `Pin<Box<dyn Future<Output = F::Output> + Send>>`.
  Boxing alone kept the concrete provider future type visible to enclosing
  futures; type erasure cut that propagation without changing construction,
  polling, cancellation points, or resource drop order. Its isolated test
  enumeration also recorded an OOM kill at 14 GiB after 431.5 seconds,
  without completing compilation. The patch was archived and reverted.
- Neither isolated candidate met the budget. Further source-level changes
  require compilation-structure evidence rather than assuming these two
  type-erasure boundaries explain the regression. Library test enumeration
  must preserve the full test-name set, and all four original jobs still
  have to pass within their existing 60-minute limits before delivery.
- A subsequent compiler monomorphization dump identified the DB transaction
  core (2740 instances) and writer retry core (2726 instances) as the two
  largest definitions by estimated total MIR size, together accounting for
  18.66% of that estimate. These estimates are not per-function RSS.
  The third isolated candidate kept their generic entrypoints and `Result<T>`,
  but moved the large bodies behind borrowed `dyn FnMut` callbacks so only the
  result type, not every caller closure type, specialized those bodies.
  A stack-local `Option::take` adapts the one-shot callback without allocating;
  callback consumption, result ownership, transaction cleanup, and retry
  ordering remained in their original places. Its isolated test enumeration
  also recorded an OOM kill at 14 GiB after 425.2 seconds without completing
  compilation. The patch was archived and reverted; the monomorphization
  ranking alone did not establish that erasing those callbacks was sufficient.
  A diagnostic reproduction of that same patch reduced estimated total MIR
  by 22% and the two core instance counts to 278/277, but reduced RSS at the
  end of monomorphization graph traversal by only 0.9%. It still recorded an
  OOM kill after 435.6 seconds. That patch has also been reverted; its failure
  does not rule out every type-erasure boundary.
- A separate diagnostic disabled only `test`/`tokio::test` functions with
  temporary `cfg` attributes, retaining test-only production hooks and helper
  modules. With the same profile, 4 CPUs, 14 GiB cgroup limit, no swap, and a
  fresh target, compilation finished in 391.14 seconds with a cgroup peak of
  11.57 GiB and no OOM. This run used local compiler phase instrumentation and
  temporarily removed `-D warnings` because unused test helpers remain.
  It enumerated zero tests and is not acceptance evidence. All temporary
  source changes have been restored.
- These measurements show that removing test bodies can fit this diagnostic
  build within the budget, not which particular test or production boundary
  is the root cause. Current library CI shards divide execution only; each
  still compiles the full library test binary. Any compilation-structure fix
  must preserve all 8162 original test names and assertions, and pass all
  four original jobs without diagnostic switches or relaxed budgets.

Subsequent CI configuration:

- Library tests now compile in four disjoint groups (`a` through `d`), with
  two groups per library job. The macOS logging gate compiles group `c`,
  which contains every selected logging test. The split preserves the
  assertions and checks test-name coverage rather than skipping tests.
- After this split, `lib-b` still received runner shutdown during compilation
  with one codegen unit. With 16 test codegen units, CI completed both group
  builds and reached every test: group `b` passed 2078 tests with two existing
  ignores; group `d` exposed four obsolete quota assertions. The timed whole
  shard command reported a maximum child RSS of 14151480 KiB. This is not a
  measurement of aggregate cgroup memory and does not establish the cause of
  earlier runner shutdowns. CI records `free -h` and `/usr/bin/time -v` output
  to distinguish subsequent compile, runner, and test failures.
- Test debug assertions, warning checks, and the existing job timeouts remain
  enabled. Successful compilation alone does not certify the full test gate.
