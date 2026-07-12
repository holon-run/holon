# 070 MiniMax Model Catalog

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
