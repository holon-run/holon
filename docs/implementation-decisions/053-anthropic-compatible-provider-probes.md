# 053 Anthropic-Compatible Provider Probes

When a provider family exposes both Anthropic Messages and OpenAI-compatible
interfaces, Holon keeps explicit `provider-anthropic` and `provider-openai`
entries. The bare `provider` id is the current default alias for that family.
The default currently prefers Anthropic Messages for agent/coding use after live
validation confirms Holon's tool-use continuation shape with
`context_management` enabled.

DeepSeek follows this family shape:
`deepseek`, `deepseek-anthropic`, and `deepseek-openai`.

Xiaomi has two product entries because the default MiMo API and token-plan API
use separate base URLs and API keys. Each product entry still exposes both
transport variants:
`xiaomi`, `xiaomi-anthropic`, `xiaomi-openai`, `xiaomi-token-plan`,
`xiaomi-token-plan-anthropic`, and `xiaomi-token-plan-openai`.

Z.ai and BigModel are separate provider families even though both expose Zhipu
models, because they use separate account systems and base URLs. Holon exposes
`zai`, `zai-anthropic`, `zai-openai`, `bigmodel`, `bigmodel-anthropic`, and
`bigmodel-openai`.

DeepSeek uses `DEEPSEEK_API_KEY`. Xiaomi uses `XIAOMI_API_KEY`. Xiaomi
token-plan uses `XIAOMI_TOKEN_PLAN_API_KEY`. Z.ai uses `ZAI_API_KEY`. BigModel
uses `BIGMODEL_API_KEY`.

The validation tests are ignored by default because they require real
credentials and network access:

```bash
cargo test --test live_anthropic_compatible -- --ignored
```
