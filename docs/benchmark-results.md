# Benchmark Results

This document summarizes the implemented benchmark waves:

- baseline: `baseline-preprompt`
- prompt architecture first pass: `prompt-v1`
- prompt architecture with finishing-contract fix: `prompt-v2`
- expanded targeted gap scans:
  - `gap-followup-v1`
  - `gap-multifile-v2`

**Note:** For structural refactors (SVS-301 through SVS-304), see
`docs/benchmark-guardrails.md` for the minimal guardrail benchmark set that
must stay green.

The initial corpus was intentionally small:

- `analysis-runtime-architecture`
- `fix-greeting-preserves-case`

The first expansion added two more targeted tasks:

- `followup-greeting-context`
- `fix-multi-file-config-merger`

The next expansion added four broader tasks:

- `failed-verification-retry`
- `followup-after-multifile-fix`
- `no-change-needed-analysis`
- `holon-project-roadmap-audit`

Each task was run once per runner in this first pass. That is enough to compare
directionally, but not enough to claim high statistical confidence.

## Setup

Compared runners:

- `HolonRunner`
- `ClaudeSdkRunner`

Shared constraints:

- same local fixture workspace
- same model endpoint
- same auth/base URL source
- no internet-dependent tasks
- no MCP, WebFetch, AskUserQuestion, or LSP

## Baseline

`baseline-preprompt` summary:

| Task | Runner | Success | Duration | Tool Calls | Notes |
|---|---|---:|---:|---:|---|
| analysis-runtime-architecture | Holon | yes | 7.4s | 6 | concise, grounded analysis |
| analysis-runtime-architecture | Claude SDK | yes | 12.4s | 7 | grounded analysis |
| fix-greeting-preserves-case | Holon | yes | 5.8s | 8 | fixed bug and verified |
| fix-greeting-preserves-case | Claude SDK | yes | 17.6s | 8 | fixed bug and verified |

Initial takeaway:

- On this corpus, Holon was already not obviously behind Claude SDK.
- Holon matched task success and was materially faster on the coding task.

## Prompt-V1

What changed:

- prompt assembly moved into explicit sections and modes
- dynamic context was separated from stable instructions
- tool guidance sections were added

Observed result:

- task success stayed at 100%
- analysis output became richer and more code-grounded
- but analysis-mode tool usage increased
- a regression appeared in the coding task:
  - Holon sometimes ended with `Completed.` after sleeping
  - the task still passed verification, but the user-facing result degraded

Takeaway:

- the architecture change itself was good
- the finishing contract was incomplete

## Prompt-V2

What changed from `prompt-v1`:

- added an explicit finishing contract:
  - provide the user-facing summary before ending the turn
  - do not call `Sleep` as the only final content when a summary is still owed
- added a generic reminder to avoid redundant tool calls

`prompt-v2` summary:

| Task | Runner | Success | Duration | Tool Calls | Notes |
|---|---|---:|---:|---:|---|
| analysis-runtime-architecture | Holon | yes | 11.7s | 12 | richest Holon analysis, but more tool-hungry |
| analysis-runtime-architecture | Claude SDK | yes | 12.5s | 7 | shorter analysis output |
| fix-greeting-preserves-case | Holon | yes | 4.8s | 8 | proper final summary restored |
| fix-greeting-preserves-case | Claude SDK | yes | 19.2s | 8 | slower but successful |

## Final Comparison

Comparing `baseline-preprompt` to `prompt-v2` for Holon:

- `analysis-runtime-architecture`
  - success: unchanged
  - latency: worse
  - tool calls: worse
  - output richness: better
- `fix-greeting-preserves-case`
  - success: unchanged
  - latency: slightly better
  - tool calls: unchanged
  - final result quality: preserved after the v2 fix

## Conclusion

The final conclusion from this first wave is:

- The prompt architecture refactor is worth keeping.
- The main benefit is not higher success rate on this small corpus.
- The main benefits are:
  - inspectability
  - cleaner abstraction boundaries
  - benchmarkability
  - easier diagnosis of prompt regressions

