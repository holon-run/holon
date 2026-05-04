# 053 Anthropic-Compatible Provider Probes

Holon uses the Anthropic Messages transport as the default transport for
`deepseek` after live validation confirmed DeepSeek accepts Holon's tool-use
request shape with `context_management` enabled.

Holon also keeps explicit `deepseek-anthropic` as a stable alias for the
DeepSeek Anthropic-compatible endpoint.

Xiaomi's token-plan Anthropic-compatible endpoint is a separate service with a
separate base URL and API key from the default Xiaomi MiMo API. Holon therefore
keeps `xiaomi` on the default OpenAI-compatible endpoint and exposes token-plan
as the separate `xiaomi-token-plan` provider.

DeepSeek uses `https://api.deepseek.com/anthropic` with `DEEPSEEK_API_KEY`.
Xiaomi token-plan uses `https://token-plan-cn.xiaomimimo.com/anthropic` with
`XIAOMI_TOKEN_PLAN_API_KEY`.

The validation tests are ignored by default because they require real
credentials and network access:

```bash
cargo test --test live_anthropic_compatible -- --ignored
```
