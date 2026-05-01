# Holon Core Test Coverage Review

Snapshot date: 2026-04-20

## Scope

This review focuses on behavioral coverage of the current Holon runtime rather
than line-level coverage percentages.

Method used:

- inspect `cargo test -- --list`
- inspect the major integration test files
- inspect runtime/core modules for explicit local tests
- identify runtime contracts that are only indirectly covered today

## Current Coverage Snapshot

Current automated test surface is broad:

- 458 test functions total across unit, integration, worktree, HTTP, TUI, and
  live-provider suites
- heaviest coverage files:
  - `tests/runtime_flow.rs`: 58 tests
  - `tests/http_routes.rs`: 49 tests
  - `src/provider/tests.rs`: 57 tests
  - `src/runtime.rs`: 45 tests
  - `src/tui.rs` + `src/tui/projection.rs` + `src/tui_markdown.rs`: 46 tests
  - `tests/run_once.rs`: 20 tests

This means the repo is not test-light. The real issue is that coverage is
uneven: many high-value runtime contracts are exercised only through large
integration flows, while several core decision modules still have no direct
contract tests.

## Coverage Assessment By Subsystem

### Strong Coverage

These areas already have meaningful behavioral coverage and are not the first
place to spend more test effort unless regressions appear:

- provider request/response shaping, fallback, retry, and auth diagnostics
  - `src/provider/tests.rs`
- end-to-end runtime flows for:
  - task rejoin
  - background command tasks
  - subagent tasks
  - worktree task flows
  - failure briefs
  - wake hints
  - token usage persistence
  - `tests/runtime_flow.rs`
- HTTP control and status API behavior
  - `tests/http_routes.rs`
- `run_once` lifecycle and wait/no-wait semantics
  - `tests/run_once.rs`
- TUI projection/render behavior on top of runtime events
  - `src/tui.rs`
  - `src/tui/projection.rs`
  - `src/tui_markdown.rs`
- worktree lifecycle integration and task-owned cleanup flows
  - `tests/wt201_multiple_worktree_tasks.rs`
  - `tests/wt202_worktree_task_summary.rs`
  - `tests/wt203_task_owned_worktree_cleanup.rs`
  - `tests/wt204_parallel_worktree_workflow.rs`
  - `tests/wt205_worktree_lifecycle_edge_cases.rs`
- daemon and host restart/control behavior
  - `src/daemon/tests.rs`
  - `src/host.rs`

### Medium Coverage

These areas have useful tests, but the current coverage is still mostly
happy-path or shallow:

- context building and basic compaction threshold behavior
  - `src/context.rs`
- storage snapshot/recovery projections
  - `src/storage.rs`
- workspace path resolution and host-local policy helpers
  - `src/system/workspace.rs`
  - `src/system/host_local_policy.rs`
- tool schema exposure and patch tool behavior
  - `src/tool/dispatch.rs`
  - `src/tool/apply_patch.rs`

### Weak Or Indirect Coverage

These are the highest-value gaps.

Several runtime-core files currently have zero explicit local tests:

- `src/runtime/closure.rs`
- `src/runtime/command_task.rs`
- `src/runtime/failure.rs`
- `src/runtime/operator_dispatch.rs`
- `src/runtime/lifecycle.rs`
- `src/runtime/message_dispatch.rs`
- `src/runtime/subagent.rs`
- `src/runtime/task_state_reducer.rs`
- `src/runtime/tasks.rs`
- `src/runtime/turn.rs`
- `src/runtime/worktree.rs`

That does not mean they are uncovered. Many are exercised indirectly through
`runtime_flow` or `runtime.rs` tests. But it does mean these contracts are not
isolated well enough yet:

- closure decision precedence
- message-dispatch branching by message kind and model visibility
- stale task-update suppression rules
- tool-loop edge conditions inside `turn.rs`
- internal command-task persistence/recovery behavior
- worktree helper error paths

## Core Risk Areas

For current project priorities, the most important thing is not “more tests
everywhere”. It is protecting the contracts that make the runtime stable while
`#231` settles and before `#221` introduces another memory-layer change.

The highest-risk current gaps are:

1. Runtime state-machine contracts are not tested close enough to the code that
   implements them.
