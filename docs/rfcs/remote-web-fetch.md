---
title: RFC: Explicit remote WebFetch
date: 2026-10-18
status: implemented
---

# Explicit remote WebFetch

`WebFetch` remains local HTTP by default, including DNS pinning, per-hop
redirect validation, host rules, extraction, and response limits. An optional
`provider` argument explicitly selects a configured `kind=deepseek` web
provider and its independent `/v1/tools/fetch` API. `WebSearch` uses the same
provider registry entry and credential profile for `/v1/tools/search`.
This does not depend on the conversation model's transport or native search.

The `holon-web-retrieval` crate implements local and remote retrieval. The
application supplies resolved configuration and checks the tool capability.

## Boundary

Remote Fetch discloses the requested URL to a third-party paid service.
Holon validates the initial public HTTP(S) URL locally and rejects embedded
credentials. The service owns subsequent DNS resolution, redirects and page
extraction; Holon cannot pin that connection or inspect the redirect chain.
Remote fetching therefore rejects custom allow/deny host rules and custom
redirect limits, rather than silently bypassing them. Use local Fetch when
those policies are needed. The DeepSeek API connection itself never follows
redirects.

DeepSeek returns Markdown only, so its adapter accepts `extract_mode=auto`
and rejects `text` or `raw`. Holon bounds the API response bytes, applies its
character limit, hashes the complete returned Markdown before character
truncation, and wraps output as untrusted external content.

## Provenance

DeepSeek responses set `source=deepseek_remote`; `status` is the provider API's
HTTP status, and `content_type=text/markdown` describes its extracted content.
DeepSeek does not return the source page status or final URL. `final_url` is
therefore nullable (null for remote Fetch), not fabricated from the input URL.
Local Fetch continues returning a string final URL and the source HTTP status.
The content hash and byte count describe the returned page Markdown, not the
provider's JSON envelope or original source bytes.

## Credentials and failures

The web provider explicitly references an existing API key profile, including
the profile already used for DeepSeek model inference. No key is copied to
configuration, tool output, or logs. Unconfigured or unsupported providers fail
explicitly; Fetch does not fall back to another paid service.
Provider errors omit response bodies and secret-bearing transport errors.