Behaviorally:

- Holon already matched Claude SDK on the two benchmark tasks.
- On this corpus, Holon remained faster than Claude SDK on the coding task even
  after prompt changes.
- The new prompt system improved output quality control, especially around
  finishing and user-facing summaries.
- The new analysis prompt is currently more tool-hungry than the baseline.

So the product decision is:

- keep `prompt-v2`
- do not claim prompt quality improved raw task completion
- next prompt work should focus on reducing analysis-mode over-exploration
  rather than adding more global instructions

## Next Recommended Step

The next step should not be another broad prompt rewrite.

The next highest-leverage step is:

- add a slightly larger benchmark corpus
- especially:
  - one more multi-file coding task
  - one follow-up/context-retention task
  - one project-audit task with stricter grounding criteria

Then tune analysis-mode tool selection against that broader corpus.

## Expanded Gap Scan

After the initial prompt benchmark wave, the corpus was expanded to expose two
more specific failure modes:

- multi-turn follow-up / context retention
- multi-file repair under a truly failing fixture

One invalid intermediate result is worth calling out explicitly:

- an earlier `config-merger-bug` fixture accidentally drifted into a passing
  state
- any results captured against that passing version should be treated as
  invalid
- the fixture was then corrected back to a genuinely failing shallow-merge bug
  before recording `gap-multifile-v2`

### Follow-Up Context Retention

`gap-followup-v1` summary:

| Task | Runner | Success | Duration | Tool Calls | Notes |
|---|---|---:|---:|---:|---|
| followup-greeting-context | Holon | yes | 15.0s | 9 | fixed the bug, then answered the follow-up correctly |
| followup-greeting-context | Claude SDK | no | 45.7s | 20 | answered with the wrong workspace/file context and failed verification |

Important observed behavior:

- Holon changed `src/greeting.js`, preserved context across turns, and answered
  the follow-up with the correct file, root cause, and verification command.
- Claude SDK produced a clearly wrong final answer:
  - referenced `benchmark/fixtures/config-merger-bug/src/merge.js`
  - claimed `node test.js` passed
  - but the actual `followup-greeting-context` verification still failed with
    the original `Hello, alice!` assertion

This is the strongest clean gap found so far.

The gap is not subtle:

- Holon succeeded
- Claude SDK failed
- Holon used fewer tools
- Holon finished materially faster

### Multi-File Repair

`gap-multifile-v2` summary:

| Task | Runner | Success | Duration | Tool Calls | Notes |
|---|---|---:|---:|---:|---|
| fix-multi-file-config-merger | Holon | yes | 44.1s | 20 | fixed `src/merge.js`, verified, but final brief collapsed to `Completed.` |
| fix-multi-file-config-merger | Claude SDK | no | n/a | 0 recorded result turns | hit `maxTurns=12` and left a partial fix behind |

Important observed behavior:

- Holon repaired the shallow-merge bug by changing `src/merge.js`, re-ran
  `node test.js`, and reached a passing verification state.
- Claude SDK also moved toward the right fix and edited `src/merge.js`, but did
  not converge before hitting:
  - `Claude Code returned an error result: Reached maximum number of turns (12)`
- Claude SDK left the workspace in a partially modified but still failing state.

This task surfaced a different gap than the follow-up test:

- the main issue was not wrong context recall
- the issue was convergence under a bounded turn budget

### What The Expanded Scan Changed

The initial benchmark wave suggested:

- Holon and Claude SDK were roughly tied on success rate
- Holon was often faster on the small local tasks

The expanded scan changes that conclusion in a meaningful way:

- Holon now has a demonstrated advantage on multi-turn follow-up handling
- Holon also has a demonstrated advantage on convergence for the current
  multi-file repair fixture
- Claude SDK remains a useful baseline, but it is no longer accurate to say the
  two systems are simply "roughly tied" on the current corpus

The more precise current conclusion is:

- on simple single-turn tasks, the two systems are close
- on the expanded tasks, Holon is ahead in practical task completion
- Holon still has an output-quality problem on some longer coding tasks, because
  it can finish with a weak final brief like `Completed.`

## Updated Conclusion

The benchmark conclusion should now be stated as:

- keep the `prompt-v2` architecture
- keep using Claude SDK as the comparison baseline
- treat multi-turn follow-up handling as a current Holon strength
- treat final result delivery on longer coding tasks as a current Holon weakness
- treat Claude SDK turn-budget exhaustion as a real benchmarked limitation in
  this harness

## Post-Benchmark Refinement Phase

After the earlier waves, the next refinement phase ran through `PB1-PB5` in
`docs/post-benchmark-roadmap.md`.

### PB1: Analysis Capability

Result:

- analysis mode in `src/prompt.rs` was strengthened to emphasize:
  - current state
  - concrete findings
  - prioritized recommendations
- roadmap-audit output became more grounded and less likely to repeat already
  completed work

Primary validation:

- `pb1-roadmap-audit-v1`

### PB2: Comparison Metrics

Result:

- `benchmark/run.mjs` now captures richer runner metrics:
  - `read_ops`
  - `search_ops`
  - `list_ops`
  - `unique_files_read`
  - `unique_search_queries`
  - `bytes_read`
  - `search_to_read_chains`

Primary validation:

- `pb2-metrics-roadmap-audit-v2`

The clean comparison run showed:

| Metric | Holon | Claude SDK |
|---|---:|---:|
| Success | yes | yes |
| Duration | 32.3s | 40.8s |
| Tool calls | 26 | 29 |
| Read ops | 19 | 19 |
| Search ops | 2 | 5 |
| List ops | 4 | 5 |
| Unique files read | 18 | 17 |
| Unique search queries | 2 | 10 |
| Bytes read | 204,757 | 75,010 |

Interpretation:

- Holon does not obviously read more files than Claude SDK
- the more meaningful difference is:
  - Claude SDK does more discovery-style search/list work
  - Holon reads larger chunks once it commits to evidence

### PB3: Analysis-Oriented Tooling

Result:

- added:
  - `TodoWrite`
  - `TaskList`
  - `TaskGet`
  - `TaskStop`
- todo snapshots now persist in storage and enter context construction
- runtime now supports cancelling running background tasks

Primary validation:

- `cargo test`
- `tests/runtime_flow.rs`

### PB4: Tool Surface Comparison

Result:

- comparison findings were captured in:
  - `docs/tool-surface-comparison.md`

Main conclusion:

- do not label Holon's current analysis behavior as a simple over-reading bug
- the current gap is better understood as a mix of:
  - tool-surface differences
  - read granularity
  - search/discovery strategy

### PB5: Final Delivery And Follow-Up Quality

Result:

- reporting guidance in `src/prompt.rs` was tightened again
- analysis mode now prefers a concise structured report
- roadmap-audit snapshot was updated to include
  `docs/post-benchmark-roadmap.md`
- `config-merger-bug` fixture drift was corrected back to a truly failing state

Primary validations:

- `pb5-roadmap-audit-v2`
- `pb5-followup-greeting-v1`

Observed outcomes:

- roadmap audit now finishes with a long structured report instead of a weak
  ending or stale recommendation set
- follow-up greeting context still succeeds with a compact grounded answer:
  - correct file
  - correct root cause
  - correct verification

## Current Conclusion

The current best benchmark-based judgment is:

- Holon is already competitive on open-ended analysis and local coding tasks
- the next useful improvements should target:
  - better evidence targeting
  - more realistic coordination benchmarks
  - tool-surface refinements only where metrics justify them
- raw tool-count reduction should not be treated as the main optimization goal

## Benchmark Expansion V1

After the `PB1-PB5` refinement phase, three additional benchmarks were added to
improve diagnosis:

- `coordination-sequential-render-plan`
- `analysis-evidence-improvements`
- `read-granularity-holon-analysis-pipeline`

