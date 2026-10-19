# 070 MiniMax Model Catalog

## October 20, 2026 update

Add the exact official `MiniMax-M3.1-Flash` to international/China general API
and `m-plan` routes. The existing Token Plan uses the subscription route rather
than a separate endpoint. China general API uses its documented
`api.minimax.cn/anthropic` base and independent `MINIMAX_CN_API_KEY`; general
and subscription credentials must not be substituted for each other.

The official model has image input, 1,000,000 context, 131,072 recommended
output and 524,288 maximum output. Only this model and its preserved Preview
compatibility ID use exact `output_config.effort` values
`low/medium/high/xhigh/max`; omit generic `thinking`/`budget_tokens`.
Preview is no longer an independent offer but its old name remains served.
Do not extend those efforts to M3/M2 or remove historical models based only
on their absence from the current model table.

See [the endpoint review](../../models.dev/endpoint-review.md) for current
official sources and remaining lifecycle/qualification gaps. The original
decision below records the earlier M3/M2 baseline, not the M3.1 contract.

## Choice

Keep the built-in direct MiniMax catalog limited to the eight models currently
listed for the Anthropic-compatible endpoint:

- `MiniMax-M3`
- `MiniMax-M2.7`
- `MiniMax-M2.7-highspeed`
- `MiniMax-M2.5`
- `MiniMax-M2.5-highspeed`
- `MiniMax-M2.1`
- `MiniMax-M2.1-highspeed`
- `MiniMax-M2`

Mark only `MiniMax-M3` as accepting image input. Keep reasoning as an intrinsic
capability for all eight models, but do not expose reasoning effort options.

Retain the existing context and output limits. MiniMax documents a
1,000,000-token context window for M3 and 204,800 tokens for M2.x. Its current
public model pages do not provide a replacement for the existing output limits.

## Reason

MiniMax's Anthropic compatibility documentation explicitly lists all eight
models, so there is no official basis to remove the older M2.x entries.

The endpoint accepts image and video content only for M3. Holon currently
models image input but not video input, so M3 becomes a vision candidate while
M2.x remains text-only.

MiniMax thinking control does not map to Holon's reasoning effort contract:
M3 supports adaptive thinking on or off, while M2.x thinking cannot be
disabled. Advertising effort levels would therefore overstate the implemented
Anthropic route.

## Sources

Verified 2026-07-12:

- <https://platform.minimax.io/docs/api-reference/text-anthropic-api>
- <https://platform.minimax.io/docs/guides/text-generation>
