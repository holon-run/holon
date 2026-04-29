# Benchmark result for `#161` - 2026-04-16

## Scope

This note records the dedicated benchmark run for:

- `#161` `Refactor: split provider.rs into transport, fallback, retry, and diagnostics modules`

The benchmark was run as a three-way comparison:

- `codex-openai`
- `runtime-incubation-openai`
- `runtime-incubation-claude`

The benchmark task manifest is:

- [runtime-incubation-0161-provider-module-split.yaml](/Users/jolestar/opensource/src/github.com/jolestar/workspace/projects/holon-run/runtime-incubation/benchmarks/tasks/runtime-incubation-0161-provider-module-split.yaml)

## Outcome

Final outcome for this round:

- `codex-openai`: pass
- `runtime-incubation-openai`: fail
- `runtime-incubation-claude`: fail to converge

This is not a symmetric same-model comparison. It is a practical three-run comparison using the currently runnable benchmark paths.

## Runner results

### `codex-openai`

`codex-openai` completed the refactor and passed verifier.

Key result:

- [summary.json](/Users/jolestar/opensource/worktrees/github.com/holon-run/runtime-incubation/benchmark-phase3-2026-04-16/.benchmark-results/bench-0161-codex-openai/runtime-incubation-0161-provider-module-split/codex-openai/run-01/summary.json)

Verifier output:

- [verify.log](/Users/jolestar/opensource/worktrees/github.com/holon-run/runtime-incubation/benchmark-phase3-2026-04-16/.benchmark-results/bench-0161-codex-openai/runtime-incubation-0161-provider-module-split/codex-openai/run-01/verify.log)

Observed shape of the kept implementation:

- split under `src/provider/`
- transport-specific code moved under `src/provider/transports/`
- fallback / retry / diagnostics separated into narrower modules

Changed files recorded by benchmark:

- `src/provider.rs`
- `src/provider/catalog.rs`
- `src/provider/diagnostics.rs`
- `src/provider/fallback.rs`
- `src/provider/mod.rs`
- `src/provider/retry.rs`
- `src/provider/tests.rs`
- `src/provider/transports/anthropic.rs`
- `src/provider/transports/mod.rs`
- `src/provider/transports/openai.rs`

### `runtime-incubation-openai`

`runtime-incubation-openai` did not reach implementation. It failed immediately on provider transport contract.

Key result:

- [summary.json](/Users/jolestar/opensource/worktrees/github.com/holon-run/runtime-incubation/benchmark-phase3-2026-04-16/.benchmark-results/bench-0161-runtime-incubation-openai/runtime-incubation-0161-provider-module-split/runtime-incubation-openai/run-01/summary.json)

Runtime failure:

- [runner-initial.log](/Users/jolestar/opensource/worktrees/github.com/holon-run/runtime-incubation/benchmark-phase3-2026-04-16/.benchmark-results/bench-0161-runtime-incubation-openai/runtime-incubation-0161-provider-module-split/runtime-incubation-openai/run-01/runner-initial.log)

The failure was:

- `openai-codex/gpt-5.4`
- `400 Bad Request`
- `{"detail":"Unsupported parameter: max_output_tokens"}`

So this result should be interpreted as:

- transport path blocked
- not an implementation-quality loss

### `runtime-incubation-claude`

`runtime-incubation-claude` made substantial progress on the refactor, but it did not converge within a reasonable benchmark window and was stopped manually.

Current agent state at stop time:

- [agent.json](/Users/jolestar/opensource/worktrees/github.com/holon-run/runtime-incubation/benchmark-phase3-2026-04-16/.benchmark-results/bench-openai-live-0161-2026-04-16-tri/runtime-incubation-0161-provider-module-split/runtime-incubation-claude/run-01/runtime-incubation-home/agents/runtime-incubation-0161-provider-module-split/agent.json)

At stop time it had:

- `status = awake_running`
- `total_model_rounds = 93`
- repeated compile/debug loops

Representative verifier error:

- [be0b10af-f57b-46b3-ae77-c4470dff9c4a.log](/Users/jolestar/opensource/worktrees/github.com/holon-run/runtime-incubation/benchmark-phase3-2026-04-16/.benchmark-results/bench-openai-live-0161-2026-04-16-tri/runtime-incubation-0161-provider-module-split/runtime-incubation-claude/run-01/runtime-incubation-home/agents/runtime-incubation-0161-provider-module-split/task-output/be0b10af-f57b-46b3-ae77-c4470dff9c4a.log)

The concrete error at that point was:

- unresolved imports from `crate::types`

The partial implementation shape was still meaningful:

- `src/provider/anthropic.rs`
- `src/provider/catalog.rs`
- `src/provider/codex.rs`
- `src/provider/doctor.rs`
- `src/provider/fallback.rs`
- `src/provider/mod.rs`
- `src/provider/openai.rs`
- `src/provider/retry.rs`
- `src/provider/stub.rs`
- `src/provider/types.rs`

But benchmark-wise, this run should be scored as non-converged.

## Takeaways

There are two different conclusions here.

Implementation comparison:

- `codex-openai` is the only run that both completed the refactor and passed verifier

Framework / runtime signals:

- `runtime-incubation-openai` is blocked by a concrete `openai-codex` transport contract mismatch
- `runtime-incubation-claude` can make large structural progress, but on this task it did not close the loop cleanly enough to finish

## Next actions

- Keep the `codex-openai` result as the benchmark winner for `#161`
- Track the `runtime-incubation-openai` transport mismatch as a standalone issue
- Treat the `runtime-incubation-claude` result as an agent-policy / convergence signal, not as a transport issue