Protocol source: [DeepSeek Search & Fetch documentation](https://api-docs.deepseek.com/guides/search/).

## Shared routing contract and discovery

`fetch_with_provider` is the shared contract for every remote adapter: a
named provider must be a configured registry ID whose kind supports remote
extraction (`remote_fetch_modes`; `deepseek` with `auto` only, `tavily`
with `auto` and `text`, `firecrawl` with `auto` only).
Unknown IDs, search-only kinds, and unsupported modes fail explicitly with
zero provider calls; there is no alias, aggregation, or fallback between
routes, and the local route keeps no `local` selector. Adapter
prerequisites — the enabled gate, site-policy restrictions, public URL
shape, embedded-credential and length checks, and target validation — live
in one shared preflight so later adapters (Tavily Extract, Exa Contents,
Firecrawl Scrape) cannot drift.

`ListWebFetchProviders` takes no arguments and returns the default local
route (`provider_id: null`, `kind=local_http`, modes `auto`/`text`/`raw`)
plus each callable remote ID with its `kind` and `extract_modes`, remote IDs
sorted after the local entry. Discovery mirrors the execution gating: every
configured registry ID names a route (WebFetch has no selector words), and
remote routes the shared preflight would always reject — custom
`allowed_hosts`/`denied_hosts` rules or a non-default redirect budget —
are not listed as callable. Only remote IDs can be passed to
`WebFetch(provider=...)`; the local route is selected by omitting
`provider`. It shares WebFetch's tool permission and uses local
configuration only; it sends no requests and never returns endpoints or
credentials. With `web.fetch.enabled=false` it returns `{"providers":[]}`.

Responses carry `provider_id` (null for local, the configured ID for
remote) and `status_source` (`target_http` or `provider_api`) so callers
can distinguish the two status domains without string-matching `source`.
Remote output uses the same untrusted external-content wrapper as local
Fetch.

## Tavily Extract adapter

`kind=tavily` providers are callable through the shared contract for
`auto` and `text` extraction, reusing the same configured ID, base URL,
and API key as Tavily Search; no second provider entry is introduced and
Search behavior is unchanged.

- One WebFetch call performs exactly one `POST {base_url}/extract` with
  `Authorization: Bearer <api_key>` and the body
  `{"urls": ["<url>"], "format": "markdown" | "text"}` (`auto` maps to
  `markdown`, `text` maps to `text`). No `query`, chunking, depth, or
  media options are sent, and `raw` is rejected before anything leaves the
  process.
- Tavily may report per-URL failures inside an HTTP 200 response, so
  `failed_results` is checked first: an entry matching the requested URL
  fails the request. Success requires a matching `results` entry with
  non-empty `raw_content` (the first match is used); HTTP 200 with no
  usable entry, empty content, or only foreign URLs is a provider error,
  never a fallback to another route.
- Provider discovery validates the base URL shape for both `deepseek` and
  `tavily`, so a malformed `base_url` is not listed as a callable route.
- Successful responses use `source=tavily_remote`,
  `status_source=provider_api`, `content_type` `text/markdown` or
  `text/plain` per the requested format, and `final_url=null` because the
  result URL is not a verified redirect target. `status` is the Tavily API
  HTTP status, not the page's own status.
- Errors map to `provider_error` with `retryable` only for rate limiting
  (HTTP 429/408) and upstream 5xx; authentication and content failures are
  not retryable. Shared limits (`max_chars`, `max_response_bytes`), the
  untrusted wrapper, and credential redaction follow the shared contract.
- `ListWebFetchProviders` lists a Tavily ID with `auto`/`text` only when
  its API key is configured.

## Exa Contents adapter

`kind=exa` reuses the configured Exa Search provider ID, base URL, and API
key without changing Search. Discovery offers `auto` only when the Exa
endpoint and key are usable and the shared fetch policy allows remote
extraction. `text` and `raw` fail without calling Exa.

- One WebFetch call sends one `POST {base_url}/contents` with `x-api-key`
  and the body `{"ids":["<url>"],"text":true}`. Exa chooses its default
  cache and crawl strategy; Holon does not request a live refresh or
  guarantee freshness.
- HTTP success is not enough: require a single successful status for the
  requested URL and exactly one corresponding result with non-blank `text`.
  An unsuccessful status, missing text, or unexpected result fails the
  call without fallback. Provider error details are not returned.
- Successful receipts use `source=exa_remote`, `status_source=provider_api`,
  `content_type=text/plain`, and `final_url=null`. The content is Exa's
  extraction, not Holon's plain text conversion or original HTML. A page
  URL reported by Exa does not verify a redirect. Shared size limits,
  credential redaction, and the untrusted-content wrapper apply.

## Firecrawl Scrape adapter

`kind=firecrawl` providers are callable through the shared contract for
`auto` extraction only, reusing the same configured ID, base URL, and API
key as Firecrawl Search; no second provider entry is introduced and Search
behavior (including keyless free-tier requests) is unchanged.

- One WebFetch call performs exactly one `POST /v2/scrape` with
  `Authorization: Bearer <api_key>` and the body `{"url": "<url>",
  "formats": ["markdown"], "onlyMainContent": true}`. No crawl, browser
  interaction, custom header, or auxiliary-fetch options are sent, and
  `text`/`raw` are rejected before anything leaves the process. A trailing
  `/v1` or `/v2` version segment in the configured base URL is normalized
  away before appending `/v2/scrape`, so an official base containing `/v2`
  never ends up doubled.
- Firecrawl reports page failures inside HTTP 200 responses, so success
  requires `success=true`, no non-empty page-level `metadata.error`, and
  non-empty `data.markdown`. `success=false`, a missing or blank body, or a
  reported page failure is a provider error regardless of the API 2xx
  status; there is never a fallback to another route.
- Successful responses use `source=firecrawl_remote`,
  `status_source=provider_api`, `content_type` `text/markdown`, and
  `final_url` from the provider-reported post-redirect `metadata.url` when
  it parses as an absolute URL (otherwise `final_url=null`). `status` is
  the Firecrawl API HTTP status, never the page metadata `statusCode`.
- Errors map to `provider_error` with `retryable` only for rate limiting
  (HTTP 429/408) and upstream 5xx; authentication and content failures are
  not retryable. Shared limits (`max_chars`, `max_response_bytes`), the
  untrusted wrapper, and credential redaction follow the shared contract.
- `ListWebFetchProviders` lists a Firecrawl ID with `auto` only when its
  API key is configured; the keyless free tier that Search allows does not
  extend to Scrape.
