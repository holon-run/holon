# Compile-time library test groups

The runtime library's inline tests use private APIs and shared fixtures. A test-name
filter runs after compilation, so splitting only the libtest command line does
not reduce the memory needed to compile its complete test binary.

`build.rs` reads `HOLON_LIB_TEST_SHARD` and declares the
`holon_lib_test_shard = "a" | "b" | "c" | "d" | "e"` cfg values. With no selector, or
`all`, all five values are enabled and ordinary local commands retain the full
suite. Invalid selectors fail the build. This selector changes no production
behavior and does not set dependency-wide `RUSTFLAGS`.

Most tests in `runtime` belong to `a`. Group `e` owns the `runtime_db`
transaction, control transition, task result settlement, and Task fixture
tests. The remaining database tests belong to `b`. Tests in either module
that instantiate the application HTTP routes run with the HTTP tests in `c`.
The test builds for `a`, `b`, and `e` omit the routes and OpenAPI.
`LIB_C_OWNERS` lists the remaining owners in `c`. The other top-level
modules belong to `d`, which also runs the application's CLI and tail unit
tests. Shared fixtures retain their private paths. Tests are gated
individually when a whole module cannot be gated.
The two-thread libtest setting applies to both crates.

Partial libtests retain helper-only modules and mixed modules exporting fixtures.
This includes exported inherent `impl` methods and the full ancestor paths of
nested helpers, even when retaining an ancestor exposes another test module.
Their unselected callers necessarily leave unused imports and dead code; only
these two lints are allowed in the partial test crate. Production builds and the
default complete test crate retain the original lint policy, including
`-D warnings`. No other lint is relaxed.

The existing `lib-a` CI job runs `a`, `c`, and `e` in sequence. The `lib-b`
job runs `b` and `d` in sequence. Scheduling the smaller transaction build in
`lib-a` uses time available in the shorter job. Each group is compiled before
enumeration, and CI logs the listing command's elapsed time and peak RSS.
The runner rejects empty lists, duplicate names, and tests outside their group,
including a new ungated test that would otherwise run in all five groups.
It saves the real lists as timing artifacts for union/overlap verification.
`verify-lib-lists --baseline <original-name-list> --directory <list-directory>`
compares all five real enumerations with an original libtest name multiset and
rejects missing, extra, or duplicate names.
Binary unit tests run once, in `lib-a`. Integration test partitioning is unchanged.
The two library test jobs compile and run all five groups. In full PR,
scheduled, and manual runs, `rust-build` relies on `lib-a` to compile group
`c`. On main pushes with the all-target build, `rust-build` compiles
`c` itself. The full build also checks the explicit `all` selector with
`cargo check --locked --all-targets` in both crates under `-D warnings`.

`make build` first compiles the production library, then the selected libtest
from `holon-runtime` without running it, and then compiles the application
with `cargo build --all-targets`. The workflow sets
`SKIP_RUNTIME_LIBTEST_BUILD=1` when `lib-a` covers the libtest build.
Staging the commands separates the two large Rust targets
without reducing Cargo's job count or dropping targets. The build script watches
existing Git paths resolved by `git rev-parse --git-path`, including worktree
HEAD and common refs, rather than nonexistent `.git/HEAD` paths that invalidate
Cargo's cache on every invocation. These scheduling/cache fixes still require
the original constrained cold-build verification before any memory claim.

When adding a test module, use its owner's existing cfg; a new top-level
subsystem defaults to `d`. Tests added within a gated module inherit its group.
Do not add `ignore`, remove assertions, or increase resource/time budgets to make
a group pass. Compare peak memory with the full test coverage preserved under
the same compiler profile and resource limits.
