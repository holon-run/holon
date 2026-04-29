# Benchmark PR comparison review - 2026-04-17 - daemon split (#160)

## Scope

Compare the three benchmark PRs for `#160`:

- `#173` `[bench][runtime-incubation-claude][#160] Refactor: split daemon.rs into lifecycle, probe, state, and logs modules`
- `#174` `[bench][codex-openai][#160] Refactor: split daemon.rs into lifecycle, probe, state, and logs modules`
- `#177` `[bench][runtime-incubation-openai][#160] Refactor: split daemon.rs into focused submodules`

Goal: choose the version that best satisfies the intended task boundary for `#160`:

- module split cleanup
- behavior-preserving refactor
- no public API drift unless explicitly intended
- no loss of important daemon behavior coverage

## Decision

Keep:

- `#174`

Do not keep:

- `#173`
- `#177`

## Ranking

1. `#174`
2. `#177`
3. `#173`

## Review notes

### `#173`

Conclusion: eliminate.

Reasoning:

- CI failed and the PR never reached a comparable quality bar.
- There was no follow-up that moved it into a mergeable state.
- It does not provide a stronger implementation case than the other two candidates.

### `#174`

Conclusion: best current implementation.

Why it wins:

1. It preserves behavior more credibly.
- The split stays centered on existing daemon responsibilities.
- It does not expand the public daemon surface as part of the refactor.

2. It preserves test coverage better.
- A large daemon-focused test surface is retained and reorganized instead of disappearing.
- This is critical for a refactor whose main claim is behavior preservation.

3. It passes the baseline engineering gates.
- `mergeStateStatus: CLEAN`
- CI `test: SUCCESS`
- No blocker-level Copilot inline comments were surfaced in the final state.

Tradeoff:

- The module taxonomy is a little less aesthetically tidy than `#177`.
- But this is an acceptable tradeoff because this task is primarily about preserving behavior, not maximizing naming elegance.

### `#177`

Conclusion: structurally promising, but not acceptable as the chosen version.

Why it is not selected:

1. It changes the public surface.
- Several helpers that were previously not part of the public daemon API became publicly re-exported.
- That is API drift, not a pure module split.

2. It drops too much daemon behavior coverage.
- log tail semantics / `--tail 0`
- persisted failure load/clear paths
- status decoding without `activity`
- runtime activity summary states
- unix probe non-socket behavior
- foreign/incompatible socket stop behavior

For a refactor issue in the scope of `#160`, losing those tests makes the behavior-preserving claim too weak.

What `#177` does well:

- Its module boundaries are visually cleaner:
  - `lifecycle`
  - `logs`
  - `probe`
  - `state`
  - `status`
- If this were a design-only comparison, it would score well on decomposition shape.

Why that still does not win:

- Better-looking boundaries do not outweigh API drift plus reduced test confidence.

## Final recommendation

For `#160`, retain `#174` as the benchmark winner.

Rationale:

- It is the version most aligned with the intended evaluation standard for this task:
  - behavior-preserving refactor
  - no unnecessary API expansion
  - better preservation of daemon behavior coverage

`#177` is the closest alternative, but should not be selected unless it first restores the lost test coverage and removes the public-surface drift.
