# Projection gate serves bounded-stale bytes on retryable assembly failure

`ProjectionGate` caches successful projection builds for a short TTL and
coalesces concurrent requests per key, but a failed build released the key
and surfaced the error directly. Under database saturation the roster
snapshot assembly (`host.agent_roster_snapshot`) can exceed its ten second
budget, so every client retry (the 503 body is `retryable: true`) started a
fresh ten second build and failed again — a visible `503` loop while a
recently built projection existed.

Choice: keep the last successful build per key for up to sixty seconds and
serve it only when the build fails with a service-unavailable class failure
(assembly budget timeout or a cancelled leader). Non-retryable failures
(4xx contract failures, 500-class errors) still surface unchanged, and the
in-flight entry is still released on failure so the next caller rebuilds
instead of pinning the stale bytes. `holon_projection_gate_stale_served_total`
counts every fallback serve. The underlying slow assembly (issue #3078
family) is tracked separately; this bounds its user-visible blast radius.
