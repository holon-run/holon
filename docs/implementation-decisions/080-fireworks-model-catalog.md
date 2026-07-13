# Fireworks model catalog

Holon snapshots Fireworks AI models that the official model library marks
`Ready`, exposes through the serverless API, and documents as supporting
function calling or image input. The snapshot was verified on 2026-07-13.

The built-in catalog records context windows and image input only when the
individual Fireworks model page publishes them. Fireworks does not publish a
maximum output-token limit on those pages, so Holon leaves that limit unset
rather than treating the context window as a generation limit. Qwen 3.6 Plus
currently reports its context length as `N/A`, which is also kept unknown.

Reasoning controls follow the Fireworks chat-completions API contract where it
names model-family behavior. DeepSeek V4 exposes `none`, `low`, `medium`,
`high`, `xhigh`, and `max`; GLM 5.2 exposes disabled, High, and Max tiers; GPT
OSS 120B and MiniMax M2 accept `low`, `medium`, and `high`. Other reasoning
models remain marked as reasoning-capable without claiming adjustable effort
levels that Fireworks does not explicitly document for that route.

The former Kimi K2.5 Turbo Fire Pass router is not retained in the default
serverless model picker because its historical router page is not part of the
current ready serverless model set. Users may still configure arbitrary
Fireworks model or router IDs explicitly.

Sources:

- Fireworks model library sitemap: `https://fireworks.ai/sitemap.xml`
- Fireworks platform model availability:
  `https://docs.fireworks.ai/faq/models/availability/platform-models`
- Fireworks reasoning guide: `https://docs.fireworks.ai/guides/reasoning`
- Fireworks chat completions API:
  `https://docs.fireworks.ai/api-reference/post-chatcompletions`
- Individual current model pages under:
  `https://fireworks.ai/models/fireworks/`
