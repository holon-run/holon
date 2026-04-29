# Holon Issue Backlog

This document turns the current roadmap set into a concrete issue list.

It currently covers these two roadmap documents:

- `docs/semantic-vs-structural-roadmap.md`
- `docs/worktree-design-roadmap.md`

The goals of this backlog are:

- make the next work items concrete
- preserve execution order
- make dependencies visible
- keep scope small enough that work can ship incrementally

## Status Summary

### Completed

- `SVS-001` to `SVS-004`
- `SVS-101` to `SVS-104`
- `SVS-201` to `SVS-204`
- `SVS-301` to `SVS-305`
- `SVS-401` to `SVS-404`
- `WT-001` to `WT-005`
- `WT-101` to `WT-105`
- `WT-201` to `WT-204`

### Notes

- `SVS-201` was attempted in a Holon-driven worktree session and failed to land
  code cleanly. The main issue appears to be brittle edit primitives plus a task
  that was too wide for the current agent behavior.
- `SVS-403` and `SVS-404` landed together as a direct public tool-surface
  cutover. Holon now exposes only the canonical names and aligned schemas; the
  old names are not kept as compatibility aliases.

## Conventions

- `SVS-*`: issues from the semantic-vs-structural roadmap
- `WT-*`: issues from the worktree roadmap
- `P0`: do next
- `P1`: do soon after prerequisites land
- `P2`: defer until earlier layers are stable

## Phase S1: Benchmark Observability V3

### `SVS-001` Add Total Token Usage To Benchmark Metrics

- Priority: `P0`
- Depends on: none
- Scope:
  - capture total input tokens when available
  - capture total output tokens when available
  - persist them into benchmark `metrics.json` and suite `summary.json`
- Done when:
  - [x] at least one Holon run and one Claude SDK run expose token totals
  - [x] benchmark summary can compare token cost directly
- Deliverable:
  - `benchmark/run.mjs`

### `SVS-002` Add Model Round Count To Benchmark Metrics

- Priority: `P0`
- Depends on: none
- Scope:
  - count provider/model turns for Holon
  - count result-yielding turns for Claude SDK runs
  - include `model_rounds` in benchmark artifacts
- Done when:
  - [x] benchmark reports can distinguish "fewer tools" from "more model rounds"
- Deliverable:
  - `benchmark/run.mjs`

### `SVS-003` Add Per-Tool Latency Metrics

- Priority: `P0`
- Depends on: none
- Scope:
  - record start/end timing for each Holon tool execution
  - summarize by tool name and total tool latency
  - expose the aggregate in benchmark artifacts
- Done when:
  - [x] benchmark comparison can show where time is spent inside tool execution
- Deliverable:
  - `benchmark/run.mjs`

### `SVS-004` Improve Benchmark Suite Summary Reporting

- Priority: `P0`
- Depends on:
  - `SVS-001`
  - `SVS-002`
  - `SVS-003`
- Scope:
  - add a benchmark summary view that includes:
    - success
    - duration
    - token usage
    - model rounds
    - tool latency
  - make it easy to compare Holon vs Claude SDK at the suite level
- Done when:
  - [x] one benchmark run produces a readable "why this runner was faster/slower"
    summary
- Deliverable:
  - `benchmark/run.mjs`
  - `.benchmark-results/*/summary.md`

## Phase S2: Analysis And Synthesis Modes Stabilization

### `SVS-101` Split Analysis Prompt Rules More Explicitly By Mode

- Priority: `P0`
- Depends on: none
- Scope:
  - clarify the behavior contract for:
    - open-ended analysis
    - bounded analysis
    - follow-up explanation
  - keep all guidance generic and reusable
- Done when:
  - [x] prompt dump clearly shows which rules come from which mode
  - [x] no task-specific benchmark hacks are added
- Deliverable:
  - `src/prompt.rs`

### `SVS-102` Add Benchmarks For Mode-Specific Regressions

