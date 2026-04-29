# pre-public runtime benchmark PR comparison review - 2026-04-16

## Scope

This note records the implementation comparison for the current benchmark PR pairs covering:

- `#139` token usage status persistence and contract docs
- `#136` TUI markdown rendering correctness and caching
- `#131` daemon logs tail semantics and lifecycle hints
- `#123` TUI chat scroll edge cases

The goal is to decide which implementation should be kept for each issue based on code shape, contract correctness, and operator-facing behavior.

## Review method

The comparison here is based on the PR diffs themselves, not just CI state.

Evaluation criteria:

1. does the implementation solve the right layer of the problem
2. is the contract shape coherent and durable
3. does it avoid introducing new correctness risk
4. does it keep the operator-facing behavior aligned with the issue goal
5. are tests added at the right boundary

## Decisions

### `#139` token usage status persistence

Candidates:

- `#156` `runtime-incubation-openai`
- `#157` `codex-openai`

Decision:

- keep `#157`
- do not keep `#156`

Why:

1. `#157` uses a single persisted `Option<TokenUsage>` for `last_turn`
- this is a cleaner contract than splitting the same semantic value into parallel `last_turn_input_tokens` and `last_turn_output_tokens`
- it avoids pair-field drift risk and keeps the persisted shape closer to the operator-facing API

2. `#157` has better test coverage
- it adds runtime-flow coverage for transcript-windowing survival
- it also adds persistence coverage in worktree storage tests
- this is the right level for a contract-fix issue

3. `#157` updates the spec more completely
- it also closes the `ProviderAttemptRecord.token_usage` documentation gap while touching the same contract surface

Why not `#156`:

- it works in principle, but the model is more fragmented
- it does not carry equivalent regression coverage
- it is the weaker persistence contract of the two

### `#136` TUI markdown rendering correctness and caching

Candidates:

- `#154` `codex-openai`
- `#158` `runtime-incubation-openai`

Decision:

- keep `#154`
- do not keep `#158`

Why:

1. `#154` has the cleaner fence-rendering model
- fence behavior is made consistent at both the opening and closing side
- its tests reflect the intended rendering contract directly

2. `#154` keeps the cache implementation inside the TUI surface without introducing new export/API problems
- the cache shape is coarse, but stable
- it avoids widening public surface just to support a manual example

3. `#158` introduces real correctness and hygiene issues
- Copilot's fence comment is valid: the opening and closing fence conditions diverge
- `examples/test_markdown_render.rs` does not fit the current visibility/export contract and would not be a clean ship shape
- cache testing is still underpowered relative to the new behavior

Why not `#158`:

- it mixes a useful cache idea with an unstable fence implementation and an invalid example entry point
- this makes it a worse implementation even before considering CI

### `#131` daemon logs tail semantics and lifecycle hints

Candidates:

- `#155` `codex-openai`
- `#162` `runtime-incubation-openai`

Decision:

- keep `#155`
- do not keep `#162`

Why:

1. `#155` preserves the correct product contract for `--tail 0`
- it treats `--tail 0` as a bounded default tail, not as “omit the tail entirely”
- that is closer to the actual operator expectation behind this issue

2. `#155` improves the message contract while keeping logs useful
- the output remains directly actionable
- the message makes boundedness explicit and tells the operator how to request more

3. `#155` also includes the config-fingerprint mismatch `daemon logs` hint
- that is a real operator-loop improvement aligned with the issue

Why not `#162`:

- it changes `--tail 0` into “omit tail”, which is a contract shift away from the issue goal
- its streaming/bounded-reading direction is technically reasonable, but it solves the wrong product behavior first

Note:

- `#155` still carries rough test/script shape
- but compared to `#162`, it is still the better implementation because the core daemon logs contract is more correct

### `#123` TUI chat scroll edge cases

Candidates:

- `#153` `runtime-incubation-openai`
- `#163` `codex-openai`

Decision:

- keep `#163`
- do not keep `#153`

Why:

1. `#163` fixes the state machine at the right place
- it passes `max_scroll` into scroll handling
- that lets the Home -> Down/PageDown path normalize immediately and deterministically
- this is a direct state-model fix rather than a deferred correction layer

2. `#163` also fixes Unicode display width at the counting layer
- it switches to `unicode-width`
- that is the right fix for wrapped-height estimation in this context

3. `#153` relies on a delayed normalization mechanism
- `pending_normalization` is a patch-style repair, not a clean state model
- the Copilot comments about follow-tail normalization and clamping are valid
- it also carries scratch test artifacts that should not be part of the shipping diff

Why not `#153`:

- it is more complex for a weaker result
- the normalization approach is more fragile than simply using `max_scroll` where the transition is computed

## Final keep-set

The implementations that should be kept are:

1. `#157` for `#139`
2. `#154` for `#136`
3. `#155` for `#131`
4. `#163` for `#123`

## Summary

Across these four benchmark groups, the selected versions are the ones that:

- fix the semantic layer directly instead of masking symptoms
- preserve or improve the operator-facing contract
- avoid introducing avoidable public-surface or state-model complexity
- add tests closer to the actual correctness boundary
