# Benchmark `#454` Turn-local Compaction Regression

Date: 2026-04-25
Label: `openai-live-refresh-2026-04-25-0454-2026-04-25T02-37-21-870Z`
Base: `b1b5dced5ff7361cd5e2ea4578cd06408e181c97`

## Final Disposition

This benchmark was originally evaluated as an artifact-first comparison because the verifier script was stale. That is still correct for the benchmark run itself, but the repository-level outcome is now settled:

- `#489` was promoted from draft, reviewed, fixed, passed CI, and merged:
  - PR: `#489`
  - merge commit: `7ba5d8f8001d9e50625943d9110201a385a43079`
- `#488` was kept only as an intermediate benchmark artifact and is now closed:
  - PR: `#488`
  - final state: `CLOSED`

So the benchmark conclusion is no longer just "prefer `#489` as the candidate". The repo has already accepted `#489` as the mainline resolution.

## Result Summary

This run compares `codex-openai` and `runtime-incubation-openai` on issue `#454`, a mock-provider regression task for repeated turn-local compaction, max-token recovery, structured tool output, and progress-signal preservation.

The verifier result should be ignored for this run because the verifier script called a non-existent test target:

```text
cargo test runtime_flow --test runtime_flow --quiet
```

Both runners failed that verifier for the same harness reason. The meaningful comparison is therefore the actual produced diff, PR, and execution trace.

### `codex-openai`

- Result artifact summary: verifier failed due harness issue
- Produced PR: `#488`
- Changed files:
  - `tests/runtime_compaction.rs`
  - `tests/support/runtime_flow.rs`
- Summary shape:
  - Adds one `MultiPassCompactionRecoveryFlowProvider`
  - Adds one integrated regression test:
    - `runtime_compaction_multi_pass_recovery_preserves_progress_and_artifacts`
  - Covers max-output recovery, repeated compaction, checkpoint prompt injection, structured tool execution records, and task artifact preservation.

### `runtime-incubation-openai`

- Result artifact summary: verifier failed due harness issue
- Produced PR: `#489`
- Commit: `a2dc160c4df4e4b9fd1c7b7acb668b2c62e78c16`
- Changed files:
  - `tests/runtime_compaction.rs`
  - `tests/support/runtime_flow.rs`
- Summary shape:
  - Adds two focused regression tests:
    - `repeated_turn_local_compaction_evolves_checkpoint_mode_and_keeps_latest_exact_tail`
    - `max_output_recovery_followed_by_turn_local_compaction_preserves_progress_signal`
  - Adds request snapshot helpers for checking prompt-visible compaction state.
  - Directly asserts `full` and `delta` checkpoint modes, delta base references, deterministic recap markers, and latest exact-tail preservation.

## Main Conclusion

`#489` is the stronger mainline candidate for `#454` if the evaluation criterion is final coverage against the issue contract.

`#488` is cleaner and narrower, and it includes a useful artifact-bearing `TaskOutput` scenario. But `#489` more directly tests the mechanisms introduced by the recent compaction work:

- repeated turn-local compaction
- `checkpoint_mode` evolution
- `full -> delta` checkpoint behavior
- delta prompt base reference
- latest exact-tail preservation
- deterministic recap preservation
- max-output recovery followed by later compaction

The better merge strategy is:

- keep `#489` as the main candidate
- port the artifact-bearing structured-output case from `#488` into `#489` or add an equivalent `TaskOutput` / artifact-pointer regression there
- then close `#488` as superseded only after that coverage is preserved

This matches the reviewer's assessment: `#489` is better as a mainline patch, but `#488` still contains one valuable realism case that should not be lost.

That recommendation was later carried out:

- the artifact-bearing structured-output case from `#488` was merged into `#489` during PR follow-up
- `#489` then went through additional CI/review fixes and merged
- `#488` was left closed as a benchmark artifact rather than a competing merge candidate

## Mechanism Check

The new full/delta checkpoint mechanism is active in this run.

`runtime-incubation-openai` emitted `18` `turn_local_compaction_applied` events:

```text
7 delta
11 full
```

Observed sequence examples:

