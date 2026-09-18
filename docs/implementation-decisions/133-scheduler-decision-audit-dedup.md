# Scheduler decision audit dedup

## Choice

`append_scheduler_decision` suppresses a repeated decision by comparing a
stable signature (decision, reason, boundary, message/work-item/task
identity, `model_reentry`/`liveness_only` flags) against same-kind events in
the recent 32-event window, instead of only the latest same-kind event.
Volatile evidence is excluded from the signature. Suppression is broken when
a `model_reentry` decision was recorded after the most recent matching
occurrence.

## Reason

Idle run loops alternate boundaries (`run_loop_idle` / `idle_tick`) with the
same wait decision, so latest-only comparison never matched and both
variants were written every tick, dominating idle write amplification on a
27.8 GB production copy (PR #3091 A/B). The `model_reentry` break preserves
the audit-tail invariant: a genuine work → idle revert is re-recorded, so
the latest recorded decision still mirrors the current posture. Scheduling
behavior is unaffected; callers ignore the returned bool.

## Preserved boundary

Consumers of the diagnostic stream may rely on the latest recorded decision
mirroring the current posture, not on seeing every tick. Window span varies
with runtime activity; past the window a decision is always appended.
