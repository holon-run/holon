# External-dependency test routing

Regular PR checks retain pure logic, synthetic fixtures and stable mocks, but do
not execute fixtures requiring real third-party commands or host process/service
interactions. Main push remains build-only. The existing nightly and manual CI
entries additionally run `rust-external` on Linux and macOS; a PR label or changed
path cannot opt that job into PR checks.

The reviewed [manifest](../../scripts/ci-external-tests.json) identifies each
external test by Cargo manifest, target, exact libtest name, source, dependencies,
platform and reason. This includes indirect fixture setup: a negative assertion
is still external if its fixture creates a real Git worktree. Tailscale CLI stubs
driving a real daemon/loopback are host integration fixtures, not an installation
requirement for Tailscale. A mock HTTP server or temporary file alone does not
make a test external. Platform fd/locking and zero-timeout assertion defects are
not reclassified to hide failures.

`make test`, `make test-shard` and the concurrent entries apply target-scoped
libtest `--skip` arguments before regular enumeration and execution. Mixed
binaries keep all their other tests. Regular entries exclude external names
before every enumeration as well as execution. The manual/nightly external entry
also captures the unfiltered names, validates the manifest against the actual
compiled target, and compares the filtered enumeration. Missing or duplicate
names and substring collisions fail closed; regular and external names
must be a disjoint, complete union. A newly added unlisted test is explicitly
assigned to regular, never silently dropped. Review fixture changes and add
external dependencies to the manifest rather than relying on name heuristics.

`make test-external` executes the exact reviewed names with one test thread
and rejects an empty, zero-test or all-ignored result. Existing ignored live
provider/SDK tests and feature-gated recovery/ONNX tests retain their previous
opt-in gates; `existing_opt_in_tests` records that audit boundary, not additional
tests enabled by this entry. No test is newly ignored. Root library tests retain the existing four
compile groups, built sequentially. Regular independent-crate commands use the
same manifest through `ci_external_tests.py crate`. Ordinary local `cargo test`
is unchanged and still runs the original suite.

Local equivalents:

```sh
make app-sdk # Required by the embedded holon.js App fixtures, as in CI.
CARGO_BUILD_JOBS=1 make test-external
make test-external EXTERNAL_ARGS="--manifest-path Cargo.toml --target cli_json_contract"
make test-external EXTERNAL_ARGS="--manifest-path Cargo.toml --target lib --lib-group all"
make test-external EXTERNAL_ARGS="--report target/ci-timing/external-tests.json"
python3 -m unittest tests.test_ci_test_shards tests.test_ci_event_matrix
```

`--name` (repeatable) selects exact names for a focused reproduction. The default
external entry runs every supported manifest entry, not a smoke subset. Preflight
prints OS, command paths/versions, capability tags and canonical fixture `TMPDIR`;
missing required commands fail with an installation diagnostic instead of skipping
tests. Missing App SDK artifacts fail with the existing `make app-sdk` preparation
command; the external job builds them before Cargo tests, just like the regular
misc shard. Git fixtures receive physical temporary paths, including on macOS. The
report records real names, counts, sources, platform and exit status per target.
Unsupported platforms are not counted as passes; Linux results cannot establish
macOS path correctness. Keep actual run evidence separate from event-selection
unit tests and from unexecuted GitHub manual/nightly jobs.