These were run together in:

- `expansion-v1`

### Coordination Benchmark

Task:

- multi-turn coding task
- asks the agent to keep track of completed and pending steps while fixing the
  sequential render fixture

Result:

| Runner | Success | Duration | Tool Calls | TodoWrite | Verify |
|---|---:|---:|---:|---:|---:|
| Holon | yes | 11.5s | 18 | 4 | pass |
| Claude SDK | no | 62.8s | 21 | 0 | fail |

Interpretation:

- Holon used `TodoWrite` repeatedly and completed the task with a grounded
  follow-up answer.
- Claude SDK produced a plausible-looking plan/status report, but left the
  fixture unchanged and failed verification.

This is a useful benchmark because it measures more than code fixing:

- planning persistence
- session coordination
- truthful follow-up reporting

### Analysis Evidence Benchmark

Task:

- analyze a small runtime fixture
- recommend three concrete improvements
- every recommendation must cite specific files and explain a current
  limitation

Result:

| Runner | Success | Duration | Tool Calls | Read Ops | List Ops |
|---|---:|---:|---:|---:|---:|
| Holon | yes | 8.9s | 9 | 5 | 3 |
| Claude SDK | yes | 20.7s | 12 | 5 | 7 |

Interpretation:

- both runners succeeded
- both read the same number of files
- Holon reached the answer faster and with fewer total tool calls
- Claude SDK relied more on discovery/list operations for the same small
  fixture

This benchmark is good at measuring evidence discipline without mixing in
project-roadmap synthesis.

### Read Granularity Benchmark

Task:

- analyze a narrow Holon snapshot
- identify where prompt assembly, context assembly, tool execution, and
  benchmark comparison live

Result:

| Runner | Success | Duration | Tool Calls | Read Ops | List Ops | Bytes Read |
|---|---:|---:|---:|---:|---:|---:|
| Holon | yes | 15.3s | 8 | 7 | 1 | 157,142 |
| Claude SDK | yes | 22.5s | 18 | 12 | 6 | 139,124 |

Interpretation:

- Holon finished faster and with fewer total exploration steps
- Claude SDK used more discovery and more file reads on this narrow mapping
  task
- Holon still read large chunks once it committed to a file, so the benchmark
  supports a more precise claim:
  - Holon is not simply "over-reading"
  - Holon currently prefers broader file reads over more discovery steps

## Updated Judgment After Expansion V1

These new tasks strengthen the current conclusion:

- Holon is already strong on:
  - grounded follow-up
  - narrow analysis mapping
  - evidence-backed improvement recommendations
- the next benchmark work should continue to focus on:
  - coordination realism
  - evidence discipline
  - tool-surface diagnosis

The current evidence still does **not** justify a blanket claim that Holon's
analysis problem is "too many file reads". The more accurate framing is:

- Holon often reaches answers with fewer search/list steps
- Holon can still read larger evidence chunks than Claude SDK
- that is a refinement target, not a blocking defect

## Bounded Synthesis Iteration

After `extension-v2-bounded`, Holon's prompt system was updated with a
turn-scoped bounded-output section. This section activates only when the user
request explicitly asks for a bounded or highly concise answer.

The goal was:

- improve concise synthesis efficiency
- keep grounded file references
- avoid making wide analysis tasks artificially terse

Validation run:

- `bounded-v2`

### Bounded Synthesis Result

`bounded-synthesis-analysis-runtime`:

| Runner | Success | Duration | Tool Calls | Final Length | Read Ops |
|---|---:|---:|---:|---:|---:|
| Holon | yes | 3.9s | 11 | 993 | 5 |
| Claude SDK | yes | 13.9s | 8 | 1182 | 4 |

Compared to the earlier Holon run in `extension-v2-bounded`:

- duration improved from `25.5s` to `3.9s`
- final length dropped from `1428` to `993`
- read ops stayed controlled and grounded evidence remained intact

Interpretation:

- the bounded-output section materially improved Holon on the task it was meant
  to optimize