- Priority: `P1`
- Depends on:
  - `SVS-101`
- Scope:
  - add or refine benchmark tasks that separately stress:
    - open-ended analysis
    - bounded synthesis
    - follow-up explanation
- Done when:
  - [x] a prompt change can be evaluated by mode rather than only by total suite
    result
- Deliverable:
  - `benchmark/tasks/bounded-synthesis-analysis-runtime.json`
  - `benchmark/tasks/followup-greeting-context.json`
  - `benchmark/tasks/followup-after-multifile-fix.json`
  - `docs/benchmark-results.md`

### `SVS-103` Stabilize Follow-Up Explanation Quality

- Priority: `P1`
- Depends on:
  - `SVS-101`
  - `SVS-102`
- Scope:
  - tighten follow-up answer contract so it stays grounded in:
    - recent edits
    - recent tool results
    - recent briefs
- Done when:
  - [x] follow-up benchmarks remain green after prompt/runtime iterations
- Deliverable:
  - `src/prompt.rs`
  - `docs/benchmark-results.md`

### `SVS-104` Keep Bounded Output Optimization Strictly Turn-Scoped

- Priority: `P1`
- Depends on: none
- Scope:
  - preserve the current bounded-output improvement
  - ensure it does not leak into general analysis turns
- Done when:
  - [x] bounded synthesis stays fast and concise
  - [x] open-ended analysis does not become artificially compressed
- Deliverable:
  - `src/prompt.rs`
  - `docs/benchmark-results.md`

## Phase S3: Coding Loop Hardening

### `SVS-201` Improve Long-Task Final Delivery Quality

- Priority: `P0`
- Status: **completed**
- Depends on: none
- Scope:
  - ensure long coding tasks finish with:
    - what changed
    - why
    - verification result
  - avoid weak generic completions
- Done when:
  - [x] long coding benchmarks no longer regress to vague final briefs
- Deliverable:
  - `src/runtime/delivery.rs`
  - `src/runtime/turn.rs`
  - `src/prompt.rs`
  - `docs/benchmark-results.md`

### `SVS-202` Strengthen Verification And Retry Discipline

- Priority: `P1`
- Status: **completed**
- Depends on:
  - `SVS-201`
- Scope:
  - make verification state more visible in final delivery
  - improve retry behavior after failed verification where benchmark evidence
    justifies it
- Done when:
  - [x] retry-oriented coding tasks stay convergent and auditable
- Deliverable:
  - `src/runtime/delivery.rs`
  - `src/runtime/turn.rs`
  - `src/prompt.rs`
  - `docs/benchmark-results.md`

### `SVS-203` Make Todo And Task Control Show Up In Real Long-Running Flows

- Priority: `P1`
- Status: **completed**
- Depends on: none
- Scope:
  - improve prompt/runtime guidance so `TodoWrite`, `TaskList`, `TaskGet`, and
    `TaskStop` are used when they add value
  - avoid forcing them into simple tasks
- Done when:
  - [x] longer coordination benchmarks use them naturally
- Deliverable:
  - `src/prompt.rs`
  - `docs/benchmark-results.md`

### `SVS-204` Preserve Subagent Result Hygiene As A Regression Guard

- Priority: `P0`
- Status: **completed**
- Depends on: none
- Scope:
  - keep current subagent sanitization behavior covered by regression tests
  - extend tests if new result leakage patterns appear
- Done when:
  - [x] subagent result hygiene remains stable through future runtime changes
- Deliverable:
  - `src/runtime.rs`
  - `docs/benchmark-results.md`

## Phase S4: Structural Refactor Without Semantic Rewrite

### `SVS-301` Extract Turn Execution From `runtime.rs`

- Priority: `P0`
- Depends on:
  - `SVS-001`
  - `SVS-002`
  - `SVS-003`
- Scope:
  - split session lifecycle from one model/tool turn of execution
  - keep behavior unchanged
