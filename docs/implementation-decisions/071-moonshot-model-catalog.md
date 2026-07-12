# 071 Moonshot Model Catalog

## Choice

Keep the built-in direct Moonshot catalog aligned with the models accepted by
the current Chat Completions schema:

- `kimi-k2.7-code`
- `kimi-k2.7-code-highspeed`
- `kimi-k2.6`
- `kimi-k2.5`
- the text, vision preview, and automatic-routing Moonshot V1 variants

Remove the Kimi K2 Thinking and Turbo entries that are no longer in the current
model list. Mark the four current Kimi models as reasoning and image-input
capable. Do not expose reasoning effort options.

## Reason

Moonshot discontinued the Kimi K2 Thinking series on May 25, 2026. Its current
model list and Chat Completions schema instead advertise K2.7 Code, K2.6, K2.5,
and Moonshot V1.

K2.7 Code always thinks, while K2.6 and K2.5 accept Moonshot's
provider-specific `thinking` object. Holon's OpenAI-compatible transport does
not implement that control, so intrinsic reasoning remains visible without
claiming an unsupported effort-level contract.

Moonshot documents the Kimi models as native multimodal models accepting text,
image, and video. Holon currently models image input but not video input.

## Sources

Verified 2026-07-12:

- <https://platform.kimi.ai/docs/models.md>
- <https://platform.kimi.ai/docs/api/chat.md>
- <https://platform.kimi.ai/docs/api/models-overview.md>
- <https://platform.kimi.ai/docs/pricing/chat-k25.md>
- <https://platform.kimi.ai/docs/pricing/chat-k26.md>
- <https://platform.kimi.ai/docs/pricing/chat-k27-code.md>
