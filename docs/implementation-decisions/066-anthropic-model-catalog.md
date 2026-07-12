# Anthropic Model Catalog

## Choice

The built-in `anthropic` catalog follows the generally available models in
Anthropic's official model overview:

- `claude-fable-5`
- `claude-opus-4-8`
- `claude-sonnet-5`
- `claude-haiku-4-5`

The first three use a 1M-token context window and 128k-token synchronous output
limit. Haiku uses a 200k-token context window and 64k-token output limit. All
four accept image input and expose reasoning capability.

## Reason

This list was verified against
<https://platform.claude.com/docs/en/about-claude/models/overview> on
2026-07-12. Older Opus 4.5/4.6/4.7 and Sonnet 4.5/4.6 entries are omitted from
the conservative built-in catalog because they are no longer in the current
generally available comparison table.

The Haiku entry uses Anthropic's supported `claude-haiku-4-5` API alias rather
than pinning Holon's default catalog to the dated
`claude-haiku-4-5-20251001` snapshot. Invitation-only Mythos models are not
included.

## Preserved boundary

`supports_reasoning` records intrinsic model capability. The Anthropic
transport can lower configured reasoning effort to a thinking budget, but this
catalog update does not introduce new user-selectable effort levels.
