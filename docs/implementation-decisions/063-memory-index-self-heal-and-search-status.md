# Memory index self-heal and search status contract

Issue #2895. Decision: dirty markers and missing full-backfill checkpoints are
healed by the existing full-rebuild path through durable rebuild intents, and
search status becomes explicitly multi-agent instead of describing the wrong
object.

## Choice

- The daemon indexer's work set now includes agents found by self-heal
  discovery: registered agents, agents with outbox watermarks, and agents
  known to the shared index, filtered to those with a dirty marker or missing
  backfill checkpoints. For each it enqueues an idempotent rebuild intent and
  lets `consume_rebuild_intents` run one rebuild per pass.
- `rebuild()` acknowledges the monotonic produced watermark, not the drained
  outbox's remaining-row maximum; before this, a rebuild after the outbox was
  drained regressed the applied cursor to 0 and reported permanent lag.
- Incremental consume alone never writes backfill checkpoints. Fabricating
  them from an empty queue would claim a full historical scan that never
  happened.
- `index_status.stale_reasons[]` names each concrete reason
  (`index_missing`, `dirty_marker`, `backfill_incomplete`, `outbox_lag`,
  `pending_sources`, `stale_projection`, `consume_error`); every reason is
  background-healed and diagnostic only.
- Multi-agent search returns `index_status_by_agent` for exactly the queried
  agents plus an aggregate `index_status` (worst freshness, unioned reasons,
  summed pending, maxed sequence fields). Single-agent searches keep the old
  compact shape.

## Reason

Dirty markers and checkpoints previously had no consumer: the daemon only
enumerated agents with pending rows, so agents whose only debt was a marker or
missing checkpoints stayed `freshness=stale` forever while searches were
actually complete. Reusing the rebuild path keeps one proven state transition
(rebuild writes all checkpoints and clears the marker atomically) instead of
adding a second, weaker proof.

## Boundary

Dirty markers for agents absent from every candidate set (registry,
watermarks, index tables) cannot be mapped back losslessly from their
munged filename and are left to explicit `holon memory-index rebuild`.
