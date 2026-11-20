# Keyless default WebSearch fallback chain

## Choice

The zero-configuration managed WebSearch order is a fixed keyless chain
`firecrawl -> keenable -> duckduckgo`, appended after explicitly configured
providers (which keep their capability-priority ordering). Keenable and
Firecrawl advertise `api_key_optional` capability metadata, and Firecrawl
requests omit `Authorization` when no key is configured.

## Reason

Live probes for #4325 showed the previous single default, DuckDuckGo, fully
challenge-blocked (0/36 queries) on the tested network, while Firecrawl's
unauthenticated `/v1/search` returned 12/12 with good relevance and Keenable's
public endpoint returned 11/12. Expressing the chain as a fixed order keeps
`default_priority` unchanged: capability priorities still order explicitly
configured providers, and the operator preference (Firecrawl first) does not
have to leak into those priorities.

## Preserved boundary

- Explicitly configured `web.providers` entries and explicit
  `web.search.providers` lists still produce exactly the configured attempt
  order; builtin entries are skipped when a provider with that ID is
  configured.
- DuckDuckGo stays available as a kind and as the final fallback; nothing is
  removed or disabled.
- Keyed behavior is unchanged: a configured API key still authenticates
  Firecrawl (raising rate limits) and Keenable (switching to the keyed
  endpoint).
- `web.search.max_provider_attempts` still truncates the combined order, so a
  configuration with several explicit providers may drop the tail entries.