```text
round 77 full
round 78 full
round 79 delta previous_checkpoint_round=78
round 80 delta previous_checkpoint_round=78
round 81 delta previous_checkpoint_round=78
```

The transcript also contains model-visible delta behavior:

- `New delta since base checkpoint`
- `Delta since base checkpoint`
- `New confirmed facts since the base checkpoint`

So the mechanism is working at the runtime and model-behavior levels:

- event metadata is emitted
- delta prompts are injected
- the model understands and follows the delta checkpoint request

However, the mechanism is not yet fully stable:

- there are still more `full` than `delta` checkpoints
- several later `full` checkpoints have no previous checkpoint base
- checkpoint reuse appears to depend too much on assistant text recognition
- continuation boundaries can reset the effective checkpoint anchor

The next improvement should make checkpoint state structural rather than text-derived. Runtime should track the last checkpoint request / response round as metadata instead of inferring it from assistant text such as `progress checkpoint`.

## Execution Process

The important process change from earlier failed runs is that pre-public runtime no longer stays in read-only exploration forever.

This run shows pre-public runtime entering implementation:

- it creates a work item and plan
- it inspects issue and code
- it edits `tests/runtime_compaction.rs`
- it edits `tests/support/runtime_flow.rs`
- it creates PR `#489`

But the process is still expensive and unstable:

- many rounds are spent debugging test wait conditions
- multiple `ApplyPatch` attempts fail due context mismatch
- the agent repeatedly expands event windows and timeout logic
- it spends late rounds inspecting `messages.jsonl` / `events.jsonl` schemas
- the final message is not a clean completion report; it stops while still analyzing a mixed-schema artifact

This distinction matters:

- as a product/runtime benchmark, pre-public runtime improved materially because it implements and opens a PR
- as an execution-quality benchmark, pre-public runtime still burns a large amount of tokens during test-debugging loops

One post-benchmark nuance matters here: the merged PR did not keep every benchmark-time stress case in the default CI path.

During PR follow-up, two very slow stress-style compaction regressions were moved out of the default CI suite and marked for manual/explicit runs, while the artifact-bearing recovery coverage and the more stable regression checks stayed in the merged patch. So the final merged result should be read as:

- stronger issue-aligned regression coverage did land
- the most timing-sensitive stress cases were treated as follow-up/manual regression coverage, not default-CI coverage

## Token Comparison

### Raw Totals

| Runner | Input tokens | Output tokens | Total tokens | Tool calls | Shell commands | Duration |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `codex-openai` | `28,326,694` | `191,416` | `28,518,110` | `480` | `389` | `2,166,185 ms` |
| `runtime-incubation-openai` | `32,498,857` | `501,953` | `33,000,810` | `487` | `468` | `2,497,197 ms` |

### Ratios

| Metric | pre-public runtime / Codex |
| --- | ---: |
| Input tokens | `1.15x` |
| Output tokens | `2.62x` |
| Total tokens | `1.16x` |
| Tool calls | `1.01x` |
| Duration | `1.15x` |

The headline is that pre-public runtime's total token usage is only about `16%` higher, not multiple times higher. The real gap is output tokens.

pre-public runtime output tokens are `2.62x` Codex output tokens, while tool call count is nearly identical.

### pre-public runtime Per-round Shape

pre-public runtime provider-round stats:

```text
rounds=518
input=32,498,857
output=501,953
avg_input_per_round=62,739
avg_output_per_round=969
max_input_round=121,327
max_output_round=21,640
high_input_rounds_>50k=341
high_output_rounds_>1k=96
```

This means:

- compaction keeps input bounded but the prompt baseline remains large
- many rounds still carry more than `50k` input tokens
- the output cost is driven by repeated verbose reasoning/progress messages, not by tool count

### Checkpoint Text Is Not The Main Token Cost

Checkpoint-like rounds in pre-public runtime:

```text
checkpoint_like_rounds=39
input=1,854,859
output=26,402
avg_input=47,560
avg_output=677
```

Checkpoint output is only about `26k` out of `502k` total output tokens, around `5%`.

So `#479` checkpoint throttling is still useful, but it is not the main token lever in this run. The larger cost comes from normal implementation/debugging rounds where the model explains every step in detail.

