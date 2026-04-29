# pre-public runtime benchmark final result - 2026-04-16

## Scope

This note records the final keep-set for the current live benchmark round after:

- phase-1 implementation runs
- phase-2 resume/follow-up runs where needed
- cross-PR implementation review
- final PR triage and candidate closure

The benchmark groups covered are:

- `#139` token usage status persistence and contract docs
- `#136` TUI markdown rendering correctness and caching
- `#131` daemon logs tail semantics and lifecycle hints
- `#123` TUI chat scroll edge cases

## Final keep-set

The implementations kept for landing are:

1. `#157` for `#139`
2. `#154` for `#136`
3. `#155` for `#131`
4. `#163` for `#123`

The competing benchmark candidates that were closed are:

- `#156` in favor of `#157`
- `#158` in favor of `#154`
- `#162` in favor of `#155`
- `#153` in favor of `#163`

## Outcome

All four retained implementations are from `codex-openai`.

This does not mean every intermediate benchmark score favored Codex at every
step. It means that after follow-up continuation, implementation review, and
final keep/close decisions, the versions chosen to preserve were:

- `codex-openai` for all four issue groups

## Why these four were kept

### `#139`

- `#157` uses the cleaner persisted model for `last_turn`
- it adds stronger regression coverage
- it keeps the runtime contract and docs more coherent than `#156`

### `#136`

- `#154` has the cleaner fence-rendering behavior
- it avoids the invalid example/export shape introduced in `#158`
- it is the better ship candidate for the markdown/cache follow-up

### `#131`

- `#155` stays closer to the intended daemon logs operator contract
- it keeps the `daemon logs` hint behavior aligned with the issue goal
- `#162` had more review debt and solved the wrong product behavior first

### `#123`

- `#163` fixes the chat scroll state transition directly
- it uses `max_scroll` at the decision point instead of deferred correction
- it also carries the Unicode display-width fix at the counting layer

## Current follow-through

The retained PRs are being tracked through AgentInbox for:

- PR review/comment events
- CI status changes

The next step is to clear remaining CI/review debt on:

- `#154`
- `#155`
- `#157`

`#163` is already in a better state and remains part of the keep-set.
