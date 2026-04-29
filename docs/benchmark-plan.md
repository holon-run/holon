# Holon Benchmark Plan

This document defines how to compare `Holon` against a Claude Agent SDK-based
runner under the same model endpoint, workspace snapshot, and task prompt.

Phase 1 now has a second track for real-repo benchmarking driven by
`benchmarks/tasks/*.yaml` and `benchmarks/suites/*.yaml`. The fixture corpus in
`benchmark/tasks/*.json` remains the fast guardrail layer; the real-repo
manifests are the benchmark path for benchmark PR/worktree comparisons.

The goal is not to prove that one system is universally “better”. The goal is
to isolate where `Holon` is currently weaker:

- prompt architecture
- prompt content
- tool orchestration
- context handling across turns
- result reporting quality

## Comparison Goal

The benchmark should answer this question:

Given the same coding task, the same model API, and the same workspace state,
does `Holon` perform materially worse, equal, or better than a minimal Claude
Agent SDK wrapper?

More specifically:

- does `Holon` choose useful tools
- does `Holon` converge in a reasonable number of turns
- does `Holon` preserve the right context on follow-up turns
- does `Holon` produce a clear final result
- does `Holon` verify its work reliably

## What Must Be Held Constant

Every benchmark run must keep these inputs identical:

- same model name
- same `ANTHROPIC_BASE_URL`
- same `ANTHROPIC_AUTH_TOKEN`
- same workspace snapshot
- same initial user prompt
- same time budget
- same verification script
- same filesystem contents before each run

The benchmark should avoid comparing unrelated variables such as:

- network fetch quality
- external documentation retrieval
- MCP server behavior
- IDE/LSP integration
- human follow-up clarification

## What Will Not Be Benchmarked Yet

The first benchmark wave should explicitly exclude:

- `WebFetch`
- `MCP`
- `AskUserQuestion`
- `LSP` or editor diagnostics
- tasks that require internet access
- tasks that require manual approval or human clarification mid-run

This keeps the comparison focused on coding-runtime quality rather than product
surface area.

## Two Runner Shapes

The harness should support two adapters with the same benchmark contract:

### 1. HolonRunner

This adapter should drive the existing `Holon` runtime:

- create a fresh session
- inject the benchmark prompt
- wait for completion or timeout
- collect transcript, events, tool records, briefs, and final workspace diff

### 2. ClaudeSdkRunner

This adapter should wrap Claude Agent SDK with a deliberately small tool
surface that is as close as possible to Holon’s current tools:

- file listing/search/read
- file write/edit
- shell execution
- sleep/no-op completion
- optional task stubs only if needed for parity

The benchmark should not allow the SDK runner to use capabilities that Holon
does not currently have.

## Benchmark Modes

The harness should support two modes.

### Controlled Mode

Goal:

- compare prompt quality and reasoning shape with tool usage minimized

Method:

- pre-read the same inputs
- inject the same prepared context bundle
- either disable tools or reduce them to read-only
- compare the output quality and next-step recommendations

Use this mode for:

- codebase understanding
- architecture explanation
- project audit
- roadmap critique

### Runtime Mode

Goal:

- compare real coding-agent behavior under actual file and shell tools

Method:

- give both runners the same workspace snapshot
- allow the same local tool surface
- run the same task prompt
- evaluate both execution traces and final verification result

Use this mode for:

- bug fixing
- multi-file edits
- test-driven repair
- follow-up refinement after a failed verification

## Benchmark Task Format

Each benchmark task should be stored as a fixture file with a stable schema.

Suggested fields:

```yaml
name: fix_runtime_status_bug
mode: runtime
workspace_fixture: fixtures/runtime-status-bug
prompt: Fix the runtime status bug and verify the result.
setup:
  - cargo test --quiet
verify:
  - cargo test --quiet
success_criteria:
  - verify_exit_code: 0
  - max_files_changed: 4
  - required_brief_substring: fixed
timeouts:
  total_seconds: 180
```

The harness should copy the fixture into a temporary workspace before each run.

## Suggested First Benchmark Corpus

The first benchmark wave should be intentionally small and local-only.

### Analysis Tasks

- explain the current runtime/session architecture
- audit the project state and recommend the next milestone
- compare two modules and identify the main responsibility split

### Coding Tasks

