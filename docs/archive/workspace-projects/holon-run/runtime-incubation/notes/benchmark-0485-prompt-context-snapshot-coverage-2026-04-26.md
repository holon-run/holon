# Benchmark `#485` Prompt Context Snapshot Coverage

Date: 2026-04-26
Base: `3159ade94f52d9ebbeef1d65fb23f48c92b3efe1`

## Runs

This issue ended up with two relevant benchmark runs:

1. Mixed Anthropic head-to-head:
   - label: `anthropic-live-refresh-2026-04-26-0485-2026-04-26T02-20-14-735Z`
   - runners:
     - `claude-cli`
     - `runtime-incubation-anthropic`
2. pre-public runtime-only rerun:
   - label: `anthropic-live-refresh-2026-04-26-0485-runtime-incubation-only-2026-04-26T02-57-27-885Z`
   - runner:
     - `runtime-incubation-anthropic`

The mixed run is the artifact-comparison run. The pre-public runtime-only rerun is mainly useful for separating Anthropic endpoint stability from issue-level implementation quality.

## Result Summary

Issue `#485` asks for broader `prompt/context` snapshot coverage across work, wait, delivery, and checkpoint-aware surfaces.

### `claude-cli`

- Benchmark summary status: `success: false`
- Verifier: passed
- Recorded branch: `bench/runtime-incubation-0485-prompt-context-snapshot-coverage/claude-cli`
- Final message reported draft PR `#503`
- Changed file:
  - `tests/prompt_context_snapshots.rs`
- Tokens:
  - input: `99,443`
  - output: `19,828`
- Tool calls: `58`
- Shell commands: `31`
- Duration: `696,340 ms`

Shape:

- Expands snapshot coverage from the initial representative surfaces to a broader matrix.
- Adds coverage for:
  - active work + queued work interaction
  - working-memory delta absence/presence variations
  - callback and richer work-state context
  - system tick / wake surfaces
  - post-compaction / checkpoint-aware continuity
  - multiple work-item prompt-state scenarios

Important nuance:

- Framework closeout marked this run as `skipped_no_changes` and did not retain a final PR record in `summary.json`.
- That closeout state should not be treated as “no useful artifact”.
- The code artifact itself is issue-aligned and materially expands coverage.

### `runtime-incubation-anthropic` mixed run

- Benchmark summary status: `success: false`
- Verifier: passed
- Changed files: none
- PR: none
- Tokens:
  - input: `46,588`
  - output: `304`
- Tool calls: `7`
- Duration: `240,553 ms`

Failure shape:

- Failed early while processing the operator prompt.
- `runner.log` shows:
  - first a retryable timeout
  - then a fail-fast transport error against `https://open.bigmodel.cn/api/anthropic/v1/messages`

Interpretation:

- This run is not evidence that pre-public runtime could not understand `#485`.
- It is evidence that the Anthropic-compatible endpoint was unstable for this benchmark-sized request path.

### `runtime-incubation-anthropic` rerun

- Benchmark summary status: `success: true`
- Verifier: passed
- Commit: `c9036e9bcea6fabb5da4b1a57433a7faaefe620d`
- PR: `#504`
- Changed file:
  - `tests/prompt_context_snapshots.rs`
- Tokens:
  - input: `1,398,249`
  - output: `36,396`
- Tool calls: `163`
- Shell commands: `153`
- Duration: `777,397 ms`

Shape:

- Converged to a small test-hygiene patch, not a broad snapshot expansion.
- Final change:
  - remove duplicate `#[test]`
  - add missing `#[test]` to an existing snapshot test function

This is a valid small repair, but it does not satisfy the main scope of `#485`.

## Main Conclusion

Preferred artifact: `claude-cli`

Reason:

- `claude-cli` is the runner that actually pursued the issue contract.
- Its artifact expands snapshot coverage in the direction explicitly requested by `#485`.
- `runtime-incubation-anthropic` eventually succeeded only after a rerun, but converged to a narrow annotation fix rather than the intended coverage expansion.

So the correct interpretation is:

- `claude-cli`: better issue-aligned implementation artifact
- `runtime-incubation-anthropic`: successful but off-center small repair

This is another case where benchmark `success` alone is not the right primary judgment field.

- `claude-cli` has the stronger code result despite framework closeout noise.
- `runtime-incubation-anthropic` has the cleaner benchmark `success` field, but the weaker issue-level outcome.

## Anthropic Endpoint Note

The Anthropic-compatible provider configuration itself is valid.

Independent live tests passed:

- `live_provider_returns_real_response`
- `live_runtime_wakes_sleeps_and_preserves_context`

That means:

- `ANTHROPIC_BASE_URL` / `ANTHROPIC_AUTH_TOKEN` were loaded correctly
- pre-public runtime's Anthropic transport path is fundamentally working

The mixed-run failure is better explained as benchmark-path transport instability for a larger issue-driven request, not as missing config.

Retry behavior also matters here:

- provider retries up to `3` attempts per provider
- timeout / connection / rate-limited / server-error failures are retryable
- `unknown` transport failures are fail-fast

The mixed pre-public runtime run showed exactly that pattern:

- first timeout retry
- then `error sending request for url ...`
- final classification as `fail_fast (unknown)`

## Token Comparison

The cleanest direct comparison is between:

- `claude-cli` mixed run artifact
- `runtime-incubation-anthropic` pre-public runtime-only rerun

| Runner | Artifact judgment | Input tokens | Output tokens | Tool calls | Duration |
| --- | --- | ---: | ---: | ---: | ---: |
| `claude-cli` | stronger issue-aligned artifact | `99,443` | `19,828` | `58` | `696,340 ms` |
| `runtime-incubation-anthropic` | smaller hygiene repair | `1,398,249` | `36,396` | `163` | `777,397 ms` |

Headline:

- pre-public runtime used about `14x` the input tokens of `claude-cli`
- pre-public runtime used about `1.8x` the output tokens
- pre-public runtime used about `2.8x` the tool calls
- Yet it still converged to the smaller issue outcome

This makes `#485` a strong example of:

- comparable model family
- similar repository/task context
- significantly different task framing and convergence behavior

## Artifact Paths

### Mixed run

- `claude-cli`
  - `/Users/jolestar/opensource/src/github.com/holon-run/runtime-incubation/.benchmark-results/anthropic-live-refresh-2026-04-26-0485-2026-04-26T02-20-14-735Z/runtime-incubation-0485-prompt-context-snapshot-coverage/claude-cli/run-01`
- `runtime-incubation-anthropic`
  - `/Users/jolestar/opensource/src/github.com/holon-run/runtime-incubation/.benchmark-results/anthropic-live-refresh-2026-04-26-0485-2026-04-26T02-20-14-735Z/runtime-incubation-0485-prompt-context-snapshot-coverage/runtime-incubation-anthropic/run-01`

### pre-public runtime-only rerun

- `runtime-incubation-anthropic`
  - `/Users/jolestar/opensource/src/github.com/holon-run/runtime-incubation/.benchmark-results/anthropic-live-refresh-2026-04-26-0485-runtime-incubation-only-2026-04-26T02-57-27-885Z/runtime-incubation-0485-prompt-context-snapshot-coverage/runtime-incubation-anthropic/run-01`
