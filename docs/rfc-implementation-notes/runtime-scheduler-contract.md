# Runtime Scheduler Contract Implementation Notes

Related handle:

- `rfc-runtime-scheduler-contract`

## Current implementation posture

The scheduler contract is now implemented as an explicit projection and decision
boundary for normal runtime scheduling. `SchedulerProjection` derives scheduler
facts from durable storage and agent state, `decide_next_action` produces the
shared decision vocabulary, and `SchedulerDecisionExecutor` owns the normal
run-loop message decision before queue mutation.

The implementation is still intentionally incremental: control, bootstrap, and
shutdown remain explicit posture authorities, and the provider turn loop still
owns safe-point operator interjection. These are preserved boundaries, not open
scheduler blockers.

## Landed contract anchors

- Normal queued-message processing records a `scheduler_decision` before the
  message is dequeued and moved into a run.
- Active tasks are ledger-derived from latest task records; the separate
  `active_task_ids` cache has been retired.
- Task lifecycle writes use `TaskTransition` on the main command, child-agent,
  worktree, task-status, and task-result paths.
- Work queue ticks use revision-based idempotency keys such as
  `work_queue:queued_available:<work_item_id>:<revision>`.
- Explicit idempotency keys are checked before fallback recent-ledger duplicate
  heuristics.
- Queue recovery replays `Queued` and `Dequeued` messages, but excludes
  `Processed`, `Interrupted`, `Interjected`, and `Dropped` messages.
- Prior tool executions remain ledger evidence and are not automatically replayed
  as scheduler-owned tool calls.
- Compaction records, briefs, and checkpoint artifacts do not become scheduler
  truth.

## Verification anchors

Focused verification currently lives in:

- `src/runtime/tests/scheduler.rs`
- `src/runtime/tests/task_recovery.rs`
- `src/runtime/tests/runtime_state.rs`
- `src/runtime/tests/wake_hints.rs`
- `src/runtime/continuation.rs`
- `src/runtime/memory_refresh.rs`
- `src/runtime/task_state_reducer.rs`
- `tests/fixtures/scheduler/`

Useful local checks:

```bash
cargo test scheduler --quiet
cargo test mismatched_timer_trigger_stays_liveness_only --quiet
cargo test queued_system_tick_explicit_idempotency_key_wins_over_newer_signals --quiet
cargo test interrupt_operator_prompt_is_interjected_before_next_provider_round --quiet
```

## Remaining follow-up

1. Move more continuation visibility detail into scheduler input if the
   `build_message_dispatch_plan -> decide_next_action` split becomes hard to
   reason about.
2. Keep interrupt-priority classification in scheduler code, while preserving
   turn-loop safe-point injection until provider/tool loop ownership changes.
3. Keep bootstrap, control, and shutdown as explicit posture authorities unless a
   later RFC moves those control-plane writes behind the scheduler executor.
4. Treat recent-ledger duplicate scans as fallback evidence. Explicit
   idempotency keys should remain the primary duplicate contract.
