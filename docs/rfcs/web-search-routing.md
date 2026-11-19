---
title: RFC: Per-call WebSearch routing
date: 2026-11-19
status: implemented
---

# Per-call WebSearch routing

`WebSearch` keeps a single optional `provider` argument. Omission inherits the
configured provider and mode; explicit `auto` inherits only the mode and
uses the automatic candidates. Explicit `aggregate` uses those same
candidates and aggregates for this call, regardless of the default mode.
A concrete ID is single-provider with no fallback. The selectors are
interpreted only at the WebSearch call boundary. If an explicit alias matches
a registered ID, reject it before dispatch rather than guessing the intent.
Configured IDs, WebFetch and model-native search are unchanged.

Aggregation retains serial dispatch, canonical URL deduplication, and an early
stop at the requested result count. The global enable switch, provider checks,
and attempt budget still apply. Allowing aggregation over a `single` default
can disclose a query to multiple providers, with corresponding costs and
latency. A budget of one caps attempts per call; it is not an allowlist.

The response shape is unchanged. `mode` reports the effective mode,
`provider_attempts` lists real attempted IDs, and `provider` reports the winner
when present or the selector otherwise. Aggregation never reports a winner.

## Standalone native search providers

Native server-side search providers registered under `web.providers` with a
native kind run only when their exact ID is passed as `WebSearch.provider`.
They never join the omitted-provider inheritance, `auto` candidates,
`aggregate` fan-out, or any fallback path, even when listed in
`web.search.providers`. Each native provider binds exactly one
`web.providers.<id>.model_route`; the bound route decides transport, endpoint,
model, and credentials (an OpenAI route uses the Responses API web search
tool, a Codex route reuses the Codex transport with its OAuth credential and
native search request shape).

The standalone request boundary carries only the query and the native search
parameters: no conversation history, workspace data, or other tools. Results
come only from structured provider citations (`url_citation` annotations),
deduplicated by URL and truncated to `max_results`; missing optional fields
stay empty and `source` keeps the provider attribution. A completed search
with no mappable sources is a success with empty `results`; a response with
no search-tool evidence is an explicit failure, as are unparseable or
unresolvable routes, missing capability, auth, rate-limit, and timeout
conditions, which map onto the existing WebSearch error kinds without leaking
credentials or raw upstream requests. Every failure is terminal: native
dispatch never retries across routes or falls back to other providers.
