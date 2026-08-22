# Event projection-effect classification (S2)

## Choice

`RuntimeEventDescriptor` declares a `projection_effect` per event family and
is the only source of truth. The classification map is: message, brief,
task, work-item, and agent-state families are `display_invalidation`;
`scheduler_diagnostic` is `none`.

Served envelopes carry the field only while `events.projection-effect.v1`
is advertised (envelope contract version 3). A stored event classifies by
wire name only when the payload schema identity matches and the payload
schema version is not newer than the registry's. Recognizable legacy
envelopes, including legacy rows whose wire name is now registry-known, are
classified conservatively as `display_invalidation`. Unknown typed kinds,
schema mismatches, and future schema versions remain unsupported.

## Reason

The referenced families invalidate display state the Web projection derives
from canonical records (conversation anchors, latest brief, task cards,
work-item anchor, agent lifecycle), so they must block projection readiness
until hydration resolves. Scheduler diagnostics are self-contained evidence
outside `AgentCanonicalProjection` v1, so marking them `none` avoids
needlessly blocking readiness for events that reference no record.

Gating emission on the durable `event_projection_effect_complete`
verification keeps the advertisement honest against databases that contain
typed-shaped events this binary cannot inventory. Completeness means every
stored event has a sound classification; it does not require legacy events to
receive the narrowest possible effect.

## Preserved boundary

The fallback classification is total but conservative; adding a new event
family requires declaring its effect in the registry and extends the
inventory test, never a per-route special case. The rich `cursor_not_found`
window is read in one SQL statement so epoch, floor, and head can never come
from different committed views.
