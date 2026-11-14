# Core Protocol And Transition Navigation Boundary

## Choice

Keep `src/types.rs` as the public import surface. The definitions and their
wire contracts live in `holon-domain`, including agent observation snapshots
and the GetAgent result. The main crate supplies the ledger-backed observation
guard, builds tool responses, and classifies provider and database errors.

`holon-system` owns host file, process, PTY, and workspace operations and uses
the shared execution types from `holon-domain`. The `holon::system` import
path re-exports the same APIs. Host unit tests run with the system crate.

Keep `src/runtime_db/transitions.rs` as the
`RuntimeTransitionRepository` commit seam. Its execution-protocol code
lives in the adjacent `src/runtime_db/transitions/` directory.

## Navigation map

Two layers sit above the same durable state. The runtime layer prepares
evidence and decides the transition; the repository layer commits it
atomically.

| Family | Repository entries | What the commit carries | Exercised by |
| --- | --- | --- | --- |
| Agent posture | `commit_agent_posture` | Agent-state mutation plus audit, called from `scheduler_executor` | `src/runtime/tests/runtime_state.rs` |
| Queue head no-progress | `commit_queue_head_no_progress` | Bounded defer or quarantine of an unprocessable queue head, called from `scheduler_executor` | `src/runtime_db/tests.rs` |
| Turn terminal | `commit_turn_terminal` | Terminal Turn, terminal tool executions, agent state, audit | `src/runtime/tests/turns.rs` |
| Work item | `commit_work_item`, `commit_work_item_focus` and their `_with_execution_protocol*` variants | Insert/update fenced by expected revision; focus, continuation, brief evidence | `src/runtime/tests/contracts/work_item.rs`, `src/runtime/tests/work_items.rs` |
| Wait | `commit_wait` and its `_with_execution_protocol*` variants | Wait conditions, timer wake claim, task-result admission, index changes | `src/runtime/tests/contracts/wait.rs`, `src/runtime/tests/timers.rs` |
| Queue | `commit_queue*`, `commit_queue_terminal*`, `commit_queue_with_wait_trigger`, `commit_queue_with_completion` | Queue mutation, message/transcript/Turn evidence, settlement, wait trigger | `src/runtime/tests/contracts/queue.rs`, `src/runtime/tests/runtime_state.rs` |
| Delivery admission | `commit_delivery_admission` | Agent message delivery admission and receipt | `src/runtime_db/agent_message_delivery.rs` |
| Task | `commit_task` and `commit_task_with_execution_protocol` | Task record, agent deletion, task-result settlement, queue, wait | `src/runtime/tests/contracts/task.rs` |
| Scheduler recovery | `commit_scheduler_recovery` | Repair/startup commit, called by `apply_scheduler_recovery_plan_with_options` in `src/runtime.rs` from scheduler-recovery CLI paths in `src/main.rs`; `src/runtime/repair.rs` is the separate scheduler-repair surface for Wait/Queue commits | `src/runtime/tests/runtime_state.rs` |
| Startup recovery | `recover_interrupted_runtime_state_at_startup` | Orphaned-claim, interrupted-turn, and execution recovery report | `src/runtime/tests/runtime_state.rs` |
| Execution protocol | `src/runtime_db/transitions/execution_protocol_repository.rs` | Prepared execution-protocol commands, authority fences, state persistence | `src/runtime_db/transitions/execution_protocol_fixture_repository.rs` (test-only) |

The suite column names entry points, not exclusive ownership: the contract
suites drive the runtime path across these repository entries, so a single
behavior can be covered in more than one file.

## Invariants

- Each repository entry commits exactly one SQLite transaction. That
  transaction combines the canonical mutation(s) (queue, WorkItem, task),
  agent state, evidence (message, transcript, Turn, audit, brief, index
  changes), and post-commit effects.
- Cross-row writes are revision-fenced: `WorkItemMutation::Update.expected_revision`,
  `WaitTransitionCommand::expected_wait_conditions`, and the expected queue
  entry inside `QueueMutation` / `WaitTaskResultAdmission`.
- Post-commit effects (`PostCommitEffects::notify_memory_index`,
  `notify_scheduler`, queued messages, fault injection) are produced only after
  a successful commit; nothing observes them from inside the transaction.
- `TransitionFaultPoint` is the test-only rollback observation seam
  (`AfterValidation`, `AfterTerminalAgentStateWrite`,
  `AfterTerminalTurnRecordWrite`, `AfterCanonicalWrites`, `AfterAuditWrites`,
  `BeforeCommit`, `BeforeCacheUpdate`, `BeforeEventPublication`, and
  `BeforeSchedulerNotification`). Production paths pass `fault: None`.
- The seam does not decide `origin`, `trust`, `priority`, scheduling, or
  user-facing brief content. It persists a transition a higher layer has
  already decided and returns `TransitionCommit { applied, effects, delivery_receipt }`.
- The runtime layer must keep preparing the same evidence before calling the
  repository; moving a decision into this seam would change admission
  semantics, not just layout.

## Reason

The shared import path lets callers and concurrent changes keep the same type
paths. The type compatibility suite runs independently with
`cargo test --manifest-path crates/holon-domain/Cargo.toml`. The runtime status inventory links to the files that define its enums.

Model route normalization uses the checked-in provider identity and model alias
tables, including preferred endpoints for legacy references. The runtime still
selects and constructs provider clients. An observation snapshot carries the
runtime's ledger handle in a serde-skipped field and forks its reservation when
cloned. The runtime retains the original handle for borrowed serialization.

Transition code continues to use the transaction and publication invariants
listed above.
