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

The service returns Markdown only, so remote Fetch accepts `extract_mode=auto`
and rejects `text` or `raw`. Holon bounds the API response bytes, applies its
character limit, hashes the complete returned Markdown before character
truncation, and wraps output as untrusted external content.

## Provenance

Remote responses set `source=deepseek_remote`; `status` is the provider API's
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
extraction (`remote_fetch_modes`; currently `deepseek` with `auto` only).
Unknown IDs, search-only kinds, and unsupported modes fail explicitly with
zero provider calls; there is no alias, aggregation, or fallback between
routes, and the local route keeps no `local` selector. Adapter
prerequisites — the enabled gate, site-policy restrictions, public URL
shape, embedded-credential and length checks, and target validation — live
in one shared preflight so later adapters (Tavily Extract, Exa Contents,
Firecrawl Scrape) cannot drift.

`ListWebFetchProviders` takes no arguments and returns the default local
route (empty `provider_id`, `kind=local_http`, modes `auto`/`text`/`raw`)
plus each callable remote ID with its `kind` and `extract_modes`, remote IDs
sorted after the local entry. It shares WebFetch's tool permission and uses
local configuration only; it sends no requests and never returns endpoints
or credentials. With `web.fetch.enabled=false` it returns
`{"providers":[]}`.

Responses carry `provider_id` (null for local, the configured ID for
remote) and `status_source` (`target_http` or `provider_api`) so callers
can distinguish the two status domains without string-matching `source`.
Remote output uses the same untrusted external-content wrapper as local
Fetch.