2. Event-stream correctness is tested for basic replay/bootstrap cases, but not
   yet as a full “native stream stability” matrix.
3. Recovery ordering across timers and blocking tasks is only partially
   covered; wake-hint and work-item activation now have direct local coverage in
   `src/runtime/memory_refresh.rs`.
4. Several internal helpers that determine whether the runtime waits, resumes,
   re-enters, or ignores stale updates are protected only by indirect tests.

## Proposed Test Requirements

The recommended order below is designed for stability work before the next
benchmark round.

## Wave 1: Runtime Contract Tests

These should be the next test wave.

### RUNTIME-001 Closure Decision Matrix

Add direct unit tests for `src/runtime/closure.rs`.

Required cases:

- runtime error overrides every waiting condition
- awaiting operator input wins over tasks/timers/external wait
- blocking tasks map to `awaiting_task_result`
- waiting intents map to `awaiting_external_change`
- timers map to `awaiting_timer`
- sleeping posture is preserved independently from closure outcome
- aborted turn maps to failed closure
- completed turn with no waiting conditions maps to completed closure
- started turn without terminal record maps to waiting
- `runtime_error_active` clears when a later success brief exists

### RUNTIME-002 Message Dispatch Matrix

Add direct tests for `src/runtime/message_dispatch.rs`.

Required cases:

- non-model-visible external/system events do not run an interactive turn
- model-visible operator/timer/task rejoin events do run an interactive turn
- `TaskStatus` routes only through task-state reduction
- `TaskResult` routes through reduction plus correct follow-up behavior
- unknown control action fails without mutating runtime state
- paused/stopped/asleep agents do not get their status overwritten by the final
  post-dispatch status rewrite
- incoming transcript entries preserve `delivery_surface`,
  `admission_context`, `correlation_id`, and `causation_id`

### RUNTIME-003 Task State Reducer Contract

Add direct tests for `src/runtime/task_state_reducer.rs`.

Required cases:

- stale non-terminal updates are ignored after terminal status exists
- terminal results remove the task from `active_task_ids`
- non-terminal task updates add missing active task ids
- blocking task updates move runtime to `AwaitingTask`
- terminal result falls back to `AwakeIdle` only when no blocking tasks remain
- non-model-visible task results emit a result brief rather than reopening a
  full interactive turn

### RUNTIME-004 Idle Tick And Work Queue Contract

Status: satisfied by direct tests in `src/runtime/memory_refresh.rs`.

Covered cases:

- queue-nonempty state suppresses idle system-tick emission
- pending wake hint takes precedence over active work item
- active work item takes precedence over queued work item activation
- queued work item activation is skipped when an active item already exists
- queued work item activation writes the expected audit event and active record
- wake-hint system tick preserves structured body/content type/correlation
- wake-hint state is cleared only if the same hint is still pending

### RUNTIME-005 Interactive Turn Setup Contract

Add direct tests for `src/runtime/operator_dispatch.rs`.

Required cases:

- prompt building runs compaction before prompt assembly
- tool visibility is derived from agent profile presets and capability families,
  not per-message trusted/untrusted labels
- stable tool specs exclude removed public tools and aliases such as
  `CreateTask`, `WorktreeTaskDiscard`, and `GetAgentState`
- private-child and public-named profile presets expose their expected
  capability-family subsets
- aborted agent loop emits failure brief
- completed agent loop emits result brief
- `Sleep` transitions runtime to sleeping after the turn only, preserving any
  requested duration

### RUNTIME-006 Tool Loop Edge Cases

Add direct tests for `src/runtime/turn.rs`.

Required cases:

- max tool round depth persists aborted terminal state and clear final text
- sleep-only tool round ends cleanly without forcing an extra provider turn
- text-only rounds append transcript/audit observations correctly
- max-output-recovery exhaust path fails deterministically after the retry cap
- disallowed tool calls stay auditable and do not corrupt conversation state
- last assistant message aggregation stays stable across text/tool/text rounds
- first provider round records prompt-cache identity fields for
  `working_memory_revision` and `compression_epoch`

## Wave 2: Stream And Recovery Stability

This wave protects `#231` stabilization work.

### STREAM-001 Bootstrap/Replay Equivalence

