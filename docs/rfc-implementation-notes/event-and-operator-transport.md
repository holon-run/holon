# Event and Operator Transport Implementation Notes

Related handles:

- `rfc-event-stream-interface`
- `rfc-remote-operator-transport-and-delivery`
- `rfc-operator-wait-and-intervention`

## Current implementation posture

The runtime has internal event projection and operator notification surfaces.
These already preserve a distinction between runtime posture, closure outcome,
task lifecycle, and operator-facing delivery.

The main unfinished area is not local event production; it is the stable remote
operator contract:

- which events are replayable;
- which events are only live notifications;
- how origin/trust/priority are represented over transport;
- how delivery failure, retries, and duplicate suppression are surfaced;
- how operator intervention attaches to an existing work item or waiting
  posture.

The v0.14 replay/projection minimum is:

- `/events` is a recent replay projection surface, not the raw append-only log
  as a public API.
- replay preserves safe provenance outside the event payload: cursor/id,
  sequence, timestamp, event kind, agent id, origin, trust, authority class,
  delivery surface, admission context, transport/source, reply route, message
  id, task id, work item id, correlation id, and causation id when available.
- operator replay is the default projection and only includes raw payloads for
  explicitly allowlisted event kinds.
- local first-party debug replay may include raw payloads only after control
  authorization.
- cursor expiry remains a deterministic `/state` refresh path rather than a
  client-side history reconstruction guess.

## Open gaps

1. Define the remote operator delivery envelope before treating a transport as
   authoritative.
2. Preserve provenance across operator input, external channel input, and
   runtime-generated notifications.
3. Keep closure result delivery separate from internal traces and task output.
4. Add verification that replayed remote events cannot elevate trust or
   overwrite operator-origin input provenance.