- Holon became faster than Claude SDK on this bounded synthesis benchmark while
  staying grounded

### Read Granularity Side Effect

`read-granularity-holon-analysis-pipeline` in the same run:

| Runner | Success | Duration | Tool Calls | Final Length | Read Ops | Unique Files |
|---|---:|---:|---:|---:|
| Holon | yes | 2.3s | 15 | 650 | 7 | 7 |
| Claude SDK | yes | 21.0s | 16 | 3665 | 12 | 12 |

Interpretation:

- the bounded-output optimization did not damage the broader mapping task
- on this rerun, Holon answered the scoped mapping question much faster while
  reading fewer files than Claude SDK
- this still does **not** justify promoting bounded-output guidance into all
  analysis turns; the current evidence only supports keeping it scoped to
  explicitly bounded requests

## Current Judgment

The current benchmark-based judgment is now more precise:

- Holon can be made highly competitive on concise bounded synthesis with a
  targeted, generic prompt contract
- that optimization should stay turn-scoped
- broader analysis efficiency should still be treated separately from bounded
  synthesis optimization

## Benchmark Extension V2

Two additional benchmark classes were then added:

- `task-inspection-subagent-status`
- `bounded-synthesis-analysis-runtime`

These were designed to answer two different questions:

- can Holon's new task-inspection tools support a real workflow?
- can Holon stay concise and grounded when the synthesis task is explicitly
  bounded?

### Task Inspection Benchmark

Task:

- Holon-only capability benchmark
- asks the agent to:
  - create a bounded subagent task
  - stop the main turn
  - later inspect the task state and report the result

Run:

- `extension-v2`

Result:

| Runner | Success | Duration | Tool Calls | CreateTask | TaskList | TaskGet |
|---|---:|---:|---:|---:|---:|---:|
| Holon | yes | 7.5s | 6 | 1 | 1 | 1 |

Interpretation:

- this benchmark is worth keeping
- it proves the new task-control tools are not just schema additions
- Holon used:
  - `CreateTask`
  - `TaskList`
  - `TaskGet`

Important caveat:

- the benchmark also exposed a quality issue:
  - the final answer included raw subagent output with internal planning traces
  - this suggests a result-hygiene gap in how subagent output is delivered back
    through task results

That hygiene issue was then fixed by:

- stronger `PromptMode::Subagent` output constraints
- runtime-side subagent result sanitization

Validation:

- `hygiene-v2`

Observed outcome after the fix:

- `task-inspection-subagent-status` still passed
- the final answer no longer leaked `<think>` blocks, pseudo-tool tags, or
  internal planning traces

So this benchmark should remain in the corpus, and now serves as a regression
test for subagent result hygiene.

### Bounded Synthesis Benchmark

Task:

- concise analysis task with a strict upper bound on final response length
- same fixture family as earlier analysis tasks
- still requires grounded file references and a concrete next milestone

Run:

- `extension-v2-bounded`

Result:

| Runner | Success | Duration | Tool Calls | Read Ops | Final Length |
|---|---:|---:|---:|---:|---:|
| Holon | yes | 25.5s | 7 | 4 | 1428 |
| Claude SDK | yes | 9.7s | 6 | 4 | 1024 |

Interpretation:

- this benchmark is also worth keeping
- it surfaced a real difference:
  - both runners stayed grounded
  - Claude SDK was materially faster and more concise
  - Holon still answered well, but was slower and longer under the same bounded
    synthesis task

This is useful because it isolates a narrower weakness than the open-ended
roadmap audit:

- not general analysis ability
- not file-reading discipline alone
- specifically concise synthesis efficiency

## Updated Judgment After Extension V2

The benchmark corpus now gives a more nuanced picture:

- Holon strengths:
  - grounded multi-turn follow-up
  - coordination with its own task/todo tools
  - efficient narrow mapping and evidence-backed analysis
- Holon weaknesses or open issues:
  - concise bounded synthesis is still slower than Claude SDK
  - subagent task result hygiene needs improvement

