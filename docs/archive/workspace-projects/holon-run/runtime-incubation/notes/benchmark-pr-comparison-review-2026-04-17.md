# Benchmark PR Comparison Review - 2026-04-17

This note records the `#53` live benchmark PR comparison for:

- `#170` `[bench][runtime-incubation-openai][#53] Tests: cover multi-provider routing and auth resolution`
- `#172` `[bench][codex-openai][#53] Tests: cover multi-provider routing and auth resolution`

## Scope

Both PRs target `#53` and both land in the same files:

- `src/auth.rs`
- `src/provider/tests.rs`

Both PRs pass the benchmark verifier and both create valid benchmark PRs.

## Assessment

The two implementations are very close.

- Both add useful coverage around:
  - provider-chain deduplication and ordering
  - unavailable-provider fallback behavior
  - provider doctor availability reporting
- Both also expand into a broad `src/auth.rs` helper-test block that is only loosely connected to the core `#53` benchmark target.
- Both lock in the current mismatch where `provider_doctor.fallback_models` preserves duplicates while the effective provider chain deduplicates them. That is a real current behavior, but it is not obviously the contract we want to preserve forever.

So the code-review conclusion is:

- `#170` vs `#172` is effectively a near tie
- if forced to choose on implementation quality alone, `#172` has a very slight edge
- but the edge is small enough that this is not a decisive win

## Final Keep Decision

For this benchmark round, the keep/close decision is:

- keep `#170`
- close `#172`

This is an operator choice, not a claim that `#170` clearly dominates `#172` on technical quality.

## Additional Note

`runtime-incubation-claude` on `#53` produced a grounded no-op result: it inspected the existing provider/config test coverage, ran the verifier successfully, and concluded that the issue was already substantially covered on `main`.
