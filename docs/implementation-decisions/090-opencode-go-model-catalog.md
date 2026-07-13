# OpenCode Go model catalog

Holon models OpenCode Go as one canonical provider with two transport
endpoints. The default endpoint uses OpenAI Chat Completions, while the
`messages` endpoint uses Anthropic Messages. Models remain addressable as
`opencode-go/<model>` and the catalog selects the transport published in the
official OpenCode Go endpoint table.

The public `GET https://opencode.ai/zen/go/v1/models` response contains only
model identifiers and currently includes entries that are not present in the
endpoint table. Discovery therefore acts as a conservative intersection: it
keeps only endpoint-table models and assigns each accepted identifier to its
published transport. It does not infer capabilities or limits from names.

Static limits and intrinsic capabilities are reused only where the same model
has already been calibrated from an official upstream source. Missing output
limits remain unset. Reasoning support does not imply configurable effort
levels because OpenCode Go does not publish portable reasoning controls.

Sources reviewed on 2026-07-13:

- <https://opencode.ai/docs/go/>
- <https://opencode.ai/zen/go/v1/models>
