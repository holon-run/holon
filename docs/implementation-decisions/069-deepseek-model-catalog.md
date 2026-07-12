# 069 DeepSeek Model Catalog

## Choice

Keep the built-in direct DeepSeek catalog limited to the current API models:

- `deepseek-v4-flash`
- `deepseek-v4-pro`

Both models use a 1,000,000-token context window and support up to 384,000
output tokens. Record reasoning as an intrinsic capability, but do not expose
reasoning effort options through the current Anthropic-compatible route.

Remove `deepseek-chat` and `deepseek-reasoner` from the selectable catalog.
They are compatibility names mapped to the non-thinking and thinking modes of
`deepseek-v4-flash`, and DeepSeek has scheduled their deprecation for
2026-07-24 15:59 UTC.

## Reason

DeepSeek's current model table lists only the V4 Flash and Pro model codes.
Keeping the compatibility names selectable would duplicate one underlying
model and retain aliases with an announced retirement date.

The Anthropic-compatible endpoint supports thinking and accepts
`output_config.effort`, but Holon's Anthropic transport currently lowers
`reasoning_effort` to Anthropic `thinking.budget_tokens`. DeepSeek explicitly
ignores `budget_tokens`, so advertising adjustable effort would overstate the
implemented route contract.

Image and document message blocks are not supported by DeepSeek's Anthropic
endpoint, so neither model is a vision candidate.

## Sources

Verified 2026-07-12:

- <https://api-docs.deepseek.com/quick_start/pricing/>
- <https://api-docs.deepseek.com/guides/thinking_mode/>
- <https://api-docs.deepseek.com/guides/anthropic_api/>
