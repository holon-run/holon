# Live Benchmark Report: Test Coverage Batch

Date: 2026-04-06

Model alignment:

- Holon: `openai-codex/gpt-5.3-codex-spark`
- Codex CLI: `gpt-5.3-codex-spark`

Task batch:

- `#50` Tests: cover runtime result closure and terminal delivery
- `#51` Tests: cover command task lifecycle and cancellation
- `#52` Tests: cover worktree lifecycle and cleanup edge cases
- `#55` Tests: cover `exec_command` and tool error paths

## Framework changes used for this run

- `holon run` final result now comes from an explicit terminal delivery step instead of taking text from a tool-using work round.
- Real benchmark prompts now use operator-style input only.
- Exact verifier commands are hidden from the agent and stay benchmark-owned.
- `changed_files` aggregation now includes untracked created files.
- Real-task scoring now requires an actual diff. No-change runs no longer score `success: true`.
- OpenAI-style requests now set `store: false` explicitly.
- Codex benchmark runs now use an isolated `CODEX_HOME` and archive session artifacts under each run directory.
- Benchmark summaries now record runner-specific turn metrics so Codex CLI turn counts are not presented as directly comparable to Holon provider rounds.
- Codex benchmark fairness defaults now disable project docs and bundled/user skills during head-to-head runs.

## Completed head-to-head result

### `#50` runtime result closure and terminal delivery

Holon:

