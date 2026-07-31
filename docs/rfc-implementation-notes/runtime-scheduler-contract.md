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

## Protocol transition layer

An additive `QueueTransitionCommand` protocol layer wraps every scheduler
boundary in an atomic SQLite transaction. The canonical protocol is the
production execution path for message admission, wait resume, settlement
recovery, delivery disposition, operator interjection, and work-queue idle
ticks. A public `SchedulerDiagnosticAuditEvent` stream is emitted for
observability. See
[scheduler spec](../website/spec/scheduler.md) and
[implementation decision 098](../implementation-decisions/098-scheduler-protocol-transition-wraps-legacy-boundaries-atomically.md).

Canonical scheduler snapshots and ordinary queue/protocol transactions no
longer load rollout manifests, preflights, per-scenario authority, expectations,
or hard blockers. Migration 40 records that those tables are retired
compatibility data without dropping published history. Existing databases with
authoritative rows and missing or stale evidence can therefore reopen canonical
partitions without manual SQL.

`holon debug scheduler-recovery` reports retired rollout row counts and stale
authoritative rows separately from typed canonical recovery candidates. The
default command is read-only. `--apply` only executes canonical reducer-backed
repairs; it does not rewrite rollout metadata and creates an integrity-checked
SQLite backup before any mutation. The control-plane scheduler repair endpoint
uses the same backup rule for non-dry-run operations.

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
  `Processed`, `Aborted`, `Interjected`, and `Dropped` messages.
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
- `src/runtime/runtime_db/transitions.rs` (protocol transition atomics)
- `src/runtime_db/tests.rs` (retired rollout compatibility, concurrent load,
  and restart)
- `src/runtime/turn/execution.rs` (operator interjection safe-point handling)
- `src/storage/mod.rs` (FIFO WorkItem queue projection)
- `tests/fixtures/scheduler/`
- `tests/scheduler_workitem_mvp.rs` (canonical protocol invariants)
- `tests/scheduler_intent_mvp.rs` (offline semantic experiment coverage)

Useful local checks:

```bash
cargo test scheduler --quiet
cargo test mismatched_timer_trigger_stays_liveness_only --quiet
cargo test queued_system_tick_explicit_idempotency_key_wins_over_newer_signals --quiet
cargo test operator_interjection_prompt_is_interjected_before_next_provider_round --quiet
cargo test scheduling_advisory --quiet
cargo test scheduler_diagnostic_audit_event --quiet
cargo test scheduler_authoritative_scenarios_require_matched_evidence_and_restart_safe_rollback --quiet
cargo test scheduler_authoritative_queue_commits_survive_sustained_concurrent_load_and_restart --quiet
cargo test retired_rollout_metadata_does_not_block_canonical_snapshot_reopen --quiet
cargo test scheduler_repair_dry_run_and_apply_cancel_agent_wait --quiet
cargo test storage_work_queue_prompt_projection_preserves_fifo_fairness_and_limit --quiet
cargo test --test scheduler_workitem_mvp --test scheduler_intent_mvp --quiet
```

## Remaining follow-up

1. Keep `ContinuationResolution` as trigger classification and
   `decide_next_action` as the final model-reentry decision boundary.
2. Keep operator-interjection classification in scheduler code, while preserving
   turn-loop safe-point injection until provider/tool loop ownership changes.
3. Track bootstrap, control, and shutdown posture ownership through
   [Agent Lifecycle Control Posture](../rfcs/agent-lifecycle-control-posture.md).
   The scheduler RFC owns runnable-agent next-action decisions; lifecycle
   control owns whether the agent is runnable at all.
4. Treat recent-ledger duplicate scans as fallback evidence. Explicit
   idempotency keys should remain the primary duplicate contract.
