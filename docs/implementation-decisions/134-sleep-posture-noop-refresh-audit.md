# Sleep posture no-op refresh audit suppression

## Choice

Sleep-boundary transitions (`SchedulerDecisionExecutor::transition_to_sleep`
and `transition_run_loop_idle_to_sleep`) return a `SleepTransition` carrying
`posture_changed`. When the agent was already asleep, they refresh the
durable `sleeping_until` when it moves, but do not re-record
`scheduler_posture_decision`, and callers skip the paired
`agent_state_changed` event. Genuine posture transitions still record both
audits and always persist.

## Reason

After decision-audit dedup (record 133), the remaining idle write
amplification on the 27.8 GB production copy was the
posture/state-changed pair emitted on every run-loop idle poll while the
agent stayed asleep (~12 events/s, ~0.5-0.7 MB/s WAL, ~34 KB per event),
re-poisoning read latency within ~2 minutes of a checkpoint. A
deadline-only refresh is not a posture transition; recording it on every
poll contradicted the audit tail's purpose of mirroring logical posture.

## Preserved boundary

Consumers may rely on `scheduler_posture_decision` / `agent_state_changed`
marking actual posture changes, not every sleep-boundary poll. The
persisted agent row remains the source of truth for the current
`sleeping_until` deadline; projections that need deadline updates must read
state rather than infer them from these events.