## Why pre-public runtime Uses More Output Tokens

### 1. Progress reporting is helping convergence but increasing verbosity

The prompt changes that make pre-public runtime report progress each round appear to help with liveness and implementation convergence. The tradeoff is that nearly every tool call is preceded by a natural-language status update.

In a short task this is acceptable. In a 500-round test-debugging loop it becomes the dominant output cost.

### 2. Test debugging is the expensive phase

The largest output rounds occur during:

- failed patch attempts
- test wait-condition redesign
- timeout / event-window debugging
- schema inspection of benchmark artifacts
- reasoning about whether provider call counts or event streams are reliable

Examples include output rounds above `10k` and one round above `21k`.

This is not checkpoint overhead. It is uncontrolled debugging narration.

### 3. pre-public runtime chooses a more complete but more complex patch

`#489` is stronger on final coverage, but it is also harder to design:

- two tests instead of one
- explicit request snapshots
- full/delta checkpoint assertions
- exact-tail preservation checks
- max-output + compaction interaction

The richer target increased the amount of exploratory debugging and thus output tokens.

### 4. Input is bounded but still has a high baseline

pre-public runtime averages about `62.7k` input tokens per model round. Compaction prevents runaway context growth, but the repeated prompt/context/tool/work-memory baseline remains large.

The input gap versus Codex is modest in this run (`1.15x`), but still worth optimizing after output verbosity is addressed.

## Optimization Ideas

### Output-token controls

Add phase-aware progress reporting:

- Before first edit: allow short explanatory updates.
- During test debugging: cap status text to one concise sentence unless plan or hypothesis changes.
- At compaction boundary: allow full or delta checkpoint.
- After repeated failed tests: require concise failure table + next action, not long narrative.

Add a runtime/prompt rule:

> If the previous round already stated the current hypothesis and next command, do not restate them. Say only the changed fact and run the next bounded action.

### Checkpoint improvements

Do not infer reusable checkpoints from assistant text alone.

Track checkpoint state structurally:

- last checkpoint prompt mode
- assistant round that answered it
- anchor state at that time
- whether a mutation/verification anchor changed since then

Then reuse this state across continuation boundaries where safe.

### Test-helper improvements

`#454` exposed that agents invent brittle wait logic. Add reusable helpers so both Codex and pre-public runtime have a simpler target:

- `wait_for_event_kind_count(runtime, kind, count, timeout)`
- `wait_for_agent_asleep(runtime, timeout)`
- `assert_checkpoint_modes_include_full_then_delta(events)`
- `captured_provider_requests(provider)`
- `assert_latest_exact_tail_contains(requests, marker)`

This should reduce both debugging loops and output-token cost.

### Patch/debug guardrail

If `ApplyPatch` fails twice on the same region:

- read a narrower exact snippet
- apply one small hunk
- stop explaining the entire strategy again

This would directly target the output-heavy patch-failure loops seen in this run.

## Implication For `#470`

This run is good evidence that the new checkpoint mechanism is partially effective.

It is not enough to close `#470` yet:

- repeated compaction does now emit full/delta metadata
- the model does produce delta checkpoints
- but checkpoint storms still occur
- full checkpoint reuse is not stable enough
- output-token cost remains high during long debugging loops

Close `#470` only after a later run shows:

- no exploration-only loop
- stable full/delta checkpoint reuse
- no repeated full checkpoint storm
- final PR output is mergeable without carrying over coverage from a competing PR

## Artifact Paths

- `codex-openai`
  - `/Users/jolestar/opensource/src/github.com/holon-run/runtime-incubation/.benchmark-results/openai-live-refresh-2026-04-25-0454-2026-04-25T02-37-21-870Z/runtime-incubation-0454-mock-provider-turn-local-compaction-regression/codex-openai/run-01`
- `runtime-incubation-openai`
  - `/Users/jolestar/opensource/src/github.com/holon-run/runtime-incubation/.benchmark-results/openai-live-refresh-2026-04-25-0454-2026-04-25T02-37-21-870Z/runtime-incubation-0454-mock-provider-turn-local-compaction-regression/runtime-incubation-openai/run-01`
