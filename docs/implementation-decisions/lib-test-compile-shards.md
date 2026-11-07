# Compile-time library test groups

The library's inline tests use private APIs and shared fixtures. A test-name
filter runs after compilation, so splitting only the libtest command line does
not reduce the memory needed to compile its complete test binary.

`build.rs` reads `HOLON_LIB_TEST_SHARD` and declares the
`holon_lib_test_shard = "a" | "b" | "c" | "d"` cfg values. With no selector, or
`all`, all four values are enabled and ordinary local commands retain the full
suite. Invalid selectors fail the build. This selector changes no production
behavior and does not set dependency-wide `RUSTFLAGS`.

Test modules select one compile group. `runtime` belongs to `a`, `runtime_db`
to `b`, the explicit `LIB_C_OWNERS` set in `scripts/ci_test_shards.py` to `c`,
and other top-level modules to `d`. Shared fixtures retain their private paths;
only their test functions are gated when a whole module cannot be gated.
Names, assertions, ignored tests, and the two-thread libtest setting are unchanged.

Partial libtests retain helper-only modules and mixed modules exporting fixtures.
This includes exported inherent `impl` methods and the full ancestor paths of
nested helpers, even when retaining an ancestor exposes another test module.
Their unselected callers necessarily leave unused imports and dead code; only
these two lints are allowed in the partial test crate. Production builds and the
default complete test crate retain the original lint policy, including
`-D warnings`. No other lint is relaxed.

The existing `lib-a` CI job compiles and runs `a`, then `c`; `lib-b` runs `b`,
then `d`. Each group is compiled **before** enumerating it. The runner rejects
empty lists, duplicate names, and tests appearing outside their owning group,
including a new ungated test that would otherwise run in all four groups.
It saves the real lists as timing artifacts for union/overlap verification.
`verify-lib-lists --baseline <original-name-list> --directory <list-directory>`
compares all four real enumerations with an original libtest name multiset,
rejecting missing, extra, or duplicate names rather than just checking counts.
Binary unit tests run once, in `lib-a`. Integration test partitioning is unchanged.
The all-target build and filtered models.dev job select `c`, and `rust-build`
additionally checks the explicit `all` selector with `cargo check --locked
--all-targets` under the job's `-D warnings`, so the default complete test
crate stays warning-free; all library test groups are still compiled and
executed by the two library test jobs.

`make build` first compiles the production library, then the selected libtest
without running it, and finally retains the original `cargo build --all-targets`.
The models.dev job likewise precompiles its libtest before the unchanged
filtered all-target test command. This separates the two large Rust targets
without reducing Cargo's job count or dropping targets. The build script watches
existing Git paths resolved by `git rev-parse --git-path`, including worktree
HEAD and common refs, rather than nonexistent `.git/HEAD` paths that invalidate
Cargo's cache on every invocation. These scheduling/cache fixes still require
the original constrained cold-build verification before any memory claim.

When adding a test module, use its owner's existing cfg; a new top-level
subsystem defaults to `d`. Tests added within a gated module inherit its group.
Do not add `ignore`, remove assertions, or increase resource/time budgets to make
a group pass. No peak-memory reduction is claimed until measured with the full
test-name union preserved under the original CI profile and resource limits.
