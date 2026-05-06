# Runtime Boundary Extraction Regression Matrix

Related issue: #940

This matrix is the phase-1 safety net for extracting runtime execution
boundaries without changing external behavior. It reviews existing coverage and
identifies the focused characterization tests that should guard follow-up
extraction issues.

## Scope

Phase 1 covers behavior-preserving extraction around:

- one agent turn lifecycle
- managed task orchestration
- sleep, wake, and waiting-intent transitions
- user-facing delivery and brief projection
- cross-cutting external contracts that must not change during extraction

Non-goals remain unchanged: no prompt contract rewrite, no tool schema change,
no scheduler rewrite, no benchmark harness rewrite, and no UI/TUI work.

## Matrix

| Boundary | Contract to preserve | Existing coverage | Added or required coverage |
| --- | --- | --- | --- |
| Turn execution | A queued operator message moves through queued, dequeued, and processed states; incoming message and assistant round transcript entries are recorded; ack/result briefs and terminal event are persisted. | `src/runtime/tests/turns.rs`, `tests/runtime_tasks.rs` cover provider rounds, tool rounds, failure, and transcript details. | `turn_execution_boundary_persists_queue_transcript_and_briefs` adds one compact end-to-end characterization across queue, transcript, brief, and terminal event persistence. |
| Managed task orchestration | Task tools keep lifecycle/status separate from output retrieval, command tasks persist terminal state, task result rejoin preserves runtime provenance, and task cancellation behavior is unchanged. | `tests/runtime_tasks.rs`, `tests/runtime_subagents.rs`, `tests/runtime_workspace_worktree.rs`, and `src/runtime/tests/agent_and_tools.rs`. | Existing coverage is sufficient for phase-1 extraction; follow-up #937 should run the focused task suites before and after refactor. |
| Sleep, wake, and waiting intents | Sleep-only completion does not force extra provider turns; wake hints coalesce and re-enter once; callback waiting intents preserve scope, provenance, cancellation, and mode semantics. | `src/runtime/tests/turns.rs`, `src/runtime/tests/work_items.rs`, `tests/runtime_waiting_and_reactivation.rs`, and `tests/runtime_waiting_and_delivery_regressions.rs`. | Existing coverage is sufficient for phase-1 extraction; follow-up #938 should keep the waiting and callback suites green. |
| Delivery and brief projection | Terminal result brief comes from the final assistant message, notification delivery uses the selected route, delivery failures are recorded without failing the notification, and remote operator provenance is preserved. | `tests/runtime_waiting_and_reactivation.rs`, `tests/runtime_waiting_and_delivery_regressions.rs`, and `tests/http_operator_transport.rs`. | Existing coverage is sufficient for phase-1 extraction; follow-up #939 should run delivery and operator-ingress suites. |
| Cross-cutting external contracts | Tool schemas remain stable, provider failures surface as failure briefs and transcript entries, compaction preserves exact tool context or recap, and HTTP ingress validates provenance/trust. | `tests/runtime_tasks.rs`, `tests/runtime_compaction.rs`, `tests/regression_fixture.rs`, `tests/http_ingress.rs`, `tests/http_callback.rs`, and provider live tests where enabled. | No new broad suite is needed for phase 1. Use focused local tests plus existing CI; live tests remain opt-in. |

## Focused Verification Commands

Use these commands as the minimum local gates for the extraction sequence:

```bash
cargo test -q --test runtime_waiting_and_reactivation turn_execution_boundary_persists_queue_transcript_and_briefs
cargo test -q --test runtime_tasks task_status_and_task_output_keep_lifecycle_and_output_boundaries
cargo test -q --test runtime_waiting_and_delivery_regressions callback_tools_register_and_revoke_waiting_state
cargo test -q --test runtime_waiting_and_delivery_regressions callback_wake_hint_routes_through_wake_hint
cargo test -q --test runtime_waiting_and_reactivation terminal_brief_uses_last_assistant_message_without_terminal_delivery_round
cargo fmt --check
```

Each follow-up extraction PR should run the focused command for its boundary and
the broader suite that owns the touched code. Full `cargo test` remains the
merge gate.
