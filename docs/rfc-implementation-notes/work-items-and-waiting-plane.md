# Work Items and Waiting Plane Implementation Notes

Related handles:

- `rfc-work-item-runtime-model`
- `rfc-waiting-plane-and-reactivation`
- `rfc-continuation-trigger`
- `rfc-external-trigger-capability`
- `rfc-objective-delta-and-acceptance-boundary`

## Current implementation posture

Work items are runtime-visible durable objectives with mutable plans, todo
snapshots, completion state, blockers, and current focus. Waiting intents and
external triggers are separate from ordinary task execution, and sleep/wake
posture is visible in the agent plane.

This is a strong foundation, but the implementation is still only partial
because some workflow invariants are enforced by agent instructions rather than
runtime state transitions.

## Open gaps

1. Ensure every cross-turn wait is anchored to the correct current work item
   before the agent sleeps.
2. Cancel stale work-item scoped external triggers when the tracked condition
   changes or the work item completes.
3. Keep objective changes explicit through work-item objective/plan updates
   rather than accumulating unrelated objectives under one item.
4. Make blocked, queued, open, completed, and current-focus states easy to
   distinguish in runtime projection.
5. Add verification for reactivation edge cases: queued continuation, external
   trigger callback, operator interruption, and completed-work re-entry.

## Verification direction

Useful tests should prove that:

- external callbacks preserve source and trust classification;
- completion closes the right objective and does not leave stale triggers;
- a continuation can reactivate a work item without silently changing its
  objective;
- waiting posture is not confused with command-task lifecycle status.
