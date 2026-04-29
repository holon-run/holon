# Anthropic Prompt Cache Live Cases

This note records the local live probe added in `tests/live_anthropic_cache.rs`.
The goal is to isolate Anthropic-compatible prompt-cache behavior from pre-public runtime
runtime flow, model exploration, compaction, and benchmark task variance.

## Why This Exists

Recent Anthropic benchmark runs showed that pre-public runtime still has poor cache hit
rates compared with Claude CLI, even after context-management and diagnostics
work. Full benchmark runs are too noisy to identify the root request-shape
cause. A smaller live matrix is needed before changing the Anthropic transport.

## Claude Code Reference Points

Claude Code's cache strategy differs from pre-public runtime in several important ways:

- It sends beta request metadata through `beta.messages.create`, including
  Claude Code and prompt-cache-scope betas for agentic calls.
- It intentionally uses exactly one message-level `cache_control` marker per
  request in `addCacheBreakpoints`.
- It latches dynamic beta/header state so mid-session feature flips do not
  change the server-side cache key.
- It has explicit prompt-cache break detection that compares model, tools,
  system blocks, cache-control shape, beta list, effort, and extra body params.
- It can place `cache_control` on tool schemas; pre-public runtime currently only marks
  prompt/system/conversation content blocks.

pre-public runtime's current Anthropic lowering is more aggressive:

- `runtime/provider_turn.rs` marks both the last `stable` and last
  `agent_scoped` prompt blocks.
- `provider/transports/anthropic.rs` adds a rolling marker to the last
  cacheable conversation content block.
- The resulting request can contain multiple `cache_control` markers across
  system, context, and message content.
- pre-public runtime does not currently send Claude-like `betas` in the Anthropic body.

## Live Harness

Run compile-only verification:

```bash
cargo test --test live_anthropic_cache --no-run
```

Run the live matrix using credentials and base URL from `~/.claude/settings.json`:

```bash
cargo test --test live_anthropic_cache -- --ignored --nocapture
```

Run against the BigModel model name observed in Claude CLI benchmark events:

```bash
PRE-PUBLIC RUNTIME_LIVE_ANTHROPIC_MODEL=glm-4.7 \
cargo test --test live_anthropic_cache -- --ignored --nocapture
```

Run with Claude-like beta metadata:

```bash
PRE-PUBLIC RUNTIME_LIVE_ANTHROPIC_MODEL=glm-4.7 \
PRE-PUBLIC RUNTIME_LIVE_ANTHROPIC_BETAS=claude-code-20250219,prompt-caching-scope-2026-01-05 \
cargo test --test live_anthropic_cache -- --ignored --nocapture
```

The harness prints `input`, `cache_read`, `cache_creation`, and `output` for
each case. It intentionally does not require cache hits to pass because cache
misses are the diagnostic signal.

Useful controls:

- `PRE-PUBLIC RUNTIME_LIVE_ANTHROPIC_REPEAT`: repeat each two-request strategy pair.
  Default: `3`.
- `PRE-PUBLIC RUNTIME_LIVE_ANTHROPIC_SECTIONS`: controls synthetic stable prefix size.
  Default: `90`, which is roughly 4.5k input tokens in local BigModel runs.
- `PRE-PUBLIC RUNTIME_LIVE_ANTHROPIC_STRATEGIES`: comma-separated strategy names to run.
  This is useful when probing larger prefixes without spending a full matrix.

Strategy names currently covered:

- `exact_single_message_marker`
- `moving_tail_only`
- `previous_and_current_tail`
- `previous_tail_only`
- `runtime-incubation_like_multi_marker`
- `large_system_marker`
- `two_system_markers`
- `system_and_tail_marker`
- `small_tool_marker`
- `large_tool_marker`
- `claude_cli_like`

## Current Local Observations

The first runs used `https://open.bigmodel.cn/api/anthropic` from Claude
settings.

With the Claude settings model `opus[1m]`, every case returned
`cache_read=0` and `cache_creation=0`. This means the raw request did not match
the effective cacheable shape used by successful benchmark runs.

With `PRE-PUBLIC RUNTIME_LIVE_ANTHROPIC_MODEL=glm-4.7`, one run produced hits for exact
single-message repeat and large tool marker:

```text
exact_single_marker_2 input=104 cache_read=4480 cache_creation=0
large_tool_marker_2 input=75 cache_read=4736 cache_creation=0
```

With `glm-4.7` plus Claude-like `betas`, one run produced hits for a large
system marker and a previous+current tail case:

```text
previous_and_current_tail_2 input=125 cache_read=4480 cache_creation=0
large_system_marker_2 input=69 cache_read=4480 cache_creation=0
```

Across repeated runs, the same matrix can also return all zero cache reads.
This matches the benchmark symptom classified as likely server-side drop, but
the live matrix also shows that request shape matters: pre-public runtime-like multi-marker
requests have not produced hits in these local runs.

After changing the harness to run repeated strategy pairs, the default Claude
settings model `opus[1m]` still produced no hits:

```text
model=opus[1m] betas=none repeat=2 sections=90
strategy=exact_single_message_marker runs=2 hits=0
strategy=moving_tail_only runs=2 hits=0
strategy=previous_and_current_tail runs=2 hits=0
strategy=previous_tail_only runs=2 hits=0
strategy=runtime-incubation_like_multi_marker runs=2 hits=0
strategy=large_system_marker runs=2 hits=0
strategy=two_system_markers runs=2 hits=0
strategy=system_and_tail_marker runs=2 hits=0
strategy=small_tool_marker runs=2 hits=0
strategy=large_tool_marker runs=2 hits=0
```

With `glm-4.7`, no betas, and the default ~4.5k prefix, hits were sparse:

```text
model=glm-4.7 betas=none repeat=3 sections=90
strategy=exact_single_message_marker runs=3 hits=1 avg_second_cache_read=1493
strategy=moving_tail_only runs=3 hits=0
strategy=previous_and_current_tail runs=3 hits=0
strategy=previous_tail_only runs=3 hits=0
strategy=runtime-incubation_like_multi_marker runs=3 hits=0
strategy=large_system_marker runs=3 hits=0
strategy=two_system_markers runs=3 hits=0
strategy=system_and_tail_marker runs=3 hits=0
strategy=small_tool_marker runs=3 hits=1 avg_second_cache_read=42
strategy=large_tool_marker runs=3 hits=0
```

With `glm-4.7`, Claude-like betas, and the default ~4.5k prefix:

```text
model=glm-4.7 betas=claude-code-20250219,prompt-caching-scope-2026-01-05 repeat=5 sections=90
strategy=exact_single_message_marker runs=5 hits=0
strategy=moving_tail_only runs=5 hits=0
strategy=previous_and_current_tail runs=5 hits=0
strategy=previous_tail_only runs=5 hits=0
strategy=runtime-incubation_like_multi_marker runs=5 hits=1 avg_second_cache_read=896
strategy=large_system_marker runs=5 hits=0
strategy=two_system_markers runs=5 hits=0
strategy=system_and_tail_marker runs=5 hits=0
strategy=small_tool_marker runs=5 hits=3 avg_second_cache_read=76
strategy=large_tool_marker runs=5 hits=1 avg_second_cache_read=947
```

With `glm-4.7`, Claude-like betas, and a larger ~17.6k prefix:

```text
model=glm-4.7 betas=claude-code-20250219,prompt-caching-scope-2026-01-05 repeat=3 sections=350
strategy=exact_single_message_marker runs=3 hits=0
strategy=moving_tail_only runs=3 hits=0
strategy=previous_and_current_tail runs=3 hits=1 avg_second_cache_read=5888
strategy=runtime-incubation_like_multi_marker runs=3 hits=1 avg_second_cache_read=5888
strategy=large_system_marker runs=3 hits=0
strategy=large_tool_marker runs=3 hits=0
```

With `glm-5.1`, Claude-like betas, and the same larger ~17.6k prefix:

```text
model=glm-5.1 betas=claude-code-20250219,prompt-caching-scope-2026-01-05 repeat=10 sections=350
strategy=exact_single_message_marker runs=10 hits=5 avg_second_cache_read=8832
strategy=moving_tail_only runs=10 hits=1 avg_second_cache_read=1766
strategy=previous_and_current_tail runs=10 hits=4 avg_second_cache_read=7065
strategy=runtime-incubation_like_multi_marker runs=10 hits=5 avg_second_cache_read=8832
```

This is materially different from `glm-4.7`: `glm-5.1` can reuse the cached
prefix much more often, but still not reliably. It also confirms that the
earlier `moving_tail_only` 2/2 run was not stable; at repeat 10 it fell to 1/10.

After adding a stricter `claude_cli_like` strategy, the gap became clear. This
case simulates Claude Code's request shape more closely:

- Claude Code style system split:
  - billing/attribution block without cache marker
  - CLI system prefix with cache marker
  - large stable system block with cache marker