Add end-to-end tests proving that:

- `/state` bootstrap plus subsequent `/events` replay yields the same effective
  projection as a clean stream from the beginning
- transcript/task/work-item/session projections match after replay

### STREAM-002 Cursor And Reconnect Contract

Add tests for:

- monotonic event ids/cursors across mixed event families
- reconnect via `Last-Event-ID` without gaps or duplicate semantic application
- refresh-hint behavior after an expired/missing cursor
- reconnect after daemon restart with old cursor behavior remaining explicit and
  consistent

### STREAM-003 Multi-Agent Isolation On Stream Surfaces

Add tests proving:

- one agent’s events do not leak into another agent’s stream
- multi-agent daemon state stays isolated across snapshot and stream surfaces
- TUI/client-side projection remains agent-scoped when the stream is multiplexed

### STREAM-004 Burst Stability For TUI Consumption

Add tests for:

- long event bursts containing transcript, task, work item, and brief updates
- projection parity after reconnect mid-burst
- idempotent handling of duplicate or repeated events
- stale markers clearing only when the corresponding full data has actually been
  refreshed

### RECOVERY-001 Restart Ordering Matrix

Add restart tests for combinations of:

- pending wake hint
- active work item
- queued work item
- blocking task
- active timer

Required checks:

- only the highest-priority resume path fires first
- no duplicate wake-up occurs
- waiting reason and runtime status remain consistent after recovery

### RECOVERY-002 Command Task Recovery Internals

Add targeted tests for `src/runtime/command_task.rs`.

Required cases:

- recovered command task restores running state and output path correctly
- cancellation after partial output keeps terminal detail consistent
- runner failure cleanup removes task handle and persists failed terminal state
- terminal persistence remains canonical when enqueue of result message fails

## Wave 3: Worktree And External Ingress Edge Hardening

These are still important, but should follow the runtime/stream contract work.

### WORKTREE-001 Helper Error Paths

Add targeted tests for `src/runtime/worktree.rs`.

Required cases:

- detached HEAD reject path
- branch/path collision helper behavior
- failed `git worktree add` handling
- failed `git worktree remove` handling
- failed branch cleanup handling
- occupancy release behavior when enter/exit fails mid-flow

### CALLBACK-001 Waiting Intent Lifecycle Matrix

Add tests covering:

- repeated callback delivery with cancellation race
- wake-hint versus enqueue-message behavior on restart
- external trigger revocation and stale token handling
- waiting-intent and closure-decision interaction after delivery

### STORAGE-001 Recovery Snapshot Completeness

Add tests proving recovery snapshots remain complete across:

- tasks
- timers
- work items
- work plans
- external triggers
- waiting intents
- transcript tail and recent briefs

## Deferred Until `#221`

Do not mix the following into the current stabilization wave. These should be
added when `#221` lands so the tests match the new compaction contract instead
of the current transitional one.

### COMPACTION-001 Long-Running Headless Turn Survival

Required future cases:

- compaction triggers before provider context overflow
- preserved context is centered on `WorkItem`, `WorkPlan`, briefs, and relevant
  pending work
- compaction records what was preserved and when
- a long-running headless task can continue beyond the current failure point

### COMPACTION-002 Rejoin After Compaction

Required future cases:

- task rejoin after compaction still preserves the correct current work truth
- wake hints and queued work items still resume the correct next step
- no reintroduction of a parallel agent-wide objective shadow state

## Recommended Execution Order

If the goal is “make the base system boringly stable before the next benchmark
round”, the recommended order is:

1. `RUNTIME-001` through `RUNTIME-006`
2. `STREAM-001` through `STREAM-004`
3. `RECOVERY-001` and `RECOVERY-002`
4. `WORKTREE-001`, `CALLBACK-001`, and `STORAGE-001`
5. after `#221`, add the compaction-specific matrix and then start a new
   benchmark comparison cycle

## Bottom Line

Holon already has a large test suite.

The immediate problem is not missing quantity. The immediate problem is missing
direct contract tests around the runtime state machine, stream/replay
invariants, and recovery ordering.

Those are the tests most likely to increase stability before the next TUI soak
period and before the next benchmark round starts.
