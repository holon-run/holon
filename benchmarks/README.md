# Real-Repo Benchmark Tasks

This directory contains repo-local benchmark examples and replay inputs.

Real-repo benchmarks are modeled as issue-driven operator assignments:

- Runner input is built from a shared issue template, not from a task-specific long prompt body.
- The template tells agents to use `gh` to inspect the issue and related GitHub context.
- Verification and scope rules remain defined in manifest metadata, with `evaluation.scope_policy` used during result scoring.
- Exact verifier commands remain benchmark-only and are not injected verbatim into the runner prompt.
- `benchmark.mode` distinguishes `live` from `replay`.
- `evaluation.scope_policy` defaults to a diagnostic metric when set to `soft`, and only becomes a hard failure for constrained tasks when set to `hard`.
- `evaluation.expected_outcome` defines whether a task requires a diff, expects a grounded no-op, or allows either.
- Suite `pr` config defines publish behavior structurally:
  - `submit_pr`
  - `draft_pr`
  - `push_branch`
- The runner renders PR policy into natural language in the issue template.
- Live head-to-head runs currently compare:
  - `holon-openai`
  - `codex-openai`
- These two runners are executed in parallel for each task; tasks themselves still advance in suite order.
- `holon-openai` live benchmark runs now set `HOLON_DISABLE_PROVIDER_FALLBACK=1` so deterministic live comparisons do not silently switch to a fallback provider/model.
- Codex live runs now use the configured or default shared `CODEX_HOME`/user environment by default rather than an isolated benchmark-specific home.

Live head-to-head benchmark tasks are intentionally kept outside the tested repository so agents cannot recover the task goal by reading benchmark manifests from the workspace. In this environment they live under:

- `workspace/projects/holon-run/holon/benchmarks/`

Repo-local layout:

- `tasks/<task_id>.yaml`
- `suites/<suite_id>.yaml`

Use:

```bash
node benchmark/run.mjs validate-manifest --manifest benchmarks/tasks/holon-0015-tool-guidance-registry.yaml
node benchmark/run.mjs real --manifest /absolute/path/to/workspace/projects/holon-run/holon/benchmarks/tasks/holon-0050-runtime-result-closure.yaml --runner holon-openai --runner codex-openai --label bench-live-0050
node benchmark/run.mjs suite --suite benchmarks/suites/openai-phase1.local.yaml --label bench-openai-phase1
```

To push benchmark branches and create draft PRs, either:

- set `pr.submit_pr: true` and `pr.draft_pr: true` in the suite file, or
- override on the CLI with `--push-branch --github-pr`.
