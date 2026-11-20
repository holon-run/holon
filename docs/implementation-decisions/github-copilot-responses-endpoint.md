# GitHub Copilot per-model Responses routing

## Decision

Copilot models whose discovered catalog entry declares `/responses` in
`supported_endpoints` route to `github-copilot@responses` (OpenAI Responses
transport); all other discovered models stay on `github-copilot@default`
(OpenAI Chat Completions). The mapping happens in OpenAI-compatible model
discovery and only for the `github-copilot` provider.

## Reason

- The modern Copilot catalog (observed 2026-11-20, 53 models) serves current
  OpenAI-family models such as `gpt-6.2-sol` only on `/responses`; the legacy
  `/chat/completions` surface cannot reach them. A single provider-wide
  transport would either lose the modern models or break the legacy ones.
- Holon already routes discovered models to registered per-provider endpoints
  (the opencode-go `default`/`messages`/`responses` pattern), so no new
  routing mechanism is needed.
- Models declaring both `/chat/completions` and `/responses` prefer
  `/responses`; models with no endpoint declaration keep the historical
  Chat Completions route.

## Preserved boundary

- Native Claude `/v1/messages` routing is not added here; Claude models remain
  reachable through Copilot's Chat Completions translation surface.
- The base URL stays `https://api.githubcopilot.com`; per-account endpoint
  discovery (`/copilot_internal/user`) is not implemented because the generic
  base admitted the same catalog in the live probe.
- Other OpenAI-compatible providers may send `supported_endpoints`; Holon
  ignores it for them and keeps their models on the default endpoint.