- normal tool schema in the `tools` array
- body `metadata`
- body `temperature`
- body `betas`
- only one message-level cache marker, on the latest message tail

Non-streaming `claude_cli_like` against `glm-5.1`:

```text
model=glm-5.1 betas=claude-code-20250219,prompt-caching-scope-2026-01-05 repeat=10 sections=350 stream=false
strategy=moving_tail_only runs=10 hits=2 avg_second_cache_read=3532
strategy=runtime-incubation_like_multi_marker runs=10 hits=4 avg_second_cache_read=7065
strategy=claude_cli_like runs=9 hits=9 avg_second_cache_read=35456
```

One `claude_cli_like` iteration failed because the peer closed the TLS
connection; all successful iterations hit the cache.

Streaming `claude_cli_like` against `glm-5.1`:

```text
model=glm-5.1 betas=claude-code-20250219,prompt-caching-scope-2026-01-05 repeat=5 sections=350 stream=true
strategy=claude_cli_like runs=5 hits=5 avg_second_cache_read=35456
```

This shows that streaming is not the root cause. The decisive difference is
the request shape.

## Working Conclusion

The cache problem should not be treated as only a provider-side miss. The
current evidence points to a combination of:

- Provider/backend cache instability under `open.bigmodel.cn`.
- pre-public runtime request shapes with too many cache-control markers.
- Rolling conversation-tail markers that move every turn and do not reliably
  reuse the previous cached prefix.
- Missing Claude-like beta metadata in the Anthropic request body.
- No stable tool-schema cache marker, even though tools are a large stable
  portion of real agent requests.

The strongest live result so far is negative: `moving_tail_only` is consistently
the weakest strategy. It was 0/3 under `glm-4.7` with a larger prefix and 1/10
under `glm-5.1`. This is the closest probe to pre-public runtime's rolling conversation tail
behavior when prior markers are not preserved as a usable cache boundary.

`previous_and_current_tail` and `runtime-incubation_like_multi_marker` can hit, especially
with a larger prefix and `glm-5.1`, but only partially. They do not match
Claude CLI's high hit rate because they do not create the same stable cached
system prefix.

The model identity matters. `glm-5.1` is a better cache candidate than
`glm-4.7` in these probes. Benchmark comparisons should avoid mixing
`opus[1m]`, `glm-4.7`, and `glm-5.1` aliases without recording the effective
model because the cache behavior is not equivalent.

The current root-cause hypothesis is now:

- Claude CLI gets high hit rate because it caches a large, stable system prefix
  and then adds one message-level tail marker.
- pre-public runtime's synthetic/current request shape has too much important context in
  message content and multiple unstable markers, so a latest-tail marker often
  has no stable cached base to reuse.
- Matching Claude CLI likely requires moving stable prompt/context material
  into structured system/cache blocks, not just changing the rolling tail
  marker.

## Proposed Direction

Use the live harness to validate one transport change at a time. The first
production change should be conservative and easy to revert:

- Add Anthropic request-shape diagnostics for `betas`, `cache_control` count,
  cache-control locations, and whether the request is using the Claude-like
  body `betas` field.
- Add configurable Anthropic body betas, initially off by default, with a
  benchmark profile that sends `claude-code-20250219` and
  `prompt-caching-scope-2026-01-05`.
- Record the effective model name in benchmark cache diagnostics. The live
  probes show materially different behavior for `opus[1m]`, `glm-4.7`, and
  `glm-5.1`.
- Stop relying on a marker that only moves to the newest rolling tail. Live
  probes show this has no reliable reuse path.
- Add a Claude-like Anthropic lowering mode:
  - split system into stable cacheable blocks and dynamic non-cacheable blocks
  - place the large stable prompt/context prefix in cacheable system blocks
  - keep exactly one message-level marker on the latest tail
  - include stable `metadata`, `temperature`, and body `betas`
- Treat this as the leading candidate for the next benchmark. It is the only
  live strategy that reproduced Claude-like high hit rates.

The likely next A/B is:

- `current`: existing pre-public runtime multi-marker strategy.
- `current`: existing pre-public runtime multi-marker strategy.
- `claude_cli_like`: stable system cache blocks plus exactly one message-level
  tail marker.
- `latest_tail_only`: useful as a negative control; it should not be the final
  default.

Success criteria should be measured by repeated live probe runs and then by one
full benchmark: higher effective cache hit ratio, fewer high-input zero-cache
rounds, and no regression in task completion.
