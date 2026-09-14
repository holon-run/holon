# Conversation Read Model v1 Rollout

This runbook covers the Rust conversation read surface and the independent
`@holon/conversation-sdk`. It does not migrate or modify the existing
`web-gui`, and it does not introduce a second event ledger.

## Compatibility preflight

Before calling conversation routes, fetch `/api/handshake` and require:

| Contract | Required value |
| --- | --- |
| Control protocol | `holon-control` version `1` |
| Capability | `agents.conversation-read.v1` |
| Conversation schema/query versions | `1` / `1` |

Capability absence means legacy/unavailable, not permission to probe the new
routes. Unknown protocol, schema, or query versions fail closed. Cursors and
checkpoints remain opaque.

## Canary sequence

1. Keep the existing legacy reads available as the client fallback.
2. Confirm the capability is advertised only after durable source verification.
3. Bootstrap one bounded summary page, attach the conversation stream from its
   checkpoint, and fetch detail only on demand.
4. Run a metadata-only shadow comparison for a small recent window:

   ```text
   GET /api/control/agents/{agent_id}/conversation/shadow-diagnostics?turn_limit=30
   ```

   The endpoint requires control-plane bearer authentication. A healthy report
   has `mismatch_count: 0`. `legacy_unattributed_briefs` may be non-zero for
   historical data and does not invent turn ownership.
5. Expand the canary only while shadow mismatches, reset reasons, timeouts,
   payload failures, and slow-consumer counts remain understood.

## Observability

Use the control-authenticated JSON snapshot:

```text
GET /api/control/runtime/performance
```

or the label-free OpenMetrics endpoint:

```text
GET /api/control/runtime/metrics
```

The conversation group records bounded operation count/time/bytes for summary,
activity, stream recovery, and shadow comparison. Fixed counters cover
capability absence, cursor/limit/payload failures, timeouts, slow consumers,
legacy unattributed Briefs, shadow match/mismatch results, and typed reset
reasons. No agent ID, cursor, Brief body, transcript, tool payload, or error text
is used as a metric label.

Capture a baseline after a representative bounded summary, detail request,
stream recovery, and shadow comparison. Compare operation count, maximum/p95
latency, and response bytes before expanding the canary; investigate growth
that is inconsistent with the configured page/replay limits.

The repo-local fixture produces a JSON artifact with the same four reads:

```sh
HOLON_BENCH_SAMPLES=10 cargo bench --bench conversation_read > conversation-read.json
```

Each sample records one logical repository read, wall time, serialized payload
bytes, returned record count, and reconnect event work. The benchmark asserts
the stable page/replay and hard payload boundaries, while absolute timings are
evidence for comparison rather than platform-independent pass/fail thresholds.

## Recovery expectations

- Long active turns may expose only a bounded complete inline activity page;
  detail pagination remains the source for older activity. They must not enter
  a permanent `replay_limit_exceeded` reset loop.
- A terminal summary first observed after bootstrap remains in the SDK's bounded
  live overlay until history pagination establishes normal membership.
- A typed reset discards stale coverage assumptions. Bootstrap a fresh snapshot
  before reconnecting.
- Legacy unattributed Briefs remain available through the legacy compatibility
  surface. The conversation projection never fabricates a native turn.

## Rollback

Stop selecting the conversation surface in the client and return to the
preserved legacy reads. Do not reinterpret checkpoints or copy data into a new
ledger. Server routes and diagnostics may remain deployed because they are
read-only and capability gated; investigate shadow/reset metrics before another
canary.
