# Live Benchmark Report: Test Coverage Batch

Date: 2026-04-06

Model alignment:

- pre-public runtime: `openai-codex/gpt-5.3-codex-spark`
- Codex CLI: `gpt-5.3-codex-spark`

Task batch:

- `#50` Tests: cover runtime result closure and terminal delivery
- `#51` Tests: cover command task lifecycle and cancellation
- `#52` Tests: cover worktree lifecycle and cleanup edge cases
- `#55` Tests: cover `exec_command` and tool error paths

## Framework changes used for this run

- `runtime-incubation run` final result now comes from an explicit terminal delivery step instead of taking text from a tool-using work round.
- Real benchmark prompts now use operator-style input only.
- Exact verifier commands are hidden from the agent and stay benchmark-owned.
- `changed_files` aggregation now includes untracked created files.
- Real-task scoring now requires an actual diff. No-change runs no longer score `success: true`.
- OpenAI-style requests now set `store: false` explicitly.
- Codex benchmark runs now use an isolated `CODEX_HOME` and archive session artifacts under each run directory.
- Benchmark summaries now record runner-specific turn metrics so Codex CLI turn counts are not presented as directly comparable to pre-public runtime provider rounds.
- Codex benchmark fairness defaults now disable project docs and bundled/user skills during head-to-head runs.

## Completed head-to-head result

### `#50` runtime result closure and terminal delivery

