# NEAR AI Cloud model catalog

Holon keeps a small built-in NEAR AI Cloud picker set and augments it with the
gateway's public Models API. The snapshot was verified against
`https://cloud-api.near.ai/v1/models` on 2026-07-13. The built-in context,
output, reasoning, and image-input metadata follows the API values for GLM 5.1,
Qwen 3.6 35B A3B, Qwen 3.5 122B A10B, Qwen3 VL 30B A3B, and Gemma 4 31B.

NEAR AI discovery maps `context_length`, `max_output_length`,
`input_modalities`, and `supported_features` into remote route metadata. It
includes only entries whose `is_ready` value is explicitly `true`, because the
upstream API uses that field to distinguish models ready for routing from
catalog entries that are not currently enabled.

The Models API is public, so metadata refresh does not require a configured
NEAR AI credential even though inference does. The `tools` feature does not
establish portable parallel tool-call semantics, and the API does not publish a
discrete reasoning-effort vocabulary, so Holon does not infer either.

Sources:

- NEAR AI Cloud available models:
  `https://docs.near.ai/cloud/models/`
- NEAR AI OpenAI compatibility:
  `https://docs.near.ai/cloud/guides/openai-compatibility/`
- NEAR AI Cloud Models API:
  `https://cloud-api.near.ai/v1/models`
- NEAR AI Cloud API implementation:
  `https://github.com/nearai/cloud-api`
