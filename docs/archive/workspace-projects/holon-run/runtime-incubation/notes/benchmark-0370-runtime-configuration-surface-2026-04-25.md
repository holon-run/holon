# Benchmark `#370` Runtime Configuration Surface

Date: 2026-04-25
Base: `55b03ffc50669917615d4d3398c621b7874ce3a4`
Label: `openai-live-refresh-2026-04-25-0370-rerun`

## Result

This rerun produced the first clean head-to-head result for `#370`: both `codex-openai` and `runtime-incubation-openai` completed successfully and passed verifier.

### `codex-openai`

- Result: success
- Verify: success
- Commit: `cc7dc8807a8ac1057f455e002d2b6c5a0b78b5f7`
- PR: `#481`
- Changed files:
  - `src/config.rs`
  - `src/main.rs`
- Tokens:
  - input: `7,775,062`
  - output: `44,428`
- Tool calls: `121`
- Duration: `471,982 ms`

Shape:

- A tight patch focused on removing startup-only keys from runtime config mutation/query surfaces.
- Also narrows `config list` output to runtime-mutable sections.

### `runtime-incubation-openai`

- Result: success
- Verify: success
- Commit: `b3fe3cc06f330a851d752d933c4c5f1a4d76c602`
- PR: `#480`
- Changed files:
  - `src/config.rs`
  - `src/daemon.rs`
  - `src/daemon/service.rs`
  - `src/http.rs`
  - `tests/support/http_routes.rs`
- Tokens:
  - input: `19,638,032`
  - output: `205,044`
- Tool calls: `309`
- Duration: `986,316 ms`

Shape:

- A fuller contract realization.
- Removes startup-only keys from runtime mutation/query surfaces.
- Also splits `/control/runtime/status` into:
  - `startup_surface`
  - `runtime_surface`
  - `agent_model_overrides`
- Preserves compatibility when older persisted `config.json` files still contain legacy startup-only sections.

## Comparison

### Main conclusion

- `codex-openai` is the better minimal patch.
- `runtime-incubation-openai` is the better issue-complete patch.

`#370` is not only about hiding startup-only keys from runtime mutation. It also asks for a clean lifecycle split across startup-only settings, runtime-mutable config, and agent-scoped overrides, with operator-facing status surfaces reflecting that split.

Against that contract:

- `#481` is smaller, faster, and easier to merge.
- `#480` tracks the issue intent more closely because it also upgrades the status surface.

### Selection

Preferred PR: `#480`

Reason:

- It better satisfies the full configuration-surface contract described in `#370`.
- `#481` should be treated as the tighter fallback if a minimal merge is preferred over the fuller lifecycle/status treatment.

## Operational notes

- This rerun was important because earlier `#370` attempts were invalidated by benchmark harness interruption.
- For this rerun, `runtime-incubation-openai` used a binary built inside the benchmark worktree, so the sample is clean with respect to base/binary alignment.
- `#480` currently has:
  - one CI failure caused by GitHub Actions billing, not by repository tests
  - three Copilot review comments that still need follow-up

## Follow-up analysis

### What this run does and does not prove

This rerun is a valid `#370` head-to-head result, but it should not be used as evidence for the later compaction-checkpoint throttling changes.

The base commit is `55b03ffc50669917615d4d3398c621b7874ce3a4`, which includes:

- `#475` progress-reporting prompt guidance
- `#476` waiting/delivery semantics regression suite

It does not include:

- `#479` full/delta compaction checkpoint throttling
- `#482` follow-up test-name cleanup

Therefore this run can support the conclusion that pre-public runtime already became capable of converging on `#370` after the progress-reporting prompt work. It cannot answer whether the later compaction checkpoint throttling fixed repeated checkpoint output.

### pre-public runtime convergence improved, but efficiency is still poor

The important positive signal is that `runtime-incubation-openai` did not remain in pure read-only exploration. It formed a work item, created a plan, modified code, added tests, ran verification, and produced a usable PR.

The cost profile is still much worse than Codex:

- input tokens: `19,638,032` vs `7,775,062` (`~2.5x`)
- output tokens: `205,044` vs `44,428` (`~4.6x`)
- tool calls: `309` vs `121` (`~2.6x`)
- duration: `986,316 ms` vs `471,982 ms` (`~2.1x`)

The `runtime-incubation-openai` run also emitted `23` `turn_local_compaction_applied` events. Its transcript contains many repeated `Progress checkpoint` messages around compaction boundaries. That means the earlier checkpoint prompt avoided context loss enough for convergence, but it also produced a checkpoint storm in this base.

This matches the motivation for the later `#479` change: keep the first checkpoint full, then use concise delta checkpoints when the compaction anchor has not materially changed.

### PR selection remains `#480`

The original selection should stand:

- `#481` is the better minimal patch.
- `#480` is the better issue-complete patch.

The issue text explicitly asks that operator-facing status and inspect surfaces make startup settings, runtime config, and agent overrides distinguishable. `#481` removes startup-only keys from config mutation/query paths, but does not upgrade the status surface. `#480` does, so it better satisfies the full `#370` contract.

### Copilot review comments on `#480`

The three Copilot comments on `#480` are worth addressing before merge:

- `control_auth_mode` should not be serialized from Rust `Debug` output. It should use a stable enum or explicit string mapping.
- `runtime_surface.model_overrides` should be sorted because it is derived from a `HashMap` and is part of an operator-facing status payload.
- The legacy config warning should say that old startup-only sections are not represented in runtime config and will be stripped on the next runtime config write, not merely "ignored".

These are not fundamental design objections to `#480`; they are small polish/stability fixes.

At the time of this analysis, another agent was already working on those `#480` review fixes, so this note does not take over that branch.

### Implication for `#470`

`#470` should remain open for now.

This rerun shows that pre-public runtime can converge on `#370`, but it does not close the broader compaction regression:

- the run predates `#479`/`#482`
- it still shows repeated progress checkpoint output
- it is not the original `#454`-style turn-local compaction regression

Close `#470` only after rerunning a `#454`-style compaction regression on a base that includes `#479`/`#482`, and after confirming that repeated turn-local compaction preserves task-progress signal without producing another exploration loop or checkpoint storm.

## Artifact paths

- `runtime-incubation-openai`
  - `/Users/jolestar/opensource/src/github.com/holon-run/runtime-incubation/.benchmark-results/openai-live-refresh-2026-04-25-0370-rerun/runtime-incubation-0370-runtime-configuration-surface/runtime-incubation-openai/run-01`
- `codex-openai`
  - `/Users/jolestar/opensource/src/github.com/holon-run/runtime-incubation/.benchmark-results/openai-live-refresh-2026-04-25-0370-rerun/runtime-incubation-0370-runtime-configuration-surface/codex-openai/run-01`
- Orchestrator log
  - `/Users/jolestar/opensource/src/github.com/holon-run/runtime-incubation/.benchmark-logs/0370-rerun.log`