So the next good benchmark-informed work is no longer "add random tasks".
The next highest-value items are:

- fix subagent result-delivery hygiene
- improve concise synthesis efficiency without losing grounding
- continue growing the corpus around coordination and bounded reporting

So the next prompt/runtime work should focus on:

- stronger finishing/result-delivery guarantees for longer coding tasks
- richer benchmark coverage for follow-up and multi-turn sessions
- possibly revisiting Claude SDK adapter settings only if we want a separate
  "higher max-turn baseline" experiment

## Expansion Two

The next benchmark wave broadened the corpus in four directions:

- retry after a verification failure
- multi-file fix followed by a follow-up question
- restraint when no code change is needed
- open-ended project audit against a real `Holon` code snapshot

These tasks are implemented as:

- `failed-verification-retry`
- `followup-after-multifile-fix`
- `no-change-needed-analysis`
- `holon-project-roadmap-audit`

### What We Verified First

Before comparing runners, the fixtures and task definitions were smoke-tested
with Holon itself.

That showed:

- `failed-verification-retry` is a valid coding benchmark
- `followup-after-multifile-fix` is a valid multi-turn benchmark
- `no-change-needed-analysis` is a valid “do not edit” benchmark
- `holon-project-roadmap-audit` is intentionally harder and currently exposes a
  Holon weakness in open-ended analysis/result delivery

The open-ended audit task is therefore useful even though Holon does not yet
pass it reliably.

### No-Change-Needed Analysis

This task asks the runner to inspect a healthy fixture, verify it, and avoid
unnecessary edits.

Observed result:

| Task | Runner | Success | Duration | Tool Calls | Notes |
|---|---|---:|---:|---:|---|
| no-change-needed-analysis | Holon | yes | 14.9s | 8 | no edits, verified, produced a real analysis summary |
| no-change-needed-analysis | Claude SDK | yes | 17.8s | 7 | no edits, verified, also stayed disciplined |

Takeaway:

- this task is a good sanity benchmark
- both runners pass it
- it does not currently expose a large quality gap

That is useful because it shows the corpus is not biased toward forcing one side
to fail.

### Follow-Up After Multi-File Fix

This task asks the runner to repair `config-merger-bug`, then answer a
follow-up grounded in the actual repair.

Observed result:

| Task | Runner | Success | Duration | Tool Calls | Notes |
|---|---|---:|---:|---:|---|
| followup-after-multifile-fix | Holon | yes | 49.0s | 25 | repaired the bug, preserved follow-up context, answered with grounded file/root-cause/verification details |
| followup-after-multifile-fix | Claude SDK | no | 165.5s | 37 | answered confidently but verification still failed |

Important observed behavior:

- Holon repaired the bug and then answered the second-turn question using the
  actual session history.
- Claude SDK produced a convincing but false final answer:
  - claimed the changed file was `benchmark/fixtures/config-merger-bug/src/merge.js`
  - claimed `node test.js` passed
  - but the verify log still failed with the original `theme === "light"`
    assertion

This is now another strong clean gap in the benchmark corpus.

It is especially valuable because it combines:

- multi-step repair
- multi-turn context retention
- honest final reporting

### Failed Verification Retry

This task uses a small fixture with two formatting defects in the render path.

Observed result so far:

| Task | Runner | Success | Duration | Tool Calls | Notes |
|---|---|---:|---:|---:|---|
| failed-verification-retry | Holon | yes | 13.2s | 14 | repaired both defects and passed verification |

Important note:

- this benchmark does not force a strict “fail once, then retry” sequence
- a sufficiently strong runner may inspect both defects and fix them in one pass

So this task should be interpreted as:

- a benchmark that allows retry behavior
not:
- a benchmark that guarantees retry behavior

It is still worth keeping because it broadens beyond single-bug fixtures.

### Holon Project Roadmap Audit

This task asks the runner to read a real tracked snapshot of the `Holon`
repository and recommend the next concrete improvements with file-grounded
evidence.

