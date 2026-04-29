# Benchmark Report - 2026-04-17 - #53 and #160

## Scope

This note records the live benchmark round for:

- `#53` multi-provider routing and auth resolution coverage
- `#160` daemon module split cleanup

The intended runner matrix was:

- `runtime-incubation-claude`
- `runtime-incubation-openai`
- `codex-openai`
- `claude-sdk`

In practice:

- `#53` produced comparable benchmark outcomes for `runtime-incubation-claude`, `runtime-incubation-openai`, and `codex-openai`
- `#160` produced comparable outcomes for `runtime-incubation-claude`, `runtime-incubation-openai`, and `codex-openai`
- `claude-sdk` was additionally exercised on `#160`, but did not complete

## Final Keep Set

Retain:

- `#170` for `#53`
- `#174` for `#160`

Do not retain:

- `#172` for `#53`
- `#173` for `#160`
- `#177` for `#160`

## Case Results

### `#53`

Candidates:

- `#170` `[bench][runtime-incubation-openai][#53] Tests: cover multi-provider routing and auth resolution`
- `#172` `[bench][codex-openai][#53] Tests: cover multi-provider routing and auth resolution`
- `runtime-incubation-claude` grounded no-op

Outcome:

- Keep `#170`
- Close `#172`
- Treat `runtime-incubation-claude` as a grounded no-op, not a useful PR candidate

Rationale:

- `#170` and `#172` were very close in code quality
- both passed the verifier
- both expanded beyond the sharpest `#53` scope by adding broad `src/auth.rs` helper tests
- both also locked in the current `provider_doctor` duplicate-fallback behavior
- the final operator keep decision was to retain `#170`, while recognizing the pair was effectively a near tie

### `#160`

Candidates:

- `#173` `[bench][runtime-incubation-claude][#160] Refactor: split daemon.rs into lifecycle, probe, state, and logs modules`
- `#174` `[bench][codex-openai][#160] Refactor: split daemon.rs into lifecycle, probe, state, and logs modules`
- `#177` `[bench][runtime-incubation-openai][#160] Refactor: split daemon.rs into focused submodules [agent: runtime-incubation-openai]`
- `claude-sdk` local-only incomplete attempt

Outcome:

- Keep `#174`
- Close `#173`
- Close `#177`
- Do not retain the `claude-sdk` sample

Rationale:

- `#173` never reached a mergeable quality bar and was not comparable
- `#177` had cleaner-looking module boundaries, but it introduced public-surface drift and dropped too much daemon behavior coverage for a behavior-preserving refactor benchmark
- `#174` preserved behavior more credibly:
  - it kept a stronger daemon-focused test surface
  - it avoided the same level of public API expansion seen in `#177`
  - it passed engineering gates cleanly and was the safest keep candidate for `#160`

## Runner Summary

### `runtime-incubation-claude`

- `#53`: grounded no-op
- `#160`: produced `#173`, but failed to reach a keepable state

### `runtime-incubation-openai`

- `#53`: produced `#170`, final keep
- `#160`: produced `#177`, but lost the final comparison to `#174`

Important note:

- the first `#160` `runtime-incubation-openai` result was a false no-op caused by private-issue fetch failure
- the task had to be continued with explicit `gh issue view 160 --repo holon-run/runtime-incubation`
- the run also hit a runtime-side `Sleep` tool schema contract bug before the final implementation was completed

### `codex-openai`

- `#53`: produced `#172`, technically near-tie but not kept
- `#160`: produced `#174`, final keep

### `claude-sdk`

- `#160`: did not complete
- the original run hit max-turn failure
- a manual continuation on the same worktree still did not converge to a compile-clean state

## Benchmark Conclusions

### Implementation outcome

Across the two retained benchmark outcomes:

- `runtime-incubation-openai` wins `#53`
- `codex-openai` wins `#160`

So this round ends as a split decision between `runtime-incubation-openai` and `codex-openai`.

### What this round exposed

1. Private-repo issue fetch matters for live tasks.
- benchmark prompts that rely on GitHub issue URLs are not sufficient for private repos unless the agent is explicitly guided to use `gh issue view`

2. `runtime-incubation-openai` still had runtime friction unrelated to the task itself.
- the `Sleep` tool schema contract bug interrupted a real benchmark continuation on `#160`

3. `claude-sdk` still lacks a practical resume path in this harness.
- unlike Codex or pre-public runtime continuation flows, it could only be continued by starting a new run against the same worktree

4. For behavior-preserving refactor tasks, module shape alone is not enough.
- preserved tests and unchanged public surface matter more than having the tidiest decomposition

## Final Status

At the end of this round:

- `#170` remains the retained benchmark result for `#53`
- `#174` remains the retained benchmark result for `#160`
- `#172`, `#173`, and `#177` were closed
- `#174` has been moved from draft to ready for review
