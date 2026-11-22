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

`ListWebSearchProviders` takes no arguments and returns `{"providers":[...]}`
with sorted, unique IDs that the caller can pass unchanged to `WebSearch`.
It shares WebSearch's tool permission and uses local configuration only; it
does not send requests, start commands, or check provider health. It includes
the built-in `duckduckgo` route and qualifying IDs outside the automatic
candidate list, but not routing selectors or conversation-only native search.
With search disabled or no eligible route, it returns `{"providers":[]}`.

## Standalone native search providers

Native server-side search providers registered under `web.providers` with a
native kind run only when their exact ID is passed as `WebSearch.provider`.
They never join the omitted-provider inheritance, `auto` candidates,
`aggregate` fan-out, or any fallback path, even when listed in
`web.search.providers`. Each native provider binds exactly one
`web.providers.<id>.model_route`; the bound route decides transport, endpoint,
model, and credentials (an OpenAI route uses the Responses API web search
tool, a Codex route reuses the Codex transport with its OAuth credential and
native search request shape, and a Gemini route uses the GenerateContent API
`google_search` grounding tool).

The standalone request boundary carries only the query and the native search
parameters: no conversation history, workspace data, or other tools. Results
come only from structured provider citations (`url_citation` annotations for
OpenAI routes, `groundingMetadata` chunks for Gemini routes), deduplicated by
URL and truncated to `max_results`; missing optional fields stay empty and
`source` keeps the provider attribution. A completed search
with no mappable sources is a success with empty `results`; a response with
no search-tool evidence is an explicit failure, as are unparseable or
unresolvable routes, missing capability, auth, rate-limit, and timeout
conditions, which map onto the existing WebSearch error kinds without leaking
credentials or raw upstream requests. Every failure is terminal: native
dispatch never retries across routes or falls back to other providers.

A native provider with a bound `model_route` joins `ListWebSearchProviders`
as an explicit-callable ID; without a bound route it stays unlisted, and the
reserved native kinds remain undiscoverable until their dispatch lands.

Standalone responses are parsed with a dedicated search parser: a completed
search without answer text (search-call-only output, or citations attached to
blank text) is a successful search outcome, while malformed transport shape
still fails. Failure messages are sanitized through the provider error
redactor before they reach WebSearch diagnostics, and result dedupe keys use
the canonical URL so one page cited with different fragments occupies one slot.

### Derived native search providers

Writing a `web.providers.<id>.model_route` by hand is no longer required for
the standalone native path. Holon derives one native entry per model provider
that has usable credentials and declares a builtin native search capability
with standalone dispatch support (today the OpenAI native kind covering
`openai` and `openai-codex`, and the Gemini native kind covering `gemini`).
Derivation is on by default and mirrors the `vision.default` three-layer
structure: an explicit `web.providers` entry with
the same id wins over the derived entry, `web.search.native_default` pins a
single provider (`<provider-id>`), disables derivation (`off`), or restores
the default (`auto`, also what setting the key to `auto` writes back).

The derived entry binds the model catalog preferred route for its provider,
keeping the model chain `auto` expansion and the standalone search path on one
source of truth. Derived entries stay explicit-only: they never join the
omitted-provider inheritance, `auto` candidates, `aggregate` fan-out, or the
default provider selection, and reserved native kinds stay undiscoverable.
`ListWebSearchProviders` lists derived entries with a `derived` source marker
alongside `configured` and `builtin` entries. Only the WebSearch tool surface
sees derived entries; conversation builtin search and WebFetch keep the raw
web configuration.

### Derived standalone DeepSeek search provider

An official DeepSeek model credential also discovers a standalone search
entry, so an existing key is callable as `WebSearch(provider="deepseek")`
without a `web.providers` declaration. The existing credential resolver must
return a non-empty key for the `deepseek` model provider, and that provider
must resolve to the official API host over HTTPS (`api.deepseek.com`).
Third-party proxies, compatible-protocol endpoints, plain-HTTP endpoints, and
similarly named models never qualify.

The derived entry is pinned to the official endpoint: it never carries the
discovered base URL, so a discovered key cannot be sent to a custom address.
An explicit `web.providers.deepseek` entry wins and is never duplicated or
rewritten. Like the native derivation above, the discovered entry is
explicit-only: it is listed with the `derived` source marker but stays out of
the default, `auto`, and `aggregate` chains, and only naming it dispatches a
request.
