# Vercel AI Gateway model catalog

Holon keeps a small built-in Vercel AI Gateway picker set and augments it with
the gateway's public Models API. The snapshot was verified against that API on
2026-07-13. Built-in context and output limits follow the API values for Claude
Opus 4.6, GPT 5.4, GPT 5.4 Pro, and Kimi K2.6.

Vercel discovery accepts only entries whose `type` is `language`. It maps
`context_window`, `max_tokens`, and the `vision` and `reasoning` tags into
remote model metadata. Tags do not establish parallel tool-call behavior or a
portable discrete reasoning-effort vocabulary, so Holon does not infer those
controls. The Models API is public, so metadata refresh does not require a
configured gateway credential even though inference does.

The Vercel provider uses the Anthropic Messages API at the documented
`https://ai-gateway.vercel.sh` base URL. Holon appends `/v1/messages`; reasoning
requests use the transport's existing Anthropic `thinking.budget_tokens`
lowering only when the operator configures a supported reasoning effort.

Sources:

- Vercel AI Gateway Models API:
  `https://ai-gateway.vercel.sh/v1/models`
- Vercel AI Gateway models and providers:
  `https://vercel.com/docs/ai-gateway/models-and-providers`
- Vercel Anthropic Messages API:
  `https://vercel.com/docs/ai-gateway/sdks-and-apis/anthropic-messages-api`
- Vercel reasoning overview:
  `https://vercel.com/docs/ai-gateway/capabilities/reasoning`
