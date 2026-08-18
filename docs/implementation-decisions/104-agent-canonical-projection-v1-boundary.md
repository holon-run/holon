# AgentCanonicalProjection v1 field boundary

Source: `docs/rfcs/observer-sync-agent-summary-and-read-markers.md` (S0
contract slice). `AgentCanonicalProjection` is the payload of the per-Agent
projection snapshot and is anchored to one `snapshot_through_seq` consistency
boundary.

v1 carries compact current state only:

- the `GET /api/agents/list` entry shape (lifecycle, posture, waiting, model);
- a current WorkItem anchor (`id`, `state`, `plan_status`, canonical
  `revision`, `updated_at`);
- conversation revision anchors (`latest_message_id`,
  `latest_transcript_entry_id`) for record families that have no per-record
  revision counter;
- latest Brief identity with bounded preview (512 UTF-8 bytes) and immutable
  `created_event_seq` linkage;
- hydration tombstones and hydration references keyed by
  `(record_kind, record_id)`.

v1 explicitly excludes verbose timelines, full transcript/message history, and
full Brief text. Those remain behind the retained event pages and the batch
record APIs. This keeps the snapshot a bootstrap/recovery baseline for the
compact projection instead of a second conversation API; unbounded history and
anchor pagination stay a separate future contract.

OpenAPI keeps the embedded agent entry as a conservative baseline object
(`AgentListEntry`) until the agents/list DTO stabilizes under a named schema;
the Rust DTO reuses the concrete `AgentListEntry` type so fixtures round-trip
the real wire shape.
