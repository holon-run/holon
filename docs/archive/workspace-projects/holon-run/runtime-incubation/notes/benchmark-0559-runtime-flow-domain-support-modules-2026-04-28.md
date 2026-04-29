# Benchmark `#559` Runtime Flow Domain Support Modules

Date: 2026-04-28

Suite label: `anthropic-live-refresh-2026-04-28-0559-cache-diag-2026-04-28T06-01-42`

Issue: `#559` Split remaining runtime_flow domain test bodies into support modules

Base: `3daaa44f233ac84f465969f6a9d97cecacc7f996`

Result directory:

`/Users/jolestar/opensource/worktrees/github.com/holon-run/runtime-incubation/benchmark-runner-0559-cache-diag-20260428/.benchmark-results/anthropic-live-refresh-2026-04-28-0559-cache-diag-2026-04-28T06-01-42`

Agent worktrees:

`/Users/jolestar/opensource/worktrees/github.com/holon-run/runtime-incubation/benchmark-agents-0559-cache-diag-20260428`

## Summary

This run is valuable for cache diagnostics, but neither artifact should be treated as a completed implementation without review.

`claude-cli` produced a CI-green PR and the benchmark verifier passed, but its own final message says the acceptance criteria were not met. It created domain support files but did not successfully wire the facade suites away from the original `runtime_flow.rs` structure.

`runtime-incubation-anthropic` attempted the more direct implementation: it modified facade suites and introduced domain support modules that are actually intended to replace `runtime_flow` usage. However, it did not converge. The verifier and CI failed on syntax/import errors, and the final message was a provider request failure rather than a completion summary.

## PRs

- Claude CLI: https://github.com/holon-run/runtime-incubation/pull/560
- pre-public runtime Anthropic: https://github.com/holon-run/runtime-incubation/pull/561

Current status at report time:

- `#560`: draft, mergeable, CI success.
- `#561`: draft, mergeable, CI failure.

## Metrics

| Runner | Benchmark success | Verifier | Duration | Input | Output | Turns | Tool calls | Files changed |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `claude-cli` | yes | yes | 790,322 ms | 88,743 | 36,306 | 112 | 111 | 6 |
| `runtime-incubation-anthropic` | no | no | 5,782,042 ms | 7,449,516 | 40,548 | 339 | 407 | 15 |

## Product Assessment

Claude's artifact is operationally clean but incomplete for the issue intent.

It adds:

- `tests/support/runtime_compaction.rs`
- `tests/support/runtime_flow_compat.rs`
- `tests/support/runtime_subagents.rs`
- `tests/support/runtime_tasks.rs`
- `tests/support/runtime_waiting.rs`
- `tests/support/runtime_workspace_worktree.rs`

The verifier and CI are green. However, the final report explicitly says it had to revert facade tests to keep using `runtime_flow.rs`, and that the acceptance criteria were not met. This means the benchmark's automated verifier was too permissive for the issue. `#560` is useful as a safe exploratory artifact, not a complete solution.

pre-public runtime's artifact is closer to the intended shape but broken.

It changes facade suites and support modules:

- updates runtime facade tests such as `tests/runtime_compaction.rs`, `tests/runtime_tasks.rs`, `tests/runtime_subagents.rs`, `tests/runtime_waiting_and_reactivation.rs`, and `tests/runtime_workspace_worktree.rs`
- adds domain support modules under `tests/support/`
- edits `tests/support/runtime_flow.rs` and `tests/support/runtime_providers.rs`

The failure is not subtle. `cargo fmt` and tests fail because generated files contain invalid imports such as:

```rust
use crate::support{
```

instead of a valid path like `use crate::support::{...};`. There is also at least one duplicate import in `runtime_workspace_worktree.rs`. The implementation likely needed one or two more repair iterations, but the run ended after an Anthropic request failure.

## Cache Diagnostics

This run used the newer precise cache diagnostics and produced the most useful breakdown so far.

pre-public runtime token optimization summary:

- `request_lowering_modes`: `prompt_cache_blocks` for 339 rounds
- `cache_read_input_tokens`: 5,074,304
- `cache_creation_input_tokens`: 0
- `high_input_zero_cache_read_rounds`: 186
- `context_management_enabled_rounds`: 339
- `context_management_eligible_tool_result_bytes`: 23,036,493
- `context_management_eligible_tool_result_count`: 34,238
- `context_management_applied_rounds`: 0
- `context_management_cleared_input_tokens`: 0
- `context_management_cleared_tool_uses`: 0

Cache-break classification:

- `true_warmup`: 15
- `normal_cache_read`: 114
- `likely_server_side_drop`: 114
- `continued_cache_miss`: 94
- `ttl_possible`: 1
- `expected_after_compaction`: 1
- `client_shape_changed_cache_break_rounds`: 0
- `moving_breakpoint_non_reuse_rounds`: 0

Top miss rounds were clustered around rounds 156-168, all after small `ExecCommand` calls, with about 59k-62k input tokens. This reinforces the pattern seen in previous runs: the most expensive prompt-cache failures happen during late short repair loops, not during the initial large mechanical split.

The important new signal is that no client-shape changes were detected. The diagnostics now separate likely provider/server-side cache drops and continued miss streaks from pre-public runtime-generated request-shape instability. That makes the next optimization target clearer: late-loop prompt size and recovery strategy matter even when the client request shape is stable.

## Conclusion

For code preservation, neither PR is an obvious merge candidate as-is:

- `#560` is CI-green but incomplete for the issue's real acceptance criteria.
- `#561` is closer to the intended implementation but fails to compile.

If choosing one branch to salvage, start from `#561` only if the goal is to preserve the facade wiring and domain-module direction. Start from `#560` only if the goal is to keep a green, conservative exploratory split and then manually finish the facade rewiring.

For the cache-diagnostics goal, this run succeeded. The new classification shows that pre-public runtime's high input cost is not driven by client-shape churn in this run; it is dominated by likely server-side drops and continued miss streaks during late repair loops.
