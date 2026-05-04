# 053 Anthropic-Compatible Provider Probes

Holon uses the Anthropic Messages transport as the default transport for
`deepseek` and `xiaomi` after live validation confirmed both providers accept
Holon's tool-use request shape with `context_management` enabled.

Holon also keeps explicit `deepseek-anthropic` and `xiaomi-anthropic` provider
ids as stable aliases for the Anthropic-compatible endpoints.

The compatible providers use the same API keys as the default providers:
`DEEPSEEK_API_KEY` and `XIAOMI_API_KEY`.

The default provider ids and their explicit Anthropic aliases use the same API
keys. DeepSeek uses `https://api.deepseek.com/anthropic`; Xiaomi uses the token
plan endpoint `https://token-plan-cn.xiaomimimo.com/anthropic`.

The validation tests are ignored by default because they require real
credentials and network access:

```bash
cargo test --test live_anthropic_compatible -- --ignored
```
