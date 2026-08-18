# 110: Per-Agent projection snapshot pins one committed boundary

## Decision

`GET /api/agents/{agent_id}/projection-snapshot` (S5 of the observer-sync
RFC) assembles one Agent's canonical projection anchors — membership,
committed AgentState, per-Agent event window, current WorkItem anchor,
conversation anchors (latest message, latest transcript entry), latest
Brief, and hydration references — from one deferred SQLite read
transaction on the runtime database, together with the runtime identity
metadata used to derive `visibility_scope_id`. The HTTP handler only
authorizes, gates on the durable `agents.projection-snapshot.v1`
verification, enforces the first-version hard limits (1 MiB serialized,
10 s assembly budget), and serializes.

`snapshot_through_seq` equals the per-Agent committed event head of that
same view. This is sound because every display-affecting event family
commits its canonical record no later than its event: Briefs are atomic
with their `brief_created` event (S3), WorkItem and message transitions
carry record and events in one DB transition, and AgentState persists
before its `agent_state_changed` event. A committed view that contains an
event therefore contains the record it describes, so replaying only
`event_seq > snapshot_through_seq` loses nothing. The contract keeps
`event_head_seq` a separate field so a future assembly may pin the
boundary below the head without a wire change.

## Preserved boundaries and tradeoffs

- Non-members answer with one constant not-found shape: unknown, private
  child, and deleted identities are indistinguishable, and the error body
  carries no runtime, epoch, or scope facts.
- The current WorkItem anchor prefers the focused row and falls back to
  the most recently updated open WorkItem; both orders break ties
  deterministically on `work_item_id`. A stored NULL `plan_status` maps
  to the draft baseline because the wire anchor is non-optional.
- `hydration_tombstones` is empty in v1: no durable per-record deletion
  ledger exists yet for Message, Brief, or TranscriptEntry. Absence is
  represented by the null anchors; when a deletion ledger lands, the
  field carries those records without a contract change.
- The snapshot marks raw timeline history at or before the boundary as
  outside the incremental baseline; conversation history pages never
  move the watermark.
- The durable `projection_snapshot_verified` check re-runs on open:
  unreadable anchors degrade the capability to unadvertised instead of
  failing startup, independently of the sibling capabilities.