pre-public runtime:

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
- PR: [#56](https://github.com/holon-run/runtime-incubation/pull/56)

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
- PR: [#57](https://github.com/holon-run/runtime-incubation/pull/57)

Observations:

- pre-public runtime completed the task and produced a real terminal summary. The previous half-sentence `final_text` problem did not reproduce on this run.
- pre-public runtime wandered more and landed in a new dedicated test file, which tripped the soft scope metric.
- Codex stayed inside an existing test file and produced a cleaner scope outcome, but used far more shell commands.
- Input token usage diverged sharply even though both runners used the same model slug.
  - pre-public runtime: `267,784`
  - Codex: `2,065,902`
- Output token usage was relatively close.
  - pre-public runtime: `13,130`
  - Codex: `15,866`

### Tool-use comparison for `#50`

pre-public runtime tool breakdown:

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

- pre-public runtime used its structured native tools more directly.
- Codex used a much more shell-heavy workflow.
- The token gap is likely explained largely by this difference in tool surface and output shape, not by model mismatch.

### Metric caveat: runner turn metrics are not apples-to-apples

- pre-public runtime turn metrics count completed provider turns.
- Codex turn metrics count `turn.completed` events from `codex exec --json`.
- For `#57`, Codex emitted many `item.started/item.completed` events but only one `turn.completed`, so `1 (codex_cli_turns)` should not be interpreted as directly comparable to pre-public runtime's `26 (provider_rounds)`.

## Partial result and blockers

### `#51` command task lifecycle and cancellation

pre-public runtime failed before producing a diff.

Observed failure:

- `OpenAI-style request failed with status 400 Bad Request: {"detail":"Stream must be set to true"}`

Implication:

- This is not a prompt-quality issue.
- It is an `openai-codex` transport compatibility problem on longer multi-round runs.
- The batch currently measures a real product gap: pre-public runtime can complete simpler live tasks, but the current Codex-subscription backend integration is not stable enough for more complex multi-round coding tasks.

Codex started editing:

- `tests/run_once.rs`
- `tests/runtime_flow.rs`

But the run was not taken to completion in this session because the benchmark driver became unreliable once the pre-public runtime side had already failed and needed to be interrupted manually.

## Additional benchmark issues found during execution

1. `openai-codex` provider compatibility is still incomplete.
   - `store: false` was required and has been fixed.
   - a second backend requirement, `stream: true`, still blocks some pre-public runtime live tasks.

2. The benchmark `real` / `suite` driver still has a bad post-run path in some failure cases.
   - After a pre-public runtime runtime error with no diff, the driver could remain alive without active child processes.
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
5. Revisit prompt-level comparisons only after Codex session inspection is in place; do not rush pre-public runtime prompt changes before we can compare prompt stacks more fairly.

## Improvement plan

### A. Codex session capture

Goal:

- make the Codex benchmark run inspectable at the same level as pre-public runtime prompt dumps and transcripts

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

- without session capture, we cannot fairly compare pre-public runtime prompt assembly vs Codex prompt assembly

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

- make pre-public runtime's Codex-subscription path stable enough for multi-round live tasks

Planned changes:

- fix the remaining `stream` requirement mismatch
- then re-run `#51` first as a probe task before re-running the whole batch

### E. Prompt comparison

Goal:

- decide whether pre-public runtime prompt changes are actually needed

Planned approach:

- do not rush more pre-public runtime prompt tuning now
- first capture Codex session/prompt artifacts
- then compare:
  - operator prompt
  - Codex base instructions
  - AGENTS.md injection
  - skill injection
  - pre-public runtime system/context prompt dump
- only after that decide whether pre-public runtime needs prompt changes, tool-guidance changes, or something else

## Update: Fair Issue-URL Runs on 2026-04-07

After the report above, I ran two more fairer `#50` benchmarks to isolate Codex prompt pollution and compare behavior when the model-visible prompt only contains the GitHub issue URL.

### 1. Fairness fix: isolate both `HOME` and `CODEX_HOME`

The first meaningful fairness improvement was isolating both:

- `CODEX_HOME`
- `HOME`

and setting:

- `project_doc_max_bytes = 0`

in Codex's temporary `config.toml`.

This matters because earlier runs still let Codex load:

- `AGENTS.md` / project-doc instructions
- user-home skills from `$HOME/.agents/skills`

After isolating both homes, the Codex rollout no longer contained:

- `AGENTS.md instructions ...`
- `<skills_instructions>`

So the final model-visible prompt was much closer to:

- permissions / sandbox instructions
- the benchmark issue URL prompt

This means later differences are much less likely to be caused by prompt pollution from local docs or home-level skills.

### 2. Prompt shape: issue URL only

For the next runs, the benchmark prompt was reduced to:

```text
Resolve GitHub issue in this repository:
https://github.com/holon-run/runtime-incubation/issues/50
```

This is much closer to a real operator assignment than the earlier task-card style prompt.

### 3. Important correction: Codex did not actually fetch the private issue

At first glance it looked like Codex was better because it could read the GitHub issue while pre-public runtime could not.

That is not what happened.

The repository is private, so the remote issue content was not actually available. In the later isolated run, Codex explicitly reported that it could not fetch the issue details from the web in this environment.

What Codex did better was:

- try to retrieve the issue remotely
- then switch to reconstructing the task from local evidence in the repository

This local evidence included things like:

- benchmark file names
- issue-numbered task artifacts
- existing source / test file names
- nearby notes and backlog files

So the real capability gap is not "Codex can read private GitHub issues".

The real gap is:

- Codex is stronger at task reconstruction / evidence synthesis when the task entry point is incomplete
- pre-public runtime is weaker at reconstructing the intended task once the direct reference cannot be resolved

### 4. Run: `fair-0050-issue-url-home-isolated-20260407`

This was the first isolated issue-URL run after removing Codex prompt pollution.

Observed outcome:

- pre-public runtime: success, but wrong task
- Codex: success, correct task

What pre-public runtime did:

- did not fetch the private issue
- searched local files
- incorrectly interpreted issue `#50` as a repository metadata / `Cargo.toml` style task

What Codex did:

- also could not reliably access the private issue content
- but reconstructed the intended task from local evidence
- landed in `src/run_once.rs` and `tests/run_once.rs`

Interpretation:

- this run showed that even after removing `AGENTS.md` and skill injection from Codex's assembled prompt, Codex still behaved better
- therefore the remaining difference was already pointing toward a framework/policy gap rather than prompt contamination

### 5. Minimal pre-public runtime prompt/tool-guidance patch

To test whether the gap was partly caused by pre-public runtime's workspace-first tool guidance, I made a minimal change in pre-public runtime:

- add a general prompt principle saying that when the operator provides an external reference or indirect task entry point, the agent should first retrieve the missing task context if it can do so directly
- broaden `exec_command` guidance so it is not treated as mostly verification-only
- reduce the bias toward broad repository mapping before grounding the task

This was intentionally kept abstract.

I did not add any GitHub-specific recipe like "use `gh issue view` when you see an issue URL".

### 6. Run: `fair-0050-issue-url-home-isolated-guidance-20260407`

This was the same issue-URL benchmark after the minimal pre-public runtime prompt/tool-guidance patch.

Observed outcome:

- pre-public runtime: no-op failure, but grounded
- Codex: success, correct task

What changed in pre-public runtime's behavior:

- pre-public runtime no longer made the wrong `Cargo.toml` edit
- it first used `exec_command` to try:
  - `curl -s https://api.github.com/repos/holon-run/runtime-incubation/issues/50`
- after getting a `404`, it searched local files
- it then stopped without making unrelated edits

This is an improvement.

The failure mode changed from:

- wrong positive

to:

- grounded no-op

That means the new principle did help:

- pre-public runtime now treats the issue URL as something to resolve first
- it no longer confidently "solves" a different local task

But it still falls short of Codex because:

- it does not reconstruct the hidden task strongly enough from local evidence after the remote reference fails

### 7. Current best interpretation

At this point the remaining gap is best described as:

- `task grounding`
- `tool prior`
- `task reconstruction / evidence synthesis`

not:

- prompt pollution from `AGENTS.md`
- hidden Codex skills
- privileged access to the private GitHub issue

More specifically:

- Codex seems better at turning an incomplete external reference into a workable internal task model
- pre-public runtime now attempts direct grounding, but still lacks the follow-through to synthesize a correct task from partial evidence when direct grounding fails

### 8. Implication for pre-public runtime improvements

The next pre-public runtime improvement should not be a GitHub-specific shortcut.

It should stay at the more general framework level:

- stronger task-grounding default
- less workspace-first bias in tool guidance
- better evidence synthesis once a reference cannot be resolved directly

The most likely high-level design axes are:

- `task grounding`
- `tool prior`
- `termination policy`

That is where the next benchmark-driven comparison should focus.

## Update: Externalized Live Benchmark Manifests on 2026-04-07

The issue-URL runs above still had one fairness concern:

- although the model-visible prompt no longer contained task-card metadata
- the tested repository still contained live benchmark manifests such as `benchmarks/tasks/runtime-incubation-0050-...yaml`

That meant an agent could still recover the benchmark goal by reading the repo.

To remove that leakage, I moved the live benchmark manifests and suite definitions out of the tested repository and into:

- `workspace/projects/holon-run/runtime-incubation/benchmarks/`

The tested `runtime-incubation` worktree no longer contains the live `#50/#51/#52/#55` benchmark task files.

### 1. Prompt / policy change used for this run

Before rerunning, I also generalized pre-public runtime's new grounding principle further.

The updated prompt contract now says, in effect:

- when the operator gives an indirect task entry point, first resolve it into a sufficiently grounded task
- use local or network tools proactively
- a failed first lookup is not automatically blocking
- avoid high-commitment edits before the task is sufficiently grounded

This is still general and not GitHub-specific.

### 2. Run: `fair-0050-externalized-grounding-20260407`

This run used:

- an issue-URL-only prompt
- isolated `HOME` + `CODEX_HOME`
- `project_doc_max_bytes = 0`
- live manifests stored outside the tested repository

Observed outcome:

- pre-public runtime: no-op failure, cleanly grounded
- Codex: success, but with a different repair direction than earlier runs

#### pre-public runtime

pre-public runtime behavior improved again:

- it attempted to fetch the issue via GitHub API
- it searched the local repository
- with the live benchmark manifests now removed from the repo, it found no reliable local task reconstruction path
- it stopped without making unrelated changes

This is important because it confirms:

- the earlier local task reconstruction path really was benefiting from benchmark leakage
- after removing that leakage, pre-public runtime no longer finds enough local evidence to invent a task

So pre-public runtime's result here is a *clean* no-op:

- not successful
- but also not contaminated by a wrong local fix

#### Codex

Codex still completed the task even after the live manifests were moved out of the repo.

But the nature of the result changed:

- it no longer landed in `tests/run_once.rs`
- instead it changed:
  - `src/runtime.rs`
  - `src/runtime/delivery.rs`

So after removing benchmark-task leakage, Codex still reconstructed "runtime final result handling" as the relevant problem area, but chose a code-level heuristic fix rather than a test-coverage implementation.

This means:

- Codex still shows stronger task reconstruction than pre-public runtime
- but the *shape* of its reconstruction changed once the benchmark files were removed from the repo

### 3. Fairness confirmation for Codex prompt assembly

For this externalized run, the captured rollout still shows:

- developer permissions instructions
- no `AGENTS.md instructions ...`
- no `<skills_instructions>`

So by this point:

- project-doc injection is removed
- user/home skills are removed
- repo-local live benchmark task files are removed

This is the cleanest head-to-head comparison in this sequence so far.

### 4. What this latest run tells us

The latest fair interpretation is:

- pre-public runtime has improved from wrong-task completion to grounded refusal
- Codex remains better at reconstructing a plausible task from incomplete evidence
- but once benchmark leakage is removed, Codex's reconstruction is less precise than before

So the remaining difference is not:

- private GitHub issue access
- `AGENTS.md`
- hidden skills
- repo-local live benchmark manifests

The remaining difference looks more like:

- stronger task reconstruction / hypothesis formation in Codex
- weaker task reconstruction but safer termination in pre-public runtime

That is a more informative and more trustworthy benchmark conclusion than the earlier runs.
