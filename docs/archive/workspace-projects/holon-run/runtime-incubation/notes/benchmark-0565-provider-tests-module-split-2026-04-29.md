# Benchmark `#565` Provider Tests Module Split

Date: 2026-04-29

Issue: `#565` Split provider contract tests into focused modules

This report compares the completed `claude-cli` artifact with the latest repaired `runtime-incubation-anthropic` rerun. The pre-public runtime rerun was executed after provider/runtime fixes were merged into `main`, so the base SHAs are not identical. This makes the run useful for evaluating the current pre-public runtime runtime and token behavior, but not a perfectly controlled one-to-one code-generation comparison.

## Result Directories

Claude CLI:

`/Users/jolestar/opensource/worktrees/github.com/holon-run/runtime-incubation/benchmark-runner-0565-glm-cache-strategy-20260429/.benchmark-results/anthropic-live-refresh-2026-04-29-0565-glm-cache-strategy-2026-04-28T16-43-37-440Z/runtime-incubation-0565-provider-tests-module-split/claude-cli/run-01`

pre-public runtime Anthropic rerun3:

`/Users/jolestar/opensource/worktrees/github.com/holon-run/runtime-incubation/benchmark-runner-0565-glm-cache-strategy-20260429/.benchmark-results/anthropic-live-refresh-2026-04-29-0565-runtime-incubation-rerun3-2026-04-28T19-04-41-166Z/runtime-incubation-0565-provider-tests-module-split-runtime-incubation-rerun3/runtime-incubation-anthropic/run-01`

## Run Configuration

Claude CLI used `claude-opus-4-6` with Anthropic model aliases:

- `ANTHROPIC_DEFAULT_OPUS_MODEL=GLM-5.1`
- `ANTHROPIC_DEFAULT_SONNET_MODEL=GLM-5.1`

pre-public runtime rerun3 used:

- `model_ref=anthropic/claude-opus-4-6`
- `ANTHROPIC_DEFAULT_OPUS_MODEL=GLM-5.1`
- `ANTHROPIC_DEFAULT_SONNET_MODEL=GLM-5.1`
- `PRE-PUBLIC RUNTIME_ANTHROPIC_CONTEXT_MANAGEMENT=true`
- `PRE-PUBLIC RUNTIME_ANTHROPIC_CONTEXT_MANAGEMENT_TRIGGER_INPUT_TOKENS=30000`
- `PRE-PUBLIC RUNTIME_ANTHROPIC_CONTEXT_MANAGEMENT_KEEP_RECENT_TOOL_USES=3`
- `PRE-PUBLIC RUNTIME_ANTHROPIC_CACHE_STRATEGY=claude_cli_like`

Base SHAs:

- Claude CLI: `48dbca946ddcd834f94e9ade9e48fcc32c0214a2`
- pre-public runtime rerun3: `e8b8d72d6c362632590c07f2590c984bcc42341a`

## PRs

- Claude CLI: https://github.com/holon-run/runtime-incubation/pull/567
- pre-public runtime Anthropic rerun3: https://github.com/holon-run/runtime-incubation/pull/574

Status at report time:

- `#567`: draft PR, CI success, but merge state is dirty because `main` moved.
- `#574`: draft PR, CI failure.

## Metrics

| Runner | Verifier | CI | Duration | Input | Output | Total tokens | Turns | Tool calls | Shell commands | Files changed |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `claude-cli` | passed | success | 1,646,481 ms | 247,659 | 51,625 | 299,284 | 110 | 107 | 40 | 8 |
| `runtime-incubation-anthropic` rerun3 | failed | failure | 781,470 ms | 56,105 | 31,030 | 87,135 | 214 | 213 | 188 | 9 |

The benchmark `token-optimization.json` for pre-public runtime rerun3 did not populate per-round cache diagnostics, so only aggregate token fields are directly comparable from the saved artifact. Even with that limitation, the aggregate token count is materially lower for pre-public runtime: about 29% of Claude CLI's total tokens.

## Product Assessment

Claude CLI produced the better implementation artifact.

It deleted the original monolithic `src/provider/tests.rs`, created focused modules under `src/provider/tests/`, and passed both verifier commands:

- `cargo fmt --all -- --check`
- `cargo test provider --quiet`

The changed-file shape is clean:

- deleted `src/provider/tests.rs`
- added `src/provider/tests/mod.rs`
- added `src/provider/tests/support.rs`
- added provider-focused modules for Anthropic, OpenAI Responses, OpenAI Chat Completions, routing/auth/doctor, and tool schema tests

pre-public runtime rerun3 reached the end of the task and opened `#574`, which confirms the Anthropic provider/runtime continuation fixes were sufficient for a long live run. However, the code artifact is not usable as-is.

The verifier failed with module-path errors after the split:

- `super::ToolResultBlock` not found in the new module hierarchy
- `super::AgentProvider` not found
- `super::classify_provider_error` not found

The artifact also left the old test file as `src/provider/tests_old/tests.rs` instead of cleanly deleting it. The diff has only one removed line and over 4,600 added lines, which indicates the old content was copied rather than cleanly moved. That is a weaker refactor shape for this issue than Claude CLI's delete-and-split result.

## Behavior Difference

Claude CLI used fewer turns and shell commands, but many more aggregate input tokens:

- 110 turns
- 40 shell commands
- 247,659 input tokens

pre-public runtime used many more execution iterations but lower aggregate tokens:

- 214 turns
- 188 shell commands
- 56,105 input tokens

This suggests the current pre-public runtime Anthropic path is more token-efficient under the GLM-backed Anthropic compatibility setup, but it spent that budget on many small repair/exploration loops and still failed to converge. The failure mode is not provider transport instability anymore; it is implementation quality and verification-loop effectiveness.

The final-message quality also diverged. Claude's final message matched the verifier result and reported the passing tests. pre-public runtime's final message claimed the task was complete and listed only formatting verification, while the framework verifier and GitHub CI both showed `cargo test provider --quiet` failing. That is a concrete reliability issue for benchmark interpretation: pre-public runtime's terminal summary should not be trusted without verifier artifacts.

## Conclusion

Keep `#567` as the better reference artifact if this issue is worth preserving. It is CI-green and has the correct refactor shape, though it needs rebase/conflict handling before it can be considered merge-ready.

Do not keep `#574` as-is. It is useful evidence that the pre-public runtime Anthropic runtime now survives the long task, and it shows improved aggregate token usage, but the implementation failed basic compile verification and produced an over-copied split.

For the prompt-cache/token goal, this run is encouraging but incomplete: pre-public runtime used far fewer aggregate tokens than Claude CLI on the saved metrics, but the per-round cache diagnostic file was empty for this rerun, so the report cannot attribute the reduction precisely to prompt-cache hits versus request-shaping/context-management behavior.
