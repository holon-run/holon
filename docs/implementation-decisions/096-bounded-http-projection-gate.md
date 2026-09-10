# 096 Bounded HTTP Projection Gate

Expensive first-party bootstrap reads use one shared `ProjectionGate` after
remote-access authorization.

The gate coalesces concurrent requests by projection key, caches the serialized
JSON bytes, and bounds concurrent leader builds. Capacity defaults to 16
leaders with a 500 millisecond TTL and is configurable without recompiling via
`api.projection.max_leaders` and `api.projection.cache_ttl_ms`; the original
compile-time defaults (4 leaders / 250 ms) rejected ordinary Web GUI polling of
several agents with `429 projection_busy` storms (#2860). Requests for an
existing in-flight key join that flight before capacity is checked. A new key
with no leader capacity receives `429 Too Many Requests`, `Retry-After: 1`,
and the retryable `projection_busy` error code.

The cache stores uncompressed response bytes so waiters and short-TTL hits
receive the exact leader payload while the existing HTTP compression layer
remains authoritative. A synchronous drop guard removes cancelled flights,
notifies waiters, and releases the leader permit, avoiding request-lifecycle
leaks without holding a lock across an await.

The Web GUI treats `projection_busy` as a best-effort refresh miss: it preserves
the current projection and lets the existing event or scheduled refresh path
retry; roster and backfill retry delays never undercut the server's
`Retry-After` hint (delay is `max(computed backoff, Retry-After)`).
Authorization remains outside the gate so rejected requests cannot consume
capacity or observe cached projection results.
