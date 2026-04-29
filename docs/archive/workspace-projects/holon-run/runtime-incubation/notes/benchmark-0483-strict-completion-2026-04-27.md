# Benchmark `#483` Strict Completion Rerun

Date: 2026-04-27

Suite: `anthropic-live-refresh-2026-04-27-0483`

Label: `anthropic-live-refresh-2026-04-27-0483-2026-04-27T12-04-53-947Z`

Base: `cd05ec1d61ed1f2e9a46a9499a48770f3294f9c7`

Issue: `#483` Split `tests/support/http_routes.rs` into surface-specific support modules

This rerun followed the failed/incomplete `#538` and `#539` artifacts. Before
starting this run, both old PRs were closed and the benchmark prompt was changed
to explicitly require one complete issue-solving PR. The verifier was also
tightened with `cargo fmt --all -- --check`.

## Artifacts

- `runtime-incubation-anthropic`:
  `/Users/jolestar/opensource/src/github.com/holon-run/runtime-incubation/.benchmark-results/anthropic-live-refresh-2026-04-27-0483-2026-04-27T12-04-53-947Z/runtime-incubation-0483-http-routes-support-modules/runtime-incubation-anthropic/run-01`
- `claude-cli`:
  `/Users/jolestar/opensource/src/github.com/holon-run/runtime-incubation/.benchmark-results/anthropic-live-refresh-2026-04-27-0483-2026-04-27T12-04-53-947Z/runtime-incubation-0483-http-routes-support-modules/claude-cli/run-01`

## PRs

- `#544` `runtime-incubation-anthropic`: https://github.com/holon-run/runtime-incubation/pull/544
- `#545` `claude-cli`: https://github.com/holon-run/runtime-incubation/pull/545

Both PRs were opened as drafts. Both failed CI.

The benchmark artifact `pr.json` again recorded `skipped_no_changes`, even
though GitHub PRs were created. This remains a benchmark framework reporting bug.

## Headline Result

Neither runner completed `#483`.

`#544` from `runtime-incubation-anthropic` is still the more issue-shaped artifact because it
creates surface-specific support modules. However, it remains a re-export shim:
the real implementations still live in `tests/support/http_routes.rs`. This is
not the full split requested by the issue.

`#545` from `claude-cli` regressed relative to the stricter prompt. It explicitly
decided not to split into modules and instead added navigation documentation and
a helper module. That does not satisfy the acceptance criteria. The PR is also
polluted by stale benchmark/mainline commits, likely because the benchmark reused
the same branch name as the previous closed `claude-cli` artifact without fully
resetting or force-updating the remote branch.

## Metrics

| Runner | Success | Verify | Duration | Input Tokens | Output Tokens | Total Tokens | Turns | Tool Calls | Changed Files |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `runtime-incubation-anthropic` | no | no | 845,340 ms | 3,334,543 | 29,868 | 3,364,411 | 109 | 108 | 10 |
| `claude-cli` | no | no | 1,230,846 ms | 311,194 | 76,096 | 387,290 | 143 | 142 | 4 |

pre-public runtime used about `8.7x` the total tokens and about `10.7x` the input tokens of
`claude-cli`.

Compared with the previous `#483` run, pre-public runtime's total token ratio improved
materially (`~30.8x` before, `~8.7x` here), but the absolute input-token count
is still very high at `3.33M`.

## pre-public runtime Prompt-Cache Diagnostics

This run used the base that includes Anthropic prompt-cache request-shape
diagnostics, and Anthropic context management was enabled for `runtime-incubation-anthropic`.

Summary:

- `context_management_enabled_rounds`: `109`
- request lowering mode: `prompt_cache_blocks` for all `109` rounds
- `cache_read_input_tokens`: `2,012,160`
- `high_input_zero_cache_read_rounds`: `62`
- `context_management_eligible_tool_result_bytes`: `8,774,507`
- `context_management_eligible_tool_result_count`: `5,400`

Top cache-miss rounds still appear late in the run and reached about
`67k-72k` input tokens. The new diagnostics confirm that context management and
prompt-cache lowering are active, but the run still has many high-input rounds
without cache reads. The remaining optimization problem is not just enabling the
feature; it is reducing repeated large prompt frames and understanding why cache
breakpoints are not producing cache reads in those late rounds.

## Code Result Comparison

### `#544` `runtime-incubation-anthropic`

Commit: `0ec7aa19d451adba59baf736f4c16215af480f76`

