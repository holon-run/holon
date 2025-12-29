# Holon

English|[中文](README.zh.md)

Holon runs AI coding agents headlessly to turn issues into PR-ready patches and summaries — locally or in CI, without babysitting the agent.

## Why Holon
- Headless by default: run AI coding agents end-to-end without TTY or human input; deterministic, repeatable runs.
- Issue → PR, end to end: fetch context, run the agent, and create or update a PR in one command.
- Patch-first, standardized artifacts: always produce `diff.patch`, `summary.md`, and `manifest.json` for review and CI.
- Sandboxed execution: Docker + snapshot workspaces by default; nothing touches your repo unless you opt in.
- Pluggable agents & toolchains: swap agent engines or bundles without changing your workflow.
- Local or CI, same run: `holon solve` locally or in GitHub Actions with identical inputs and outputs.

## GitHub Actions quickstart (with holonbot)
1) Install the GitHub App: [holonbot](https://github.com/apps/holonbot) in your repo/org.  
2) Add a trigger workflow (example minimal setup):

```yaml
name: Holon Trigger

on:
  issue_comment:
    types: [created]
  issues:
    types: [labeled, assigned]
  pull_request:
    types: [labeled]

permissions:
  contents: write
  issues: write
  pull-requests: write
  id-token: write

jobs:
  holon:
    name: Run Holon (via holon-solve)
    uses: holon-run/holon/.github/workflows/holon-solve.yml@main
    with:
      issue_number: ${{ github.event.issue.number || github.event.pull_request.number }}
      comment_id: ${{ github.event.comment.id }}
    secrets: inherit
```

3) Set secret `ANTHROPIC_AUTH_TOKEN`. `holon-solve` will derive mode/context/output dir from the event and run the agent headlessly.

## Local CLI (`holon solve`)
Prereqs: Docker, Anthropic token (`ANTHROPIC_AUTH_TOKEN`), GitHub token (`GITHUB_TOKEN` or `HOLON_GITHUB_TOKEN` or `gh auth login`), optional base image (auto-detects from repo).

Install:
- Homebrew: `brew install holon-run/tap/holon`
- Or download a release tarball from GitHub and place `holon` on your `PATH`.

Run against an issue or PR (auto collect context → run agent → publish results):
```bash
export ANTHROPIC_AUTH_TOKEN=...
export GITHUB_TOKEN=...   # or use gh auth login

holon solve https://github.com/owner/repo/issues/123
# or: holon solve owner/repo#456
```

Behavior:
- Issue: creates/updates a branch + PR with the patch and summary.
- PR: applies/pushes the patch to the PR branch and posts replies when needed.


## Development & docs
- Build CLI: `make build`; test: `make test`; agent bundle: `(cd agents/claude && npm run bundle)`.
- Design/architecture: `docs/holon-architecture.md`
- Agent contract: `rfc/0002-agent-scheme.md`
- Modes: `docs/modes.md`
- Contributing: see `CONTRIBUTING.md`
