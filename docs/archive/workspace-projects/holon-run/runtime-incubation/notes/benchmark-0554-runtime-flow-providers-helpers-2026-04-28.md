# Benchmark `#554` Runtime Flow Providers and Helpers

Date: 2026-04-28

Suite label: `anthropic-live-refresh-2026-04-28-0554-cache-stats-2026-04-28T03-06-51`

Issue: `#554` Split runtime_flow support providers and shared harness helpers

Base: `227a71d1e1fddf84151ad2ed9898d32fa1f09883`

Result directory:

`/Users/jolestar/opensource/worktrees/github.com/holon-run/runtime-incubation/benchmark-runner-0554-cache-stats-20260428/.benchmark-results/anthropic-live-refresh-2026-04-28-0554-cache-stats-2026-04-28T03-06-51`

Agent worktrees:

`/Users/jolestar/opensource/worktrees/github.com/holon-run/runtime-incubation/benchmark-agents-0554-cache-stats-20260428`

## Summary

`runtime-incubation-anthropic` produced the better merge candidate. It completed a conservative first-stage split, passed the benchmark verifier, and CI is green on PR `#555`.

`claude-cli` attempted a broader split and produced a more ambitious code artifact, but it failed formatting verification and CI. The targeted runtime tests passed, so the failure is likely cleanup-level rather than semantic, but the artifact is not merge-ready.

This run is useful for prompt-cache diagnostics because pre-public runtime completed successfully while still showing many high-input zero-cache-read rounds. The new statistics classify those misses: no client-shape changes were detected, and 42 rounds are classified as likely server-side cache breaks.

## PRs

- pre-public runtime: https://github.com/holon-run/runtime-incubation/pull/555
- Claude CLI: https://github.com/holon-run/runtime-incubation/pull/556

Current status at report time:

- `#555`: draft, mergeable, CI success.
- `#556`: draft, mergeable, CI failure.

## Metrics

| Runner | Success | Verify | Duration | Input | Output | Turns | Tool calls | Files changed |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `runtime-incubation-anthropic` | yes | yes | 928,036 ms | 4,032,676 | 15,693 | 154 | 144 | 3 |
| `claude-cli` | no | no | 2,260,274 ms | 327,356 | 22,309 | 136 | 196 | 4 |

pre-public runtime token optimization summary:

- `request_lowering_modes`: `prompt_cache_blocks` for 154 rounds
- `cache_read_input_tokens`: 2,120,704
- `cache_creation_input_tokens`: 0
- `high_input_zero_cache_read_rounds`: 93
- `context_management_enabled_rounds`: 154
- `context_management_eligible_tool_result_bytes`: 13,609,032
- `context_management_eligible_tool_result_count`: 9,410

New cache-break classification:

- `warmup`: 95
- `no_break`: 17
- `likely_server_side`: 42
- `client_shape_changed_cache_break_rounds`: 0
- `ttl_possible_cache_break_rounds`: 0
- `expected_after_compaction_cache_break_rounds`: 0

The top high-input miss rounds were late in the run:

- round 154: 58,039 input tokens after `UpdateWorkItem`
- round 153: 57,752 input tokens after `ExecCommand`
- round 151: 57,079 input tokens after `ExecCommand`
- round 150: 56,950 input tokens after `ExecCommand`
- round 149: 56,822 input tokens after `ExecCommand`

## Product Assessment

pre-public runtime chose a conservative implementation. It extracted shared helpers into `tests/support/runtime_helpers.rs`, updated `tests/support/mod.rs`, and reduced `tests/support/runtime_flow.rs` by about 52 lines. This satisfies the helper-extraction part of the issue and is safe to preserve. It did not move provider implementations yet.

Claude chose a larger implementation. It created both `tests/support/runtime_helpers.rs` and `tests/support/runtime_providers.rs`, moving roughly 1.7k lines out of `runtime_flow.rs`. That better matches the full issue scope, especially the provider split, but the artifact failed `cargo fmt --all -- --check` and CI. It may still be useful as a reference for a follow-up provider extraction PR after formatting and review.

Verification outcomes:

- pre-public runtime passed `cargo fmt --all -- --check`.
- pre-public runtime passed all requested runtime test suites.
- Claude failed `cargo fmt --all -- --check`.
- Claude passed the requested runtime test suites after the fmt failure.

## Cache Assessment

The new diagnostics are more actionable than the previous run. In earlier reports, high-input zero-cache-read rounds were visible but hard to classify. This run adds a useful distinction:

- no client request-shape changes were detected
- no TTL or expected-after-compaction misses were reported
- 42 rounds are classified as likely server-side cache breaks

That means the remaining high token cost is less likely to be caused by pre-public runtime changing the client-side cacheable request shape. The next optimization question is whether the provider-side cache behavior can be improved, or whether pre-public runtime should avoid relying on cache reuse for late short repair loops and instead reduce prompt size more aggressively.

The late top misses happened after small tool operations, mostly short `ExecCommand` calls. This matches the earlier `#483` pattern: once the conversation is large, even small repair steps can become expensive when cache reads do not hit.

## Conclusion

Use `#555` as the keeper PR from this benchmark. It is smaller, verified, and CI-green.

Do not use `#556` as-is. It is more ambitious and closer to the full provider/helper split, but it needs formatting and likely review cleanup before it can be considered mergeable.

For prompt-cache work, this run confirms the diagnostics improvement: the system can now distinguish client-shape cache breaks from likely server-side misses. The remaining target is reducing late-round prompt cost when server-side cache reads do not materialize.
