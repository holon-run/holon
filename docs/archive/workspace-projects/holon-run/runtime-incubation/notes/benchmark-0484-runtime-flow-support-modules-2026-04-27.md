# Benchmark: #484 Runtime Flow Support Modules

Date: 2026-04-27

Suite: `anthropic-live-refresh-2026-04-27-0484`

Base: `20ad75711dbc3b2806736a41b4f7141f52551cc0`

Issue: `#484 Split tests/support/runtime_flow.rs into domain-owned support modules`

Runners:

- `runtime-incubation-anthropic`: `anthropic/claude-sonnet-4-6`
- `claude-cli`: `claude-sonnet-4-6`

## Artifacts

- pre-public runtime result:
  `/Users/jolestar/opensource/src/github.com/holon-run/runtime-incubation/.benchmark-results/anthropic-live-refresh-2026-04-27-0484-2026-04-27T03-25-31-021Z/runtime-incubation-0484-runtime-flow-support-modules/runtime-incubation-anthropic/run-01`
- Claude CLI result:
  `/Users/jolestar/opensource/src/github.com/holon-run/runtime-incubation/.benchmark-results/anthropic-live-refresh-2026-04-27-0484-2026-04-27T03-25-31-021Z/runtime-incubation-0484-runtime-flow-support-modules/claude-cli/run-01`

## Outcome

Neither runner produced a mergeable completion for `#484`.

`runtime-incubation-anthropic` attempted a broad split of `tests/support/runtime_flow.rs` into domain-owned modules and opened draft PR `#536`, but verifier failed with Rust compile errors. The branch was also conflicting/dirty on GitHub. The PR was closed as a benchmark artifact.

`claude-cli` attempted the split, then reverted the main refactor and left the repository passing. It opened draft PR `#537`, but its final report explicitly stated that `#484` was not resolved. The PR was closed as a benchmark artifact.

## Metrics

| Runner | Result | Verify | Input | Output | Total | Turns | Tools | Shell | Duration |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|
| `runtime-incubation-anthropic` | failed | failed | 2,261,423 | 20,999 | 2,282,422 | 130 | 135 | 119 | 550,180 ms |
| `claude-cli` | failed | passed | 54,785 | 13,366 | 68,151 | 114 | 245 | 106 | 879,084 ms |

Token ratio:

- pre-public runtime total tokens were about `33.5x` Claude CLI.
- pre-public runtime input tokens were about `41.3x` Claude CLI.
- pre-public runtime output tokens were about `1.57x` Claude CLI.

## Token Observations

pre-public runtime's cost came mainly from repeated large input frames, not from excessive output. `token-optimization.json` showed:

- 130 rounds using `prompt_cache_blocks`.
- `cache_read_input_tokens`: `2,763,776`.
- `high_input_zero_cache_read_rounds`: `54`.
- Several late rounds had `50k+` input tokens without cache read.
- `context_management_enabled_rounds`: `0`.

This suggests prompt cache was active but unstable for this run. Many rounds benefited from cached prefix reads, but the expensive tail was dominated by large input frames with zero cache read.

Important correction: pre-public runtime already has Anthropic context-management support in current code. This benchmark does not show that the mechanism is missing or ineffective. It shows that the mechanism was not active in this run:

- every provider round reported `context_management.enabled=false`
- every provider round reported `disabled_reason=provider_context_management_not_enabled`
- summary metrics reported `context_management_enabled_rounds=0`

The likely direct cause is runner configuration. `buildpre-public runtimeBenchmarkEnv()` only set `PRE-PUBLIC RUNTIME_MODEL` and, for live mode, `PRE-PUBLIC RUNTIME_DISABLE_PROVIDER_FALLBACK=1`. It did not set `PRE-PUBLIC RUNTIME_ANTHROPIC_CONTEXT_MANAGEMENT=true`.

There is also a trigger threshold caveat. The default Anthropic context-management trigger is `100000` input tokens, while this run's largest individual pre-public runtime round was about `51.6k` input tokens. Even if the boolean flag were enabled, the default trigger may still be too high to exercise the mechanism in this benchmark. A future token-focused rerun should explicitly set:

```bash
PRE-PUBLIC RUNTIME_ANTHROPIC_CONTEXT_MANAGEMENT=true
PRE-PUBLIC RUNTIME_ANTHROPIC_CONTEXT_MANAGEMENT_TRIGGER_INPUT_TOKENS=30000
PRE-PUBLIC RUNTIME_ANTHROPIC_CONTEXT_MANAGEMENT_KEEP_RECENT_TOOL_USES=3
```

Claude CLI used more tool calls but far fewer tokens. It was much more aggressive about context management, though it did not complete the task.

The token comparison is therefore not a clean same-completion comparison. Claude CLI achieved lower token usage partly because it eventually reverted the attempted refactor and returned a report saying the issue was unresolved. pre-public runtime spent more tokens because it kept trying to complete the broad split and then entered a long compile-fix loop.

The pre-public runtime-specific waste pattern was:

- one long provider turn with 130 model rounds
- 119 shell commands, many of them small `grep` / `head` / `sed` / `cat` steps
- repeated compile-check and log-inspection loops after a large generated split
- tool history repeatedly present in later rounds, amplified by cache misses

## Code Product Assessment

pre-public runtime produced the more issue-aligned artifact: it actually attempted the domain split and moved facade tests toward domain-owned support files. However, the result was not close to mergeable because the split preserved stale or mismatched API calls in the new modules.

Claude CLI produced the safer artifact: it kept verification green after backing out the attempted refactor. That behavior was operationally conservative, but the code product did not resolve `#484`.

## Follow-up Recommendation

Do not rerun `#484` as one full-file split unless the goal is specifically to stress long refactor behavior.

For an implementation benchmark, split `#484` into smaller issues:

- first split `runtime_subagents`
- then split `runtime_tasks`
- then split `runtime_compaction`
- leave waiting/delivery last because it has the most cross-domain coupling

That smaller sequence should produce more actionable PRs and clearer token comparisons.

For a token-management benchmark, rerun with Anthropic context management explicitly enabled and with a trigger below the observed long-turn range. The report should treat these as first-class fields:

- whether `PRE-PUBLIC RUNTIME_ANTHROPIC_CONTEXT_MANAGEMENT` was enabled
- configured trigger and keep-recent values
- `context_management_enabled_rounds`
- eligible cleared tool-result bytes/count
- high-input zero-cache-read rounds before and after enabling context management

Separately, pre-public runtime still needs runtime/tooling improvements that are independent of Anthropic context management:

- batch read-only shell/tool support to reduce one-small-command-per-round behavior
- stronger summarization of background task status/results before presenting them back to the model
- a hard boundary before long compile-fix loops so the next phase starts from a compact checkpoint rather than the full exploratory/edit history

## PR Disposition

- `#536`: closed as failed `runtime-incubation-anthropic` benchmark artifact.
- `#537`: closed as incomplete `claude-cli` benchmark artifact.