- Done when:
  - [x] `runtime.rs` no longer owns all turn-execution responsibilities
  - [x] benchmark suite shows no meaningful regression
- Deliverable:
  - `src/runtime/turn.rs`

### `SVS-302` Extract Final Delivery Logic From `runtime.rs`

- Priority: `P1`
- Depends on:
  - `SVS-301`
- Scope:
  - move final result derivation / delivery shaping into a clearer module
- Done when:
  - [x] final delivery behavior is easier to test independently of session loop
- Deliverable:
  - `src/runtime/delivery.rs`

### `SVS-303` Split Tool Spec / Dispatch / Execution

- Priority: `P0`
- Depends on: none
- Scope:
  - break `src/tools.rs` into clearer layers
  - preserve current tool names and runtime behavior
- Done when:
  - [x] tool schema exposure
  - [x] tool routing
  - [x] tool execution
  are no longer concentrated in one file
- Deliverable:
  - `src/tool/`

### `SVS-304` Separate Provider Turn Contract From Session Orchestration

- Priority: `P1`
- Depends on:
  - `SVS-301`
- Scope:
  - clarify the boundary between:
    - prompt/context assembly
    - provider turn call
    - session state management
- Done when:
  - [x] provider interaction is easier to test without booting the full session loop
- Deliverable:
  - `src/runtime/provider_turn.rs`

### `SVS-305` Add Benchmark Guardrails For Structural Refactors

- Priority: `P0`
- Status: **completed**
- Depends on:
  - `SVS-004`
- Scope:
  - define the minimal benchmark set that must stay green while structural
    refactors land
- Done when:
  - [x] every major refactor has an agreed benchmark safety rail
  - [x] guardrail criteria documented
  - [x] five core guardrails identified and documented
  - [x] usage instructions provided for running guardrails
- Deliverable:
  - `docs/benchmark-guardrails.md`

## Phase S5: Tool Surface Decision

### `SVS-401` Compare Current Exploration Tool Surface Against Claude SDK

- Priority: `P1`
- Depends on:
  - `SVS-001`
  - `SVS-002`
  - `SVS-003`
- Scope:
  - use benchmark evidence to compare:
    - discovery steps
    - read granularity
    - tool latency
    - token cost
- Done when:
  - [x] the project has a concrete explanation of current tool-surface tradeoffs
- Deliverable:
  - `docs/tool-surface-comparison.md`

### `SVS-402` Decide Whether A Stronger Exploration Tool Is Needed

- Priority: `P2`
- Depends on:
  - `SVS-401`
- Scope:
  - decide whether to add a new exploration-oriented tool
  - or keep the current tool set and improve strategy instead
- Done when:
  - [x] a written go/no-go decision exists with benchmark evidence behind it
- Deliverable:
  - `docs/svs402-decision.md`

### `SVS-403` Replace The Public Tool Surface With Canonical Names

- Priority: `P1`
- Status: **completed**
- Depends on:
  - `SVS-402`
- Scope:
  - replace the old public tool names instead of adding aliases
  - align file/discovery names to:
    - `Glob`
    - `Grep`
    - `Read`
    - `Write`
    - `Edit`
  - align command execution naming to:
    - `exec_command`
  - align the exposed input schema toward Claude-style file/discovery tools and
    Codex-style `exec_command`
- Done when:
  - [x] only canonical names are exposed by the tool registry
  - [x] prompt, runtime, tests, and benchmark metrics use canonical names
  - [x] the old public names are rejected instead of being kept as aliases
- Deliverable:
  - `src/tool/dispatch.rs`
  - `src/tool/execute.rs`
  - `docs/basic-tool-comparison.md`

### `SVS-404` Add `ApplyPatch` As The Primary Complex Edit Primitive

- Priority: `P0`
- Status: **completed**
- Depends on:
  - `SVS-403`
