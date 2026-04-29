# pre-public runtime Benchmark PR Comparison Review

Date: 2026-04-15

## Scope

This note compares the benchmark-generated paired implementations for the same pre-public runtime issues and makes a selection recommendation for each issue.

Compared pairs:

1. `#124`
   - `#144` `[bench][runtime-incubation-openai][#124] Bug: daemon stop fails closed against incompatible runtime status response`
   - `#145` `[bench][codex-openai][#124] Bug: daemon stop fails closed against incompatible runtime status response`
2. `#122`
   - `#146` `[bench][runtime-incubation-openai][#122] Bug: blocking subagent tasks can regress from completed back to running`
   - `#147` `[bench][codex-openai][#122] Bug: blocking subagent tasks can regress from completed back to running`
3. `#118`
   - `#125` `feat: add daemon runtime activity summary`
   - `#148` `[bench][codex-openai][#118] Feature: make daemon status show active agent and task summary`
4. `#115`
   - `#141` `docs: codify local operator troubleshooting workflow`
   - `#149` `[bench][codex-openai][#115] Docs: codify the local operator troubleshooting workflow`

## Review Method

- Compare the code and docs diffs directly.
- Judge each pair on product correctness, architecture quality, safety, and how directly the implementation addresses the issue.
- Do not treat benchmark runner identity as a deciding factor.

## Results

### Issue `#124`

Selected version: `#144`

Not selected: `#145`

Reasoning:

1. `#144` fixes the operator-facing failure mode directly.
   - It rewrites the `daemon stop` incompatible-runtime error into a clear fail-closed explanation.
   - It gives concrete next steps:
     - inspect `runtime-incubation daemon logs`
     - inspect `runtime-incubation daemon status`
     - terminate by PID if needed
     - then restart a compatible runtime

2. `#145` is more of a message-refactor than a product fix.
   - It extracts and reuses a shared incompatible-contract message.
   - It pushes the same wording into both `daemon_status` and `daemon_stop`.
   - That is cleaner structurally, but less aligned with the actual issue.

3. `#145` gives a weaker recovery recommendation.
   - It points users toward `runtime-incubation daemon restart`.
   - In this fail-closed incompatibility path, `restart` is not the strongest advice because `stop` is exactly the operation that may not be safe.

Conclusion:

- `#124` is primarily about safe and actionable operator guidance under fail-closed behavior.
- `#144` solves that more directly and more safely than `#145`.

### Issue `#122`

Selected version: `#147`

Not selected: `#146`

Reasoning:

1. `#147` fixes the runtime state-machine layer.
   - It introduces logic to ignore stale non-terminal task updates after a task is already terminal.
   - It prevents the parent runtime from being pulled back into `AwaitingTask` or stale active-task state by late task messages.

2. `#146` mainly changes the storage merge/view layer.
   - It makes `latest_task_records()` prefer terminal task states over later non-terminal records.
   - That improves what readers see from storage, but it does not directly repair the runtime behavior that produced the inconsistency.

3. The issue is not only about task-list presentation.
   - The real bug includes:
     - terminal tasks regressing to running
     - parent state and `active_task_ids` becoming inconsistent
   - `#147` addresses the source of that inconsistency.
   - `#146` mainly masks one visible symptom at the aggregation layer.

Conclusion:

- `#122` should be fixed at the runtime semantics layer, not only at the storage aggregation layer.
- `#147` is the stronger and more correct implementation.

### Issue `#115`

Selected version: `#141`

Not selected: `#149`

Reasoning:

1. `#141` preserves the full operator mental model.
   - It distinguishes one-shot reproduction from long-lived runtime troubleshooting.
   - It explains entry-point choice, inspection order, recovery flow, and why the order exists.
   - It reads like a product runbook rather than a command checklist.

2. `#149` over-compresses the workflow.
   - It reduces the runbook to a short “primary path”, some use-case branches, and a memory cue.
   - That is concise, but it drops too much of the reasoning and edge-case guidance.

3. `#149` also mixes in unrelated test-deflake work.
   - It modifies `tests/wt204_parallel_worktree_workflow.rs` alongside the docs rewrite.
   - That makes it a less disciplined implementation of the issue itself.

Conclusion:

- `#115` is better served by a complete and explicit troubleshooting runbook.
- `#141` is substantially stronger than `#149`.

### Issue `#118`

Selected version: `#125`

Not selected: `#148`

Reasoning:

1. `#125` is the real implementation of the feature.
   - It adds the runtime activity model itself:
     - `RuntimeActivitySummary`
     - `idle | waiting | processing`
     - activity counts for agents and tasks
   - It computes activity from public agent snapshots.
   - It makes `/control/runtime/status` expose the activity contract.
   - It keeps daemon-status decoding backward-compatible with older runtimes.

2. `#148` is only a presentation-layer follow-up.
   - It adds a compact `activity_summary` string on `DaemonStatusView`.
   - It assumes the structured `activity` field already exists.
   - That makes it a formatting/polish layer on top of `#125`, not a competing primary implementation.

3. `#148` does not stand on its own as the issue solution.
   - The underlying activity summary contract, host snapshot plumbing, HTTP exposure, and tests are in `#125`.
   - Without that base, `#148` does not actually deliver the feature described by `#118`.

Conclusion:

- `#118` should be judged on whether it adds real daemon activity visibility as a product/runtime surface.
- `#125` clearly does.
- `#148` is at most an optional follow-up for compact rendering, not the implementation to choose for the issue itself.

## Final Selection Summary

1. `#124`: choose `#144`
2. `#122`: choose `#147`
3. `#118`: choose `#125`
4. `#115`: choose `#141`

## Overall Conclusion

Across these three issue pairs, the preferred implementations are the ones that:

- solve the problem at the correct layer
- preserve clearer operator semantics
- avoid masking runtime bugs with read-layer heuristics
- keep scope aligned with the actual issue

In practice that means:

- choose the safer operator-facing `daemon stop` fix for `#124`
- choose the runtime-state-machine fix for `#122`
- choose the full troubleshooting runbook for `#115`