Diff shape:

- 10 files changed
- 99 insertions
- 1 deletion
- adds `tests/support/http_callback.rs`
- adds `tests/support/http_client.rs`
- adds `tests/support/http_control.rs`
- adds `tests/support/http_events.rs`
- adds `tests/support/http_ingress.rs`
- adds `tests/support/http_operator.rs`
- adds `tests/support/http_runtime.rs`
- adds `tests/support/http_tasks.rs`
- adds `tests/support/http_workspace.rs`
- updates `tests/support/mod.rs`

Assessment:

- Better than `claude-cli` in terms of following the requested module shape.
- Still incomplete because the new modules re-export functions from
  `http_routes.rs`; the implementation was not moved.
- Its final message incorrectly claims `http_routes.rs` is now a thin
  implementation shim. In practice it remains the monolithic implementation
  owner.
- Failed verifier and CI at formatting.

### `#545` `claude-cli`

Benchmark summary changed files:

- `tests/support/README.md`
- `tests/support/http_common.rs`
- `tests/support/http_routes.rs`
- `tests/support/mod.rs`

GitHub PR diff also includes unrelated files from the prompt-cache diagnostics
and benchmark prompt commits, including `Cargo.toml`, `Cargo.lock`,
`benchmark/run.mjs`, `benchmark/tests/manifest.test.mjs`,
`src/provider/transports/anthropic.rs`, and others. This makes `#545` unsuitable
as a clean artifact.

Assessment:

- Does not solve the issue. It explicitly rejects the requested split and
  replaces it with documentation/navigation.
- Adds a helper module and README, but does not create the requested
  surface-specific implementation modules.
- PR branch appears contaminated by stale branch history from the previous
  `claude-cli` benchmark branch. This is a benchmark framework hygiene problem:
  rerunning the same `task_id/runner_id` branch should reset or force-update the
  remote branch, or use unique branch names per suite label.
- Failed verifier and CI at formatting.

## Verification And CI

Both benchmark verifier failures were caused by the new `cargo fmt --all -- --check`
command.

For `#544`, formatting failed on import/re-export ordering in the new
surface-specific shim modules and `tests/support/mod.rs`. The subsequent targeted
tests passed:

- `cargo test --test http_control --quiet`
- `cargo test --test http_events --quiet`
- `cargo test --test http_callback --quiet`
- `cargo test --test http_workspace --quiet`
- `cargo test --test http_tasks --quiet`

For `#545`, formatting failed in `tests/support/http_common.rs` near the file
end, same basic issue as the previous `claude-cli` artifact. The subsequent
targeted tests also passed.

GitHub CI failed for both PRs at the same `cargo fmt --all -- --check` stage.
The verifier now catches the same failure as CI, so the verifier tightening
worked.

## Prompt Change Effect

The stricter prompt did not cause either agent to fully solve the issue.

Observed behavior:

- `runtime-incubation-anthropic` still interpreted "complete" as creating named modules that
  re-export the original implementation.
- `claude-cli` decided the requested split was too complex and substituted
  documentation instead.

This suggests prompt pressure alone is insufficient for this issue. The task
likely needs a smaller decomposition or a more explicit acceptance guard such as:

- `tests/support/http_routes.rs` must lose the actual test functions, not just
  be re-exported.
- Each new module must contain the moved test bodies or helper code for its
  surface.
- PRs that only add documentation, comments, or re-export shims do not satisfy
  the benchmark.

## Framework Follow-Ups

1. Fix PR artifact reporting: `pr.json` should record the actual PR number/URL
   when the framework or agent creates a PR.
2. Avoid stale branch contamination on reruns. Either include the suite label in
   branch names or force-reset both local and remote benchmark branches before
   reuse.
3. Keep `cargo fmt --all -- --check` in Rust benchmark manifests when PR
   readiness is part of the evaluation.
4. Add optional artifact-level acceptance checks for this class of task. For
   `#483`, a simple check could assert that `tests/support/http_routes.rs` is
   substantially reduced, or that new module files contain real test bodies
   rather than only `pub use super::http_routes::*`.

## Recommendation

Do not keep either PR as-is.

If we want a salvage path, `#544` is the better starting point, but it should be
treated as a new human/agent follow-up rather than a successful benchmark result:
run `cargo fmt`, move actual test implementations into the new modules, and make
`http_routes.rs` a real shared shim or remove it.

Close `#545`; it is both semantically off-target and branch-contaminated.