- Scope:
  - add an `ApplyPatch` tool modeled after Codex-style structured patching
  - keep `Edit` for small exact replacements
  - move complex refactor guidance toward `ApplyPatch`
- Done when:
  - [x] `ApplyPatch` supports add, delete, update, and move operations
  - [x] patch application validates the full patch before mutating files
  - [x] prompt/runtime guidance treats `ApplyPatch` as the primary structural
    edit primitive
  - [x] long-task edit retries no longer depend only on brittle exact
    replacement semantics
- Deliverable:
  - `src/tool/`
  - `docs/basic-tool-comparison.md`

## Worktree Layer 1: Session-Level Worktree Tools

### `WT-001` Add Worktree Session State To Runtime State Model

- Priority: `P1`
- Depends on:
  - `SVS-301`
  - `SVS-302`
- Scope:
  - persist worktree session metadata:
    - original cwd
    - original branch
    - worktree path
    - worktree branch
- Done when:
  - [x] a session can remember that it entered a worktree
  - [x] this state can be restored on resume when valid
- Deliverable:
  - `src/types.rs`
  - `src/worktree_tests.rs`
  - `tests/worktree_storage_test.rs`

### `WT-002` Implement `EnterWorktree`

- Priority: `P1`
- Depends on:
  - `WT-001`
- Scope:
  - create a managed worktree
  - switch session workspace root into it
  - audit and persist the transition
- Done when:
  - [x] a running session can enter a worktree through a formal tool
- Deliverable:
  - `src/tool/execute.rs`
  - `tests/runtime_flow.rs`

### `WT-003` Implement `ExitWorktree`

- Priority: `P1`
- Depends on:
  - `WT-001`
  - `WT-002`
- Scope:
  - leave the current managed worktree
  - support:
    - keep
    - remove
  - refuse destructive cleanup when changes exist unless forced
- Done when:
  - [x] a session can safely leave or discard a managed worktree
- Deliverable:
  - `src/tool/execute.rs`
  - `tests/runtime_flow.rs`

### `WT-004` Add Worktree Prompt And Context Visibility

- Priority: `P1`
- Depends on:
  - `WT-001`
- Scope:
  - expose worktree metadata in prompt/context when relevant
- Done when:
  - [x] the model can tell whether it is operating in a worktree
- Deliverable:
  - `src/context.rs`

### `WT-005` Add Regression Tests For Session-Level Worktree Lifecycle

- Priority: `P1`
- Depends on:
  - `WT-002`
  - `WT-003`
  - `WT-004`
- Scope:
  - test enter
  - test exit keep
  - test exit remove
  - test resume with preserved worktree state
- Done when:
  - [x] worktree lifecycle is covered by integration tests
- Deliverable:
  - `tests/runtime_flow.rs`

## Worktree Layer 2: Worktree-Isolated Subagent Tasks

### `WT-101` Add `worktree_subagent_task` Kind

- Priority: `P1`
- Depends on:
  - `WT-001`
  - `WT-002`
  - `SVS-303`
- Scope:
  - extend task kinds to include worktree-isolated subagent execution
- Done when:
  - [x] the runtime can represent worktree-isolated background work explicitly
- Deliverable:
  - `src/types.rs`
  - `src/runtime.rs`

### `WT-102` Run Subagent Task In Dedicated Worktree

- Priority: `P1`
- Depends on:
  - `WT-101`
- Scope:
  - create a per-task worktree
  - run subagent in that worktree
  - keep parent session cwd unchanged
- Done when:
  - [x] one background task can run in an isolated working copy
- Deliverable:
  - `src/runtime.rs`

### `WT-103` Return Worktree Metadata In Task Results

- Priority: `P1`
- Depends on:
  - `WT-102`
- Scope:
  - include:
    - worktree path
    - branch
    - changed files summary
  in task results
- Done when:
  - [x] parent session can inspect preserved worktree outputs
