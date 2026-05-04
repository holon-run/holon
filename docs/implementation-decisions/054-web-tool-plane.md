# Web Tool Plane

Holon exposes web access as a distinct tool capability family with stable
model-facing tools:

- `WebFetch` fetches one explicit `http` or `https` URL through runtime policy.
- `WebSearch` routes a query through Holon's web provider registry.

`WebFetch` is runtime-native because its real contract is not just HTTP. The
runtime owns SSRF checks, redirect checks, response limits, extraction,
external-content wrapping, and tool-ledger provenance. External formatters may
be added later, but they should operate on already-fetched bytes instead of
fetching URLs themselves.

`WebSearch` is model-facing but provider-routed. DuckDuckGo Lite is used only as
a key-free best-effort fallback, while SearXNG is the first self-hosted
provider. API-backed providers and native OpenAI/Anthropic/Gemini lowering stay
behind the provider boundary so they do not collide with the stable `WebSearch`
tool contract.
