# Benchmark `#483` Prompt Cache V2

Date: 2026-04-27/28

Suite label: `anthropic-live-refresh-2026-04-27-0483-cache-v2-2026-04-27T15-29-30`

Issue: `#483` Split `tests/support/http_routes.rs` into surface-specific support modules

Base: `818ce9c60eb3b7104f3c0d6004e97c978e0e2162` (`origin/main`)

Result directory:

`/Users/jolestar/opensource/worktrees/github.com/holon-run/runtime-incubation/benchmark-runner-0483-cache-20260427/.benchmark-results/anthropic-live-refresh-2026-04-27-0483-cache-v2-2026-04-27T15-29-30`

Agent worktrees:

`/Users/jolestar/opensource/worktrees/github.com/holon-run/runtime-incubation/benchmark-agents-0483-cache-20260427`

## Summary

This run is valid for token/cache behavior, but neither runner produced a mergeable completion.

`runtime-incubation-anthropic` followed the issue guidance more directly than prior `#483` attempts: it performed a real mechanical split of `tests/support/http_routes.rs` into surface-specific files and continued fixing compile errors. It did not stop at a shim-only implementation. However, the final artifact failed the verifier at `cargo fmt --all -- --check`, kept generated `.bak` files in the commit, and GitHub reports the recreated artifact PR as conflicting.

`claude-cli` did not make code changes. It spent 157 turns exploring the split, then stopped with a recommendation to use a shim/simplified approach. The benchmark verifier passed only because the worktree was unchanged and the original targeted tests already passed.

## PRs

- pre-public runtime artifact PR: https://github.com/holon-run/runtime-incubation/pull/551
- Original harness-reused PR record: https://github.com/holon-run/runtime-incubation/pull/544

The benchmark harness initially recorded PR `#544` as `updated`, but that PR was already closed and still pointed at an older head commit. The actual new pre-public runtime artifact commit was `0da4534d29036028595a8109d5805c2fe1585c8f`. I recreated the artifact on a unique branch:

`bench/runtime-incubation-0483-http-routes-support-modules/runtime-incubation-anthropic-cache-v2-20260428`

and opened draft PR `#551`.

This exposes a framework bug: branch/PR reuse should not select a closed PR as the active submission target for a new benchmark run. Future runs should either include a run label in the branch name or explicitly create a new PR when the matching PR is closed.

## Metrics

| Runner | Success | Verify | Duration | Input | Output | Turns | Tool calls | PR |
|---|---:|---:|---:|---:|---:|---:|---:|---|
| `runtime-incubation-anthropic` | no | no | 1,995,145 ms | 6,282,428 | 54,336 | 438 | 397 | `#551` |
| `claude-cli` | no | yes | 935,285 ms | 123,089 | 40,043 | 157 | 155 | none |

pre-public runtime token optimization summary:

- `request_lowering_modes`: `prompt_cache_blocks` for 437 rounds
- `cache_read_input_tokens`: 6,074,880
- `cache_creation_input_tokens`: 0
- `high_input_zero_cache_read_rounds`: 141
- `context_management_enabled_rounds`: 437
- `context_management_eligible_tool_result_bytes`: 17,732,375
- `context_management_eligible_tool_result_count`: 16,994

Top high-input zero-cache-read rounds were still large:

- round 84: 56,484 input tokens after `ApplyPatch`
- round 81: 55,695 input tokens after `ApplyPatch`
- round 75: 53,903 input tokens after `TaskStop`
- round 74: 52,682 input tokens after `TaskOutput`
- round 69: 51,263 input tokens after `ExecCommand`

## Product Assessment

The issue-level implementation guidance helped pre-public runtime choose the right broad path. Unlike prior `#483` runs, it did not merely create re-export shims. It split the monolith into files such as:

- `tests/support/http_control.rs`
- `tests/support/http_events.rs`
- `tests/support/http_callback.rs`
- `tests/support/http_ingress.rs`
- `tests/support/http_workspace.rs`
- `tests/support/http_tasks.rs`
- `tests/support/http_client.rs`
- `tests/support/http_operator_ingress.rs`

The product artifact is still not mergeable:

- `cargo fmt --all -- --check` failed.
- Backup files such as `tests/support/http_client.rs.bak`, `.bak3`, `.bak4`, and similar files were committed.
- The recreated PR `#551` is currently draft and GitHub reports it as conflicting.
- The final message claimed full verification passed, but the benchmark verifier captured a fmt failure. Treat verifier artifacts as authoritative.

Targeted HTTP verifier tests passed after the fmt failure:

- `cargo test --test http_control --quiet`
- `cargo test --test http_events --quiet`
- `cargo test --test http_callback --quiet`
- `cargo test --test http_workspace --quiet`
- `cargo test --test http_tasks --quiet`

## Token/Cache Assessment

The new prompt-cache path is active, but this workload still exposes a high token-cost failure mode.

The positive signal is that pre-public runtime reported 6.07M cache-read tokens and had context management enabled throughout the run. The diagnostics are now visible enough to identify the bad rounds.

The negative signal is that pre-public runtime still accumulated 6.28M billable input tokens over 438 provider rounds. There were 141 high-input rounds with zero cache read, and several were around 50k-56k input tokens. This is not just a missing diagnostics problem; the cache anchor/request-shape still appears to break often enough on long mechanical refactors.

The largest misses correlate with small tool operations after the context is already large, including `ApplyPatch`, `TaskOutput`, and short `ExecCommand` calls. That suggests the next optimization should focus on preserving cache reuse across short iterative repair rounds, not only on reducing large tool outputs.

## Comparison With Claude CLI

Claude used far fewer input tokens, but it also failed to produce code. Its lower token use is not directly comparable as implementation efficiency because it abandoned the task and left the worktree unchanged.

For this run, the meaningful comparison is:

- pre-public runtime spent much more but made substantive progress toward the implementation.
- Claude spent less but did not deliver an artifact.
- pre-public runtime's prompt-cache diagnostics are now detailed enough to guide the next optimization.

## Follow-ups

- Fix benchmark PR creation so closed PRs are not reused as active PR targets.
- Consider unique branch names that include the suite label or timestamp.
- For pre-public runtime prompt cache, investigate why small repair rounds after `ApplyPatch`/`TaskOutput` still produce high-input zero-cache-read requests.
- For this issue type, keep the mechanical-split guidance in the issue body; it improved pre-public runtime's strategy selection.
