# Live LLM Baseline Tests

Holon keeps the default test suite deterministic and provider-free. Real LLM
behavior is covered by an ignored/manual baseline suite that is intended for
release validation and for changes that touch provider transports, context
projection, compaction, or prompt-cache strategy.

## Command

Run the baseline explicitly:

```bash
make test-live
```

This runs the configured-chain smoke and tool-roundtrip tests from
`live_llm_baseline`, followed by `live_provider_smoke`. The Anthropic-only
prompt-cache probe stays in `make test-live-anthropic` so the baseline does not
silently require Anthropic credentials. The suite requires configured provider
credentials and network access. By default, the baseline exercises the first
configured provider model in the runtime model chain. Set
`HOLON_LIVE_BASELINE_MAX_MODELS` to include more configured models:

```bash
HOLON_LIVE_BASELINE_MAX_MODELS=3 make test-live
```

## Coverage

The baseline is intentionally broader than cache validation:

- provider smoke: a configured model accepts a real request and returns output
- tool roundtrip: a configured model emits a real tool call for a supplied schema
- prompt cache: Anthropic reports cache read tokens on a repeated stable prefix

Provider-specific live tests remain in files such as `tests/live_anthropic.rs`,
`tests/live_codex.rs`, and `tests/live_anthropic_cache.rs`. The baseline suite
is the release-oriented entry point that should stay small, actionable, and
representative.

Use the focused targets when validating one provider family or runtime surface:

```bash
make test-live-openai
make test-live-anthropic
make test-live-codex
make test-live-xai
make test-live-images
make test-live-runtime
```

Each target prints its credential boundary and exact test binaries before
running ignored tests. Live targets are manual and are not part of the
credential-free default CI.

`make test-live-anthropic` includes the Anthropic prompt-cache baseline and the
broader Anthropic-compatible provider matrix. It may require multiple
provider-specific credentials and incur higher cost than the baseline target.

## Required configuration

- normal Holon provider configuration from `AppConfig::load()`
- credentials for the configured provider chain
- `ANTHROPIC_AUTH_TOKEN` or equivalent configured Anthropic credentials for the
  prompt-cache baseline
- optional `HOLON_LIVE_ANTHROPIC_MODEL` to override the Anthropic cache model
- optional `HOLON_LIVE_BASELINE_MAX_MODELS` to widen the provider smoke/tool
  matrix

## Diagnostics

Tests print the provider/model refs, token counts, tool inputs, and cache usage
that caused a pass or failure. A failure should identify whether the regression
is provider construction, basic request transport, tool lowering, or observable
cache behavior rather than returning a generic provider error only.
