# 053 Anthropic-Compatible Provider Probes

Holon keeps `deepseek` and `xiaomi` on the existing OpenAI Chat Completions
transport while adding explicit `deepseek-anthropic` and `xiaomi-anthropic`
provider ids for their Anthropic-compatible endpoints.

The compatible providers use the same API keys as the default providers:
`DEEPSEEK_API_KEY` and `XIAOMI_API_KEY`.

This keeps the default user path stable while giving operators a concrete live
smoke path for the Anthropic Messages transport. The default transport should
only move to Anthropic after live validation confirms that both providers accept
Holon's tool-use request shape with `context_management` enabled.

The validation tests are ignored by default because they require real
credentials and network access:

```bash
cargo test --test live_anthropic_compatible -- --ignored
```
