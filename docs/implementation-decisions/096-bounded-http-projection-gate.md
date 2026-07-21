# 096 Bounded HTTP Projection Gate

Expensive first-party bootstrap reads use one shared `ProjectionGate` after
remote-access authorization.

The gate coalesces concurrent requests by projection key, caches the serialized
JSON bytes for 250 milliseconds, and allows at most four leader builds across
`/agents/list` and `/agents/:id/state`. Requests for an existing in-flight key
join that flight before capacity is checked. A new key with no leader capacity
receives `429 Too Many Requests`, `Retry-After: 1`, and the retryable
`projection_busy` error code.

The cache stores uncompressed response bytes so waiters and short-TTL hits
receive the exact leader payload while the existing HTTP compression layer
remains authoritative. A synchronous drop guard removes cancelled flights,
notifies waiters, and releases the leader permit, avoiding request-lifecycle
leaks without holding a lock across an await.

The Web GUI treats `projection_busy` as a best-effort refresh miss: it preserves
the current projection and lets the existing event or scheduled refresh path
retry. Authorization remains outside the gate so rejected requests cannot
consume capacity or observe cached projection results.
