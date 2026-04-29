# Benchmark `#483` HTTP Routes Support Modules

Date: 2026-04-27

Suite: `anthropic-live-refresh-2026-04-27-0483`

Label: `anthropic-live-refresh-2026-04-27-0483-2026-04-27T04-28-06-554Z`

Base: `dcbb4808e69d15f0c823370edca0dc9b71cd1c6c`

Issue: `#483` Split `tests/support/http_routes.rs` into surface-specific support modules

## Artifacts

- `runtime-incubation-anthropic`:
  `/Users/jolestar/opensource/src/github.com/holon-run/runtime-incubation/.benchmark-results/anthropic-live-refresh-2026-04-27-0483-2026-04-27T04-28-06-554Z/runtime-incubation-0483-http-routes-support-modules/runtime-incubation-anthropic/run-01`
- `claude-cli`:
  `/Users/jolestar/opensource/src/github.com/holon-run/runtime-incubation/.benchmark-results/anthropic-live-refresh-2026-04-27-0483-2026-04-27T04-28-06-554Z/runtime-incubation-0483-http-routes-support-modules/claude-cli/run-01`

## PRs

- `#539` `runtime-incubation-anthropic`: https://github.com/holon-run/runtime-incubation/pull/539
- `#538` `claude-cli`: https://github.com/holon-run/runtime-incubation/pull/538

Both PRs were opened as draft PRs. The benchmark artifact `pr.json` records
`skipped_no_changes` for both runners, but GitHub shows the PRs were created.
This is a benchmark framework reporting bug, not a missing PR.

## Headline Result

`#539` from `runtime-incubation-anthropic` is the better artifact to keep if we want a
reviewable stepping stone for `#483`. It updates the HTTP facade tests to depend
on newly named surface modules and creates shims for the expected surfaces.

Neither PR fully satisfies `#483`, because neither actually moves the underlying
implementations out of `tests/support/http_routes.rs`. Both are phase-1
structural changes rather than the full split requested by the issue.

## Metrics

| Runner | Success | Verify | Duration | Input Tokens | Output Tokens | Total Tokens | Turns | Tool Calls | Changed Files |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `runtime-incubation-anthropic` | yes | yes | 759,146 ms | 3,357,170 | 34,444 | 3,391,614 | 133 | 125 | 15 |
| `claude-cli` | yes | yes | 584,652 ms | 76,278 | 33,982 | 110,260 | 86 | 85 | 3 |

pre-public runtime used about `30.8x` the total tokens and about `44.0x` the input tokens of
`claude-cli`.

## Context Management

This run enabled Anthropic context management for `runtime-incubation-anthropic`:

- `PRE-PUBLIC RUNTIME_ANTHROPIC_CONTEXT_MANAGEMENT=true`
- `PRE-PUBLIC RUNTIME_ANTHROPIC_CONTEXT_MANAGEMENT_TRIGGER_INPUT_TOKENS=30000`
- `PRE-PUBLIC RUNTIME_ANTHROPIC_CONTEXT_MANAGEMENT_KEEP_RECENT_TOOL_USES=3`

pre-public runtime token diagnostics confirm the setting was active:

- `context_management_enabled_rounds`: `133`
- `cache_read_input_tokens`: `1,945,344`
- `high_input_zero_cache_read_rounds`: `72`
- request lowering mode: `prompt_cache_blocks` for all `133` rounds

Context management was enabled, but it did not solve pre-public runtime's input-token
problem. Late rounds still reached roughly `65k-70k` input tokens, and many
large rounds had zero cache-read input tokens. The primary remaining issue is
still repeated large prompt frames and cache miss behavior, not output verbosity.

## Code Result Comparison

### `#539` `runtime-incubation-anthropic`

Commit: `7dc62228d51941b8a2fbe07889423a428d1576ec`

Diff shape:

- 15 files changed
- 120 insertions
- 21 deletions
- adds `tests/support/http_callback.rs`
- adds `tests/support/http_client.rs`
- adds `tests/support/http_control.rs`
- adds `tests/support/http_events.rs`
- adds `tests/support/http_ingress.rs`
- adds `tests/support/http_tasks.rs`
- adds `tests/support/http_workspace.rs`
- updates facade tests to use corresponding `support::http_*` modules

Assessment:

- Stronger alignment with the issue's requested surface boundaries.
- Better as a base PR, because the visible facade dependencies now reflect the
  intended module layout.
- Still incomplete: the new modules are compatibility shims that re-export
  symbols from `http_routes.rs`; the monolithic implementation remains.
- Introduces many unused-import warnings when running narrow verifier commands,
  because shim modules re-export more functions than each facade uses.

### `#538` `claude-cli`

Commit: `8362c0938fe722ff53d8d214e36f3161c57c30e1`

Diff shape:

- 3 files changed
- 289 insertions
- adds `tests/support/http_common.rs`
- adds `tests/support/README.md`
- updates `tests/support/mod.rs`

Assessment:

- It creates a plausible shared helper module and documentation.
- It does not update facade tests to depend on surface modules.
- It leaves all route-specific support code in `http_routes.rs`.
- Its own PR body marks most acceptance criteria incomplete.
- It is less useful as a direct `#483` solution, though parts of
  `http_common.rs` may be reusable in a later real split.

## CI Status

Both PRs failed GitHub CI only at `cargo fmt --all -- --check`.

`#538` failed formatting in `tests/support/http_common.rs` near the file end.

`#539` failed formatting due import/module ordering in the new shim modules and
`tests/support/mod.rs`.

The benchmark verifier did not catch this because the task verifier ran selected
`cargo test --test ... --quiet` commands and did not run `cargo fmt --check`.
Future benchmark manifests should include formatting when the target repo's CI
requires it, or the benchmark framework should run a repo-level standard preflight
before marking PR-ready artifacts.

## Verifier Gap

The verifier was adequate for behavioral smoke checks but too weak for PR
readiness:

- It missed formatting failures.
- It did not run full `cargo test`.
- It allowed a result that is structurally partial to be marked `success=true`.

For future implementation benchmarks, evaluate success from the artifact and
issue acceptance, not only verifier status. The verifier should remain tolerant
for product assessment, but PR readiness needs an additional CI-equivalent
preflight.

## Recommendation

Keep `#539` if we want to preserve the benchmark artifact and turn it into a
formal incremental PR. The next fix should run `cargo fmt`, reduce warning noise
if practical, and make the PR body explicit that this is a shim/module-boundary
step rather than the complete `http_routes.rs` implementation migration.

Close `#538` unless we specifically want to salvage `http_common.rs` into the
follow-up implementation-migration PR. As a direct artifact for `#483`, it is
too shallow.
