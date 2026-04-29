This directory stores live head-to-head benchmark inputs for the `runtime-incubation` repository.

These manifests live outside the tested repository on purpose so agents cannot recover the task goal by reading benchmark task files from the workspace under test.

Current layout:

- `tasks/<task_id>.yaml`
- `suites/<suite_id>.yaml`

Current benchmark source repo:

- `/Users/jolestar/opensource/src/github.com/holon-run/runtime-incubation`

Current benchmark wave base:

- `origin/main` at `cd05ec1d61ed1f2e9a46a9499a48770f3294f9c7`

Usage notes:

- Benchmark runner-created code changes should land in per-run worktrees, not in the canonical repo checkout.
- The refreshed issue-driven live suite for the 2026-04-27 wave is:
  - `suites/anthropic-live-refresh-2026-04-27-0483.yaml`
  - `suites/anthropic-live-refresh-2026-04-27-0484.yaml`
- Current recommended first pass is `anthropic-live-refresh-2026-04-27-0483`, with
  `PRE-PUBLIC RUNTIME_ANTHROPIC_CONTEXT_MANAGEMENT` enabled for the `runtime-incubation-anthropic`
  runner.
- The earlier 2026-04-24b suite is retained for audit:
  - `suites/openai-live-refresh-2026-04-24-453-454.yaml`
- The earlier 2026-04-24 `369/370` suite is retained for audit:
  - `suites/openai-live-refresh-2026-04-24-369-370.yaml`
- The 2026-04-23 refresh suites are retained for audit:
  - `suites/openai-live-refresh-2026-04-23-core.yaml`
  - `suites/openai-live-refresh-2026-04-23-contracts.yaml`
  - `suites/openai-live-refresh-2026-04-23-all.yaml`
- Historical 2026-04-15 / 2026-04-18 suites are retained for audit, but should not be used as the current live wave unless explicitly refreshed.
- The earlier test-coverage suite is `suites/openai-test-coverage-live-2026-04-15.yaml`.
- The earlier product-oriented phase-2 live suites are:
  - `suites/openai-phase2-live-2026-04-15.yaml`
  - `suites/openai-phase2-followups-2026-04-15.yaml`
- The earlier phase-3 suite that exercises same-session review-fix resume is:
  - `suites/openai-phase3-live-2026-04-16.yaml`
- The earlier mixed live suite is:
  - `suites/openai-live-0053-0160-2026-04-16.yaml`
- For live implementation tasks, prefer `scope_policy: soft` without `allowed_paths`; reserve hard path allowlists for constrained follow-up / review-fix tasks.
