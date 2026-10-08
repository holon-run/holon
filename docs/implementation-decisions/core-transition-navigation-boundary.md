# Core Protocol And Transition Navigation Boundary

## Choice

Keep `src/types.rs` and `src/runtime_db/transitions.rs` as deliberately broad
shared seams, and record their navigation map here instead of splitting either
file in this change.

- `src/types.rs` stays the shared runtime type surface. It is imported
  throughout `src/` (154 files at the time of writing) and re-exports
  `crate::domain::{agent, work_item}`; both aggregates are intentional.
- `src/runtime_db/transitions.rs` stays the single
  `RuntimeTransitionRepository` commit seam. Its execution-protocol
  sub-boundary already lives next to it in `src/runtime_db/transitions/`, so
  transition code is not confined to one file.

This note is a navigation aid. It names the entry families, their invariants,
and the suites that exercise them. It does not authorize a structural change.

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
| Scheduler recovery | `commit_scheduler_recovery` | Repair/startup commit, called from `src/runtime/repair.rs` | `src/runtime/tests/runtime_state.rs` |
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
  (`AfterValidation`, `AfterCanonicalWrites`, `AfterAuditWrites`,
  `BeforeCommit`, and the post-commit points). Production paths pass
  `fault: None`.
- The seam does not decide `origin`, `trust`, `priority`, scheduling, or
  user-facing brief content. It persists a transition a higher layer has
  already decided and returns `TransitionCommit { applied, effects, delivery_receipt }`.
- The runtime layer must keep preparing the same evidence before calling the
  repository; moving a decision into this seam would change admission
  semantics, not just layout.

## Reason

`src/types.rs` is the shared vocabulary for the whole crate and re-exports the
agent and work-item domain modules. Splitting it by file size would reshape the
type surface that wire contracts and nearly every module depend on.
`src/runtime_db/transitions.rs` is the one place that makes a transition
atomic, and its `commit_*` families are already grouped by responsibility;
spreading them across files would distribute the single-transaction invariant
without reducing coupling.

The measured cost here is navigation, not correctness: the atomicity, CAS, and
post-commit rules live in one place and are covered by the contract suites
above. A navigation map removes that cost without risking the wire contract or
the commit semantics.

## Preserved boundary and stop conditions

- No file split, module rename, or public API change accompanies this note.
- A future structural split needs a dependency and change-cooccurrence matrix
  that shows real coupling, plus a verification plan for transaction atomicity,
  rollback, and failure visibility.
- If that matrix does not show a benefit larger than the coupling and
  regression risk, keep the current shape and extend this note instead.