- Deliverable:
  - `src/runtime.rs`
  - `tests/runtime_flow.rs`

### `WT-104` Auto-Cleanup Unchanged Worktrees

- Priority: `P2`
- Depends on:
  - `WT-102`
  - `WT-103`
- Scope:
  - remove worktree automatically when the task made no changes
- Done when:
  - [x] no-op worktree tasks do not leave garbage behind
- Deliverable:
  - `src/runtime.rs`
  - `tests/runtime_flow.rs`

### `WT-105` Keep Changed Worktrees For Review

- Priority: `P1`
- Depends on:
  - `WT-103`
- Scope:
  - preserve worktree when changes exist
  - make it easy for the operator to inspect or discard later
- Done when:
  - [x] changed worktree tasks behave as reviewable artifacts rather than temporary
    side effects
- Deliverable:
  - `src/runtime.rs`
  - `tests/runtime_flow.rs`

## Worktree Layer 3: Parallel Worktree Development Orchestration

### `WT-201` Let One Session Coordinate Multiple Worktree Tasks

- Priority: `P2`
- Depends on:
  - `WT-101`
  - `WT-102`
  - `WT-103`
- Scope:
  - allow the top-level session to create multiple worktree-isolated subtasks
- Done when:
  - [x] one parent session can supervise several isolated attempts
- Deliverable:
  - `tests/wt201_multiple_worktree_tasks.rs`

### `WT-202` Summarize Candidate Worktree Results For Review

- Priority: `P2`
- Depends on:
  - `WT-201`
- Scope:
  - produce a clear parent-session summary of:
    - task status
    - worktree path
    - changed files
    - verification result
- Done when:
  - [x] the operator can quickly decide which worktree attempt to inspect
- Deliverable:
  - `src/http.rs`
  - `tests/wt202_worktree_task_summary.rs`

### `WT-203` Add Discard Workflow For Failed Or Unwanted Worktree Attempts

- Priority: `P2`
- Depends on:
  - `WT-201`
  - `WT-202`
- Scope:
  - provide a clean way to remove bad attempts after review
- Done when:
  - [x] poor attempts can be discarded without manual cleanup guessing
- Deliverable:
  - `src/runtime.rs`
  - `tests/wt203_worktree_task_discard.rs`

### `WT-204` Add Benchmark Or Demo Workflow For Parallel Worktree Development

- Priority: `P2`
- Depends on:
  - `WT-201`
  - `WT-202`
  - `WT-203`
- Scope:
  - create one realistic workflow or benchmark showing:
    - parallel worktree attempts
    - reviewable outputs
    - keep/discard decisions
- Done when:
  - [x] worktree orchestration is demonstrated end-to-end
- Deliverable:
  - `tests/wt204_parallel_worktree_workflow.rs`

## Suggested Execution Order

If the team wants a strict recommended sequence, the current order should be:

1. `SVS-001`
2. `SVS-002`
3. `SVS-003`
4. `SVS-004`
5. `SVS-101`
6. `SVS-104`
7. `SVS-103`
8. `SVS-201`
9. `SVS-204`
10. `SVS-203`
11. `SVS-301`
12. `SVS-303`
13. `SVS-302`
14. `SVS-304`
15. `SVS-305`
16. `SVS-401`
17. `SVS-402`
18. `WT-001`
19. `WT-002`
20. `WT-003`
21. `WT-004`
22. `WT-005`
23. `WT-101`
24. `WT-102`
25. `WT-103`
26. `WT-105`
27. `WT-104`
28. `WT-201`
29. `WT-202`
30. `WT-203`
31. `WT-204`

## Current Recommendation

The next practical chunk is the remaining coding-loop hardening work:

- `SVS-201`
- `SVS-202`
- `SVS-203`
- `SVS-204`

After that, the backlog should be reviewed again to decide whether any further
worktree/product cleanup is still needed beyond the implemented layers.
