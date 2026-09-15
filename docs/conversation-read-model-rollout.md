# Conversation Read Model v1 Rollout

This runbook covers the Rust conversation read surface and the independent
`@holon/conversation-sdk`. It does not migrate or modify the existing
`web-gui`, and it does not introduce a second event ledger.

## Runtime database upgrade

Runtime database migration v66, `conversation_input_assignment_repair`, fixes
legacy replay inputs whose assignment points at the replay turn instead of the
canonical source turn. The migration now scans replay provenance once, validates
that each source turn exists and contains the input, and changes only affected
assignments. Migration logs include version, name, stage, row counts, duration,
and failures.

Before upgrading a large long-lived runtime:

1. Stop `holon serve` and preserve the runtime database, WAL, and SHM files.
2. Start the new binary and watch for
   `starting runtime database migration`,
   `repairing conversation input assignments`, and
   `finished runtime database migration`.
3. Treat a provenance validation failure as a data-integrity blocker. Do not
   edit assignments or migration markers by hand; retain the bounded sample in
   the error and investigate the referenced turns.
4. After v66 commits, restart the new binary once and confirm migration is
   skipped and normal startup time returns.

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

The checked-in Phase 5 baseline is
[`docs/benchmarks/conversation-read-model-phase5.json`](benchmarks/conversation-read-model-phase5.json).
It was captured on revision `b09e000a7cafae2b71c99e763c5cf9e87bf8616e`
with 10 measured repetitions on 2026-09-14:

| Read | Median wall time | Median payload | Records | Replay events |
| --- | ---: | ---: | ---: | ---: |
| First summary page | 2.90 ms | 11,167 bytes | 31 | 0 |
| Long-turn detail | 1.44 ms | 11,965 bytes | 50 | 0 |
| Reconnect replay | 1.84 ms | 837 bytes | 2 | 1 |
| Metadata-only shadow | 3.06 ms | 499 bytes | 30 | 0 |

All four workloads used one logical repository read per sample. Treat these
timings as comparison evidence for the recorded environment, not universal
latency thresholds.

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

If the database has committed only the data-only v66 marker and the previous
v65-capable binary must be restored, first stop `holon serve` and run the
protected offline preflight:

```sh
holon debug runtime-db conversation-input-assignment-rollback --json
```

The command is dry-run by default. It is eligible only when the highest
migration is exactly v66 with the expected
`conversation_input_assignment_repair` name. It refuses databases with a newer
migration or a mismatched marker.

Apply the rollback only after reviewing the report:

```sh
holon debug runtime-db conversation-input-assignment-rollback --apply --json
```

Apply mode creates and integrity-checks a `VACUUM INTO` backup, verifies that
the backup retains v66, then atomically removes only the v66 marker and verifies
that the live database head is v65 `task_result_settlements`. The repaired
assignments remain in place because their tables and rows are already valid for
v65. Do not start a v66-capable binary against the downgraded marker before the
v65 rollback test, because it will apply v66 again.
