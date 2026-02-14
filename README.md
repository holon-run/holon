# Holon

English|[中文](README.zh.md)

Holon runs AI coding agents headlessly (Claude Code by default) to turn issues into PR-ready patches and summaries — locally or in CI, without babysitting the agent.

Design direction: Holon is built around a sandboxed run + standardized artifacts contract, so higher-level automation (planning, ask-for-info, review/merge controllers) can be layered on over time—staged goals.

## Why Holon
- Headless by default: run AI coding agents end-to-end without TTY or human input; deterministic, repeatable runs.
- Issue → PR, end to end: fetch context, run the agent, and create or update a PR in one command.
- Patch-first, standardized artifacts: always produce `diff.patch`, `summary.md`, and `manifest.json` for review and CI.
- Sandboxed execution: Docker container runtime with direct workspace mode by default for `run` and `solve --workspace`; solve without `--workspace` still uses isolated temp workspaces.
- State persistence: optional `--state-dir` mount for cross-run skill caches (e.g., synced issues cache).
- Pluggable agents & toolchains: swap agent engines or bundles without changing your workflow.
- Local or CI, same run: `holon solve` locally or in GitHub Actions with identical inputs and outputs.

## Agents
Holon currently ships with a Claude Code agent bundle by default. You can also run other agent bundles (including custom ones) via `--agent` / `HOLON_AGENT` and select update behavior via `--agent-channel` / `HOLON_AGENT_CHANNEL`.

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
      comment_id: ${{ github.event.comment.id || 0 }}
    secrets:
      anthropic_auth_token: ${{ secrets.ANTHROPIC_AUTH_TOKEN }} # required
      anthropic_base_url: ${{ secrets.ANTHROPIC_BASE_URL }}
```

3) Set secret `ANTHROPIC_AUTH_TOKEN` (org/repo visible) and pass it via the `secrets:` map as shown. `holon-solve` will derive mode/context/output dir from the event and run the agent headlessly. Ready-to-use workflow: copy [`examples/workflows/holon-trigger.yml`](examples/workflows/holon-trigger.yml) into your repo for a working trigger.

## Local CLI (`holon solve`)
Prereqs: Docker, Anthropic token (`ANTHROPIC_AUTH_TOKEN`), GitHub token (`GITHUB_TOKEN` or `HOLON_GITHUB_TOKEN` or `gh auth login`), optional base image (auto-detects from repo).

Install:
- Homebrew: `brew install holon-run/tap/holon`
- Or download a release tarball from GitHub and place `holon` on your `PATH`.

Run against an issue or PR (auto collect context → run agent → publish results):
```bash
export ANTHROPIC_AUTH_TOKEN=...
export GITHUB_TOKEN=...   # or use gh auth login

# Basic usage
holon solve https://github.com/owner/repo/issues/123
# or: holon solve owner/repo#456

# With state persistence for skill caches
holon solve owner/repo#123 --state-dir .holon/state
```

Behavior:
- Issue: creates/updates a branch + PR with the patch and summary.
- PR: applies/pushes the patch to the PR branch and posts replies when needed.


## Using Claude Skills

Claude Skills extend Holon's capabilities by packaging custom instructions, tools, and best practices that Claude can use during task execution.

**Quick example** - Add testing skills to your project:

```bash
# Create a skills directory
mkdir -p .claude/skills/testing-go

# Add a SKILL.md file (see examples/skills/ for templates)
cat > .claude/skills/testing-go/SKILL.md << 'EOF'
---
name: testing-go
description: Expert Go testing skills for table-driven tests and comprehensive coverage
---
# Go Testing Guidelines
[Your testing instructions here]
EOF

# Run Holon - skills are automatically discovered
holon run --goal "Add unit tests for user service"
```

**Skill sources** (in precedence order):
1. CLI flags: `--skill ./path/to/skill` or `--skills skill1,skill2`
2. Project config: `skills: [./skill1, ./skill2]` in `.holon/config.yaml`
3. Spec file: `metadata.skills` field in YAML specs
4. Auto-discovery: `.claude/skills/*/SKILL.md` directories

**See** `docs/skills.md` for complete documentation, examples, and best practices.

## State Persistence

Skills can cache data across runs using the optional `--state-dir` flag:

```bash
# Enable state persistence
holon run --goal "Analyze project trends" --state-dir .holon/state

# Combine with actions/cache in CI for persistent caches
```

The state directory is mounted at `/holon/state` inside the container and persists across runs. Skills should use `/holon/state/<skill-name>/` for cache files.

**See** `docs/state-mount.md` for complete documentation.

## Development & docs
- Build CLI: `make build`; test: `make test`; agent bundle: `(cd agents/claude && npm run bundle)`.
- Operator guide (v0.11): `docs/operator-guide-v0.11.md`
- `run` GA contract: `docs/run-ga-contract.md`
- Skills guide: `docs/skills.md`
- Serve GitHub MVP: `docs/serve-github-mvp.md`
- Design/architecture: `docs/holon-architecture.md`
- Agent contract: `rfc/0002-agent-scheme.md`
- Modes: `docs/modes.md`
- Contributing: see `CONTRIBUTING.md`
