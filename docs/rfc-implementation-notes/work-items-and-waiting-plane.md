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

## v0.14 lifecycle contract

Work-item-scoped external triggers are owned by the current work item. The
runtime rejects creation when there is no current work item, binds the waiting
intent to that work item, and revokes the trigger when the work item completes,
when focus switches to a different work item, or when a new work-item-scoped
trigger replaces the old waiting condition for the same work item.

Agent-scoped external triggers are owned by the agent lifecycle. They survive
ordinary work-item switching, completed-work cleanup, and the absence of a
current work item. They are appropriate for durable integration entry points
such as AgentInbox wake hints.

Both scopes preserve external-trigger provenance. A callback capability may
reactivate or enqueue integration input, but its payload remains external
integration input rather than operator authority. `wake_hint` records activation
context and may wake the agent; `enqueue_message` appends a callback-origin
message. The two modes must not be inferred from the payload body.

## Remaining gaps

1. Keep objective changes explicit through work-item objective/plan updates
   rather than accumulating unrelated objectives under one item.
2. Make blocked, queued, open, completed, and current-focus states easy to
   distinguish in runtime projection.
3. Add verification for reactivation edge cases: queued continuation, external
   trigger callback, operator interruption, and completed-work re-entry.

The lifecycle rules above are tracked by #914.

## Verification direction

Useful tests should prove that:

- external callbacks preserve source and trust classification;
- completion closes the right objective and does not leave stale triggers;
- a continuation can reactivate a work item without silently changing its
  objective;
- waiting posture is not confused with command-task lifecycle status.
