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
- Runner IDs are suite-defined unique lowercase slugs. Optional `transport` and
  `endpoint` fields label route-specific comparisons.
- Suites may set `repetitions` and deterministic `execution` controls:
  - `runner_order: configured | paired_randomized | alternating`
  - `random_seed`
  - `max_parallel_runners`
  - `cooldown_ms`
- Every task/repetition is one paired group. Paired order modes require serial
  runner execution. Run, branch, worktree, artifact,
  and Holon agent identities include the repetition so state cannot leak
  between samples.
- Non-PR runs execute the manifest verifier after the runner. PR runs continue
  to use GitHub CI as their verification source.
- Raw per-run summaries remain under each `run-NN` directory. Suites also emit
  `paired-summary.json` and `paired-summary.md`.
- `holon-openai` live benchmark runs now set `HOLON_DISABLE_PROVIDER_FALLBACK=1` so deterministic live comparisons do not silently switch to a fallback provider/model.
- Codex live runs now use the configured or default shared `CODEX_HOME`/user environment by default rather than an isolated benchmark-specific home.

Live head-to-head benchmark tasks are intentionally kept outside the tested repository so agents cannot recover the task goal by reading benchmark manifests from the workspace. In this environment they live under:

- `workspace/projects/holon-run/holon/benchmarks/`

Repo-local layout:

- `tasks/<task_id>.yaml`
- `suites/<suite_id>.yaml`

Use:

```bash
node benchmark/run.mjs validate-manifest --manifest benchmarks/tasks/holon-1611-tool-guidance-markdown.yaml
node benchmark/run.mjs validate-manifest --manifest benchmarks/tasks/holon-1764-runtime-performance-diagnostics.yaml
node benchmark/run.mjs real --manifest /absolute/path/to/workspace/projects/holon-run/holon/benchmarks/tasks/holon-0050-runtime-result-closure.yaml --runner holon-openai --runner codex-openai --label bench-live-0050
node benchmark/run.mjs suite --suite benchmarks/suites/openai-phase1.local.yaml --label bench-openai-phase1
node benchmark/run.mjs suite --suite benchmarks/suites/performance-diagnostics.local.yaml --label bench-perf-diagnostics
```

The checked-in DeepSeek pilot suite compares the two canonical routes with five
seeded, serial repetitions:

```bash
node benchmark/run.mjs suite --suite benchmarks/suites/deepseek-transport-pilot.local.yaml --label deepseek-transport-pilot
```

This command uses paid provider traffic. Run it only with operator authorization
and required credentials. The suite disables provider fallback and does not
create PRs or change the default DeepSeek route.

To push benchmark branches and create draft PRs, either:

- set `pr.submit_pr: true` and `pr.draft_pr: true` in the suite file, or
- override on the CLI with `--push-branch --github-pr`.
