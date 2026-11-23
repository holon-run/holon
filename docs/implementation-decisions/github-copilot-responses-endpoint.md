# GitHub Copilot Responses-default routing

## Decision

`github-copilot@default` is the OpenAI Responses transport. Copilot models
whose discovered catalog entry declares `/responses` in `supported_endpoints`
use that default route; every other discovered model routes to
`github-copilot@chat-completions` (OpenAI Chat Completions). The mapping
happens in OpenAI-compatible model discovery and only for the
`github-copilot` provider.

## Reason

- The modern Copilot catalog (observed 2026-11-20, 53 models) serves current
  OpenAI-family models such as `gpt-6.2-sol` only on `/responses`; the legacy
  `/chat/completions` surface cannot reach them. A single provider-wide
  transport would either lose the modern models or break the legacy ones.
- A read-only catalog probe on 2026-11-23 showed that most entries declare
  `/responses`, so unqualified references now use the Responses transport.
- Holon already routes discovered models to registered per-provider endpoints
  (the opencode-go `default`/`messages`/`responses` pattern), so no new
  routing mechanism is needed.
- Models declaring both `/chat/completions` and `/responses` use Responses.
  Without an HTTP `/responses` declaration, discovery retains the old Chat
  Completions route. This is not a guarantee of Chat Completions support for
  entries that only declare another API or declare no endpoints.

## Preserved boundary

- Native Claude `/v1/messages` routing is not added here; Claude models remain
  reachable through Copilot's Chat Completions translation surface.
- The base URL stays `https://api.githubcopilot.com`; per-account endpoint
  discovery (`/copilot_internal/user`) is not implemented because the generic
  base admitted the same catalog in the live probe.
- Other OpenAI-compatible providers may send `supported_endpoints`; Holon
  ignores it for them and keeps their models on the default endpoint.
- Old discovery cache entries are remapped on load to preserve their original
  transport. Saved model references are not rewritten: replace explicit
  `github-copilot@responses/<model-id>` references with
  `github-copilot/<model-id>`, and re-select the discovered
  `github-copilot@chat-completions/<model-id>` route for Chat Completions models.
