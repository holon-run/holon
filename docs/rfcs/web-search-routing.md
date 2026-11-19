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