Initial observed result:

| Task | Runner | Success | Duration | Tool Calls | Notes |
|---|---|---:|---:|---:|---|
| holon-project-roadmap-audit | Holon | no | 38.4s | 17 | gathered substantial evidence, but final result delivery collapsed into an over-short summary |

The initial failure exposed a real runtime/output bug rather than a pure
reasoning gap.

Root cause:

- the model sometimes called `Sleep` with a malformed structured payload instead
  of a single clean `reason`
- `Sleep` preserved only the short `reason` field and silently dropped the rest
  of the structured content
- `derive_final_text()` then preferred a short assistant preamble over the
  richer `Sleep` summary

This was fixed by:

- making `Sleep` preserve malformed structured payloads instead of collapsing
  them to a placeholder
- preferring a richer `Sleep` summary over obvious short preambles in
  `derive_final_text()`
- tightening the prompt contract so `Sleep` is told to pass exactly one string
  field for `reason`

After the fix, the same task became stable for both runners:

| Task | Runner | Success | Duration | Tool Calls | Final Message Length | Notes |
|---|---|---:|---:|---:|---:|---|
| holon-project-roadmap-audit | Holon | yes | 40.1s | 26 | 4756 | stable long-form report, grounded in current docs/code/benchmarks |
| holon-project-roadmap-audit | Claude SDK | yes | 82.6s | 18 | 4204 | stable long-form report, slightly more concise, slower overall |

One additional issue surfaced after that first fix:

- Holon still treated provider-side `max_tokens` truncation as a successful
  turn because it did not parse `stop_reason`
- Holon also preferred the `Sleep` tool record summary over the full
  `Sleep.reason` content, which could re-truncate a long report back into an
  ellipsized summary

That was corrected by:

- raising the old fixed `1024` output-token budget to a configurable runtime
  setting
- parsing provider `stop_reason`
- automatically continuing generation when the provider stops at `max_tokens`
- using the full `Sleep` result content rather than the truncated tool summary
  when deriving the final user-facing report

Revalidation after this second fix:

| Task | Runner | Success | Duration | Tool Calls | Final Message Length | Notes |
|---|---|---:|---:|---:|---:|---|
| holon-project-roadmap-audit | Holon | yes | 105.8s | 30 | 4349 | report completes cleanly after truncation-recovery and full Sleep-result delivery |

Comparison takeaway:

- the report-stability problem in Holon is now fixed in this benchmark
- Holon is faster on this task, but also more tool-hungry
- Claude SDK is slower, but produces a comparably grounded final report
- the current gap is no longer “Holon cannot finish open-ended audit tasks”
- the more precise remaining difference is:
  - Holon tends to over-read and over-assemble evidence
  - Claude SDK tends to read less, synthesize earlier, and spend more wall time

This task remains intentionally hard.

Its purpose is not only to check “can the model say something smart about the
repo”. Its purpose is to stress:

- open-ended project understanding
- roadmap judgment
- grounding in real files
- long-form result delivery quality

So the updated interpretation is:

- it is no longer just an aspirational benchmark
- it is now a useful comparison task for open-ended analysis quality and
  efficiency

## `SVS-401`: Focused Tool-Surface Recompare

`SVS-401` reran two focused comparison tasks with current token and model-round
metrics:

- `analysis-evidence-improvements`
- `read-granularity-holon-analysis-pipeline`

Fresh summary:

- `.benchmark-results/svs401-compare-v1/summary.json`

Key takeaways:

- Holon does not look like a simple "reads too many files" agent.
- On the focused evidence task, both runners read the same number of files.
- On the read-granularity task, Holon read fewer files and finished faster.
- Claude SDK still spends more steps in discovery/listing mode.
- Holon currently tends to spend more model rounds synthesizing once it has
  gathered evidence.
- Token and round cost are now observable on focused tasks, but historical
  older comparison runs still lack those counters and should not be used for
  token-cost claims.

See also:

- `docs/tool-surface-comparison.md`
