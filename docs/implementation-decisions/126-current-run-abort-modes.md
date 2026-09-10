# Current-run abort modes: turn-scoped vs lifecycle stop

## Decision

`POST /api/control/agents/{agent_id}/current-run/abort` supports two modes:
`stop_after_abort` (default, existing behavior) cancels the current run and
applies the stop projection, leaving the agent `Stopped` until an explicit
lifecycle start. `idle_after_abort` cancels the same run token but applies the
idle projection instead, so the agent stays awake (or falls naturally asleep)
and immediately accepts the next operator prompt.

## Why

- The Web GUI chat composer exposes a turn-scoped "stop this turn" action that
  must not degrade into a lifecycle stop: after stopping a misbehaving turn,
  the operator expects to type and send the next prompt without discovering a
  hidden "start the agent again" step. Before this split, the only abort mode
  left every agent `Stopped` and the next control prompt failed with
  `409 agent is stopped; start first`.
- Lifecycle stop keeps its stronger side effects (interrupting active tasks,
  releasing workspace occupancy, revoking external triggers) exclusive to
  `ControlAction::Stop`; `idle_after_abort` performs no lifecycle cleanup, it
  only interrupts the in-flight turn and settles its queue entry as
  `Interrupted`.
- Keeping `stop_after_abort` as the default preserves existing CLI and API
  callers that treat abort as "stop everything now".

## Preserved boundary / tradeoff

`idle_after_abort` only makes sense while a run is active; stale run ids and
absent runs still surface the same 409 conflict responses. Queued follow-up
messages may still be processed after a turn-scoped abort because the agent
remains schedulable.