- status: success
- verify: pass
- changed files: `tests/result_closure.rs`
- duration: `284801 ms`
- input tokens: `267,784`
- output tokens: `13,130`
- turns: `26 (provider_rounds)`
- tool calls: `33`
- shell commands: `5`
- scope: soft violation
- PR: [#56](https://github.com/holon-run/holon/pull/56)

Codex:

- status: success
- verify: pass
- changed files: `tests/run_once.rs`
- duration: `357472 ms`
- input tokens: `2,065,902`
- output tokens: `15,866`
- turns: `1 (codex_cli_turns)`
- tool calls: `44`
- shell commands: `42`
- scope: clean
- PR: [#57](https://github.com/holon-run/holon/pull/57)

Observations:

- Holon completed the task and produced a real terminal summary. The previous half-sentence `final_text` problem did not reproduce on this run.
- Holon wandered more and landed in a new dedicated test file, which tripped the soft scope metric.
- Codex stayed inside an existing test file and produced a cleaner scope outcome, but used far more shell commands.
- Input token usage diverged sharply even though both runners used the same model slug.
  - Holon: `267,784`
  - Codex: `2,065,902`
- Output token usage was relatively close.
  - Holon: `13,130`
  - Codex: `15,866`

### Tool-use comparison for `#50`

Holon tool breakdown:

- `Read`: `11`
- `Glob`: `6`
- `Grep`: `6`
- `exec_command`: `5`
- `Edit`: `3`
- `Write`: `1`
- `TaskOutput`: `1`

Codex command/tool breakdown from JSONL events:

- shell file reads (`sed`, `cat`, `wc`): `33`
- search (`rg`): `5`
- `cargo test`: `2`
- `git diff`: `1`
- `ls`: `1`

Interpretation:

- Holon used its structured native tools more directly.
- Codex used a much more shell-heavy workflow.
- The token gap is likely explained largely by this difference in tool surface and output shape, not by model mismatch.

### Metric caveat: runner turn metrics are not apples-to-apples

- Holon turn metrics count completed provider turns.
- Codex turn metrics count `turn.completed` events from `codex exec --json`.
- For `#57`, Codex emitted many `item.started/item.completed` events but only one `turn.completed`, so `1 (codex_cli_turns)` should not be interpreted as directly comparable to Holon's `26 (provider_rounds)`.

## Partial result and blockers

### `#51` command task lifecycle and cancellation

Holon failed before producing a diff.

Observed failure:

- `OpenAI-style request failed with status 400 Bad Request: {"detail":"Stream must be set to true"}`

Implication:

- This is not a prompt-quality issue.
- It is an `openai-codex` transport compatibility problem on longer multi-round runs.
- The batch currently measures a real product gap: Holon can complete simpler live tasks, but the current Codex-subscription backend integration is not stable enough for more complex multi-round coding tasks.

Codex started editing:

- `tests/run_once.rs`
- `tests/runtime_flow.rs`

But the run was not taken to completion in this session because the benchmark driver became unreliable once the Holon side had already failed and needed to be interrupted manually.

## Additional benchmark issues found during execution

1. `openai-codex` provider compatibility is still incomplete.
   - `store: false` was required and has been fixed.
   - a second backend requirement, `stream: true`, still blocks some Holon live tasks.

2. The benchmark `real` / `suite` driver still has a bad post-run path in some failure cases.
   - After a Holon runtime error with no diff, the driver could remain alive without active child processes.
   - This needs a separate harness fix before large unattended live batches are trustworthy.

3. We can now capture Codex session artifacts, but prompt-level comparison still needs a second pass.
   - Codex includes built-in base instructions, AGENTS.md layers, and possibly skill instructions.
   - The benchmark now archives isolated Codex session artifacts, but we still need to inspect and document which files contain the fully assembled prompt stack.
   - Prompt-level comparisons remain incomplete until that artifact inspection is automated.

## Recommended next steps

1. Fix the remaining `openai-codex` transport incompatibility around `stream`.
2. Fix benchmark driver post-run hangs after runner failure or no-change exits.
3. Inspect the newly archived Codex session artifacts and identify the exact files that record effective prompt assembly.
4. Re-run the full live batch for `#50`, `#51`, `#52`, and `#55`.
5. Revisit prompt-level comparisons only after Codex session inspection is in place; do not rush Holon prompt changes before we can compare prompt stacks more fairly.

## Improvement plan

### A. Codex session capture

Goal:

- make the Codex benchmark run inspectable at the same level as Holon prompt dumps and transcripts

Implemented in this branch:

- each Codex benchmark run now uses an isolated `CODEX_HOME`
- Codex session and history artifacts are preserved under the run artifact directory
- session metadata is written to `codex-session.json`
- `CODEX_DISABLE_PROJECT_DOC=1` disables `AGENTS.md` loading during benchmark runs
- temporary benchmark `config.toml` sets `skills.bundled.enabled = false`
- benchmark runtime homes do not copy user `skills/`

Still to do:

- confirm whether the stored rollout contains:
  - base instructions
  - AGENTS.md fragments
  - skill fragments
  - effective user/developer messages

Why this matters:

- without session capture, we cannot fairly compare Holon prompt assembly vs Codex prompt assembly

### B. Benchmark driver robustness

Goal:

- make `real` / `suite` runs reliable in failure cases

Planned changes:

- fix the path where a runner finishes with no child process alive but the benchmark driver remains stuck
- make failure/no-change exits finalize cleanly with stable artifacts
- prefer per-runner result closure over long-lived suite process blocking

### C. Metric normalization

Goal:

- avoid misleading cross-runner comparisons

Implemented in this branch:

- benchmark summaries now include runner-specific turn semantics
  - `runner_turns`
  - `runner_turns_kind`
- token usage, changed files, and verification outcome remain the primary comparable metrics

Still to do:

- optionally add normalized tool categories
  - search
  - read
  - edit
  - verification
  - vcs / diff

### D. Provider compatibility

Goal:

- make Holon's Codex-subscription path stable enough for multi-round live tasks

Planned changes:

- fix the remaining `stream` requirement mismatch
- then re-run `#51` first as a probe task before re-running the whole batch

### E. Prompt comparison

Goal:

- decide whether Holon prompt changes are actually needed

Planned approach:

- do not rush more Holon prompt tuning now
- first capture Codex session/prompt artifacts
- then compare:
  - operator prompt
  - Codex base instructions
  - AGENTS.md injection
  - skill injection
  - Holon system/context prompt dump
- only after that decide whether Holon needs prompt changes, tool-guidance changes, or something else
