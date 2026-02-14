# holon solve Reliability Matrix

This document defines the release-blocker reliability matrix for `holon solve`.

## Scope

- Command under test: `holon solve` (`issue` and `pr` subcommands)
- Environment: deterministic integration tests with mock agent driver
- Out of scope: `holon serve`-only proactive workflows

## Matrix Scenarios

| ID | Scenario | Expected behavior | Test |
|---|---|---|---|
| S1 | Issue -> code change -> PR-oriented output | Command succeeds, writes `manifest.json`, `diff.patch`, `summary.md`, and exits predictably | `tests/integration/testdata/solve-matrix-issue-flow.txtar` |
| S2 | PR fix flow (review/check remediation path) | Command succeeds with deterministic code change artifacts and success manifest | `tests/integration/testdata/solve-matrix-pr-fix-flow.txtar` |
| S3 | No-change / no-diff | Command succeeds, manifest indicates success, and diff artifact remains empty or minimal | `tests/integration/testdata/solve-matrix-no-diff.txtar` |
| S4 | Base/ref divergence or invalid workspace ref | Command fails during workspace preparation with actionable diagnostics | `tests/integration/testdata/solve-matrix-workspace-ref-failure.txtar` |
| S5 | Permission/auth missing (`GITHUB_TOKEN`) | Command fails fast before runtime with explicit auth error | `tests/integration/testdata/solve-matrix-auth-failure.txtar` |

## Failure Classification

| Class | Typical signal | Retry policy | Operator action |
|---|---|---|---|
| Auth required | `GITHUB_TOKEN ... is required` | Non-retryable until credentials change | Set `GITHUB_TOKEN`/`HOLON_GITHUB_TOKEN` or complete `gh auth login` |
| Workspace/ref invalid | `failed to prepare workspace` / git clone-ref errors | Non-retryable until input changes | Fix `--workspace-ref`, base branch, or repo/ref selection |
| Runtime/preflight transient | Docker daemon/network transient failures | Retryable | Retry after daemon/network recovery |
| Agent execution failure | `holon execution failed` with failing manifest/outcome | Depends on root cause | Check `summary.md`, logs, and rerun after fix |
| Publish validation failure | malformed manifest or explicit failed outcome | Non-retryable until output fixed | Fix skill/agent output contract and rerun |

## CI Execution

- These scenarios run as part of `make test-integration`.
- CI job: `.github/workflows/ci.yml` -> `test-integration` -> `make test-integration-artifacts`.
- Release readiness requires matrix tests green in CI.