- fix a small failing test
- make a targeted one-file behavior change
- perform a two-file refactor with verification
- inspect a command failure and repair the code
- answer a follow-up question that depends on previous tool results

### Multi-Turn Tasks

- make a change, then answer “what changed”
- fix a bug, then answer “why was it broken”
- edit a file, then adapt after a second user instruction

## Expanded Corpus

The benchmark corpus now includes four broader tasks beyond the initial bugfix
set:

- `failed-verification-retry`
  - a fixture with multiple local formatting defects where a single local fix may
    not be enough
- `followup-after-multifile-fix`
  - fix a multi-file bug, then answer a follow-up grounded in the actual repair
- `no-change-needed-analysis`
  - verify a healthy fixture and avoid unnecessary edits
- `holon-project-roadmap-audit`
  - read a snapshot of the real `Holon` codebase and recommend the next concrete
    improvements with file-grounded evidence

These tasks broaden the benchmark beyond “small bugfix” toward:

- retry behavior after failed verification
- multi-turn explanation quality
- restraint when no change is needed
- open-ended codebase understanding

## Metrics To Record

Each run should emit structured metrics.

Minimum metrics:

- `task_name`
- `runner`
- `success`
- `verify_success`
- `duration_ms`
- `model_turns`
- `tool_calls`
- `shell_commands`
- `files_changed`
- `final_brief_length`
- `timed_out`
- `error_kind`

Useful secondary metrics:

- `time_to_first_tool_ms`
- `tool_errors`
- `sleep_calls`
- `task_creations`
- `context_summary_size`
- `events_count`

## Artifacts To Persist

Each run should persist a directory of artifacts.

Minimum artifacts:

- `prompt.txt`
- `transcript.jsonl`
- `events.jsonl`
- `tools.jsonl`
- `briefs.jsonl`
- `metrics.json`
- `final_message.md`
- `git.diff`
- `verify.log`

The point is not only to score runs, but to make failures inspectable.

## Fairness Rules

The harness should enforce a few strong fairness rules:

- no network access in benchmark tasks
- no hidden benchmark-specific prompt tweaks for one runner only
- no extra tools for the SDK runner that Holon does not have
- no reuse of previous run state
- no manual intervention once a run starts

If a runner needs a custom adapter detail, it should be documented explicitly in
the artifact bundle.

## Repetition Policy

Single runs are too noisy to trust.

Each benchmark task should be run at least:

- `3` times per runner for baseline comparison

The harness should report:

- per-run results
- average success rate
- average duration
- average tool-call count
- average shell-command count

If variance is high, increase repetitions before making a product decision.

## Benchmark Success Criteria

The benchmark framework is useful when it can clearly answer at least one of
these:

- Holon fails tasks that the SDK runner completes
- Holon uses materially more turns to finish the same task
- Holon loses follow-up context more often
- Holon’s final result reporting is less clear or less faithful
- a prompt architecture change improves Holon on the same corpus

If it cannot distinguish those outcomes, the corpus or metrics are not good
enough yet.

## Immediate Build Order

The recommended implementation order is:

1. define the benchmark fixture schema
2. build a local workspace-fixture copier
3. implement `HolonRunner`
4. implement `ClaudeSdkRunner`
5. emit artifact bundles and metrics
6. add a small initial corpus
7. establish the first baseline report

Only after a baseline exists should prompt changes be evaluated.

## Implemented First Wave

The first benchmark wave is now implemented under:

- `benchmark/run.mjs`
- `benchmark/tasks/analysis-runtime-architecture.json`
- `benchmark/tasks/fix-greeting-preserves-case.json`
- `benchmark/fixtures/analysis-runtime/`
- `benchmark/fixtures/greeting-bug/`

Current runner contract:

- `HolonRunner`
  - runs `target/debug/holon run --json`
  - reuses one local `HOLON_HOME` + `--agent` across benchmark turns
  - collects run artifacts from `HOLON_HOME/agents/<agent_id>/`
- `ClaudeSdkRunner`
  - uses `@anthropic-ai/claude-agent-sdk`
  - runs against the same workspace snapshot and model endpoint
  - restricts built-in tools to:
    - read-only mode: `Read`, `Glob`, `Grep`
    - coding mode: `Read`, `ApplyPatch`, `Glob`, `Grep`, `Bash`

Raw benchmark artifacts are written to `.benchmark-results/` and intentionally
stay out of git.
