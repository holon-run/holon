---
title: holon solve
summary: Use holon solve to automate GitHub issues and pull requests in headless mode.
order: 8
---

# holon solve

`holon solve` is the headless command for automating GitHub issues and pull
requests. Give it a target and it runs an agent that collects context, implements
a fix, reviews a PR, or publishes a comment — then writes structured output
artifacts you can consume in scripts or CI.

## When to use solve

Use `holon solve` when you want to:

- Automatically implement a GitHub issue without manual TUI interaction
- Fix a PR based on review feedback in CI
- Run a review pass on a pull request
- Integrate Holon into a scripted pipeline (GitHub Actions, CLI scripting)

Do not use `holon solve` for interactive work — use `holon tui` or `holon run`
when you need direct conversation with the agent.

## Target reference formats

The first positional argument is the target ref. Holon accepts three formats:

| Format | Example | Notes |
|--------|---------|-------|
| Full URL | `https://github.com/holon-run/holon/issues/42` | Most explicit; kind is inferred from the URL |
| `owner/repo#NN` | `holon-run/holon#42` | Treated as an issue or pull request |
| `#NN` with `--repo` | `#42 --repo holon-run/holon` | Shortest form; requires `--repo` |

## Quick start

### Solve an issue

```bash
# Full URL form
holon solve https://github.com/holon-run/holon/issues/42

# Short form
holon solve holon-run/holon#42

# Numeric ref with --repo
holon solve 42 --repo holon-run/holon
```

The agent clones the repository, reads the issue, collects related context,
implements the fix, and commits the change. Output artifacts are written to
a temporary directory.

### Provide a custom goal

```bash
holon solve holon-run/holon#42 \
  --goal "Add a --dry-run flag to the solve command"
```

Use `--goal` to override the agent's interpretation of the target, or to scope
the work when the issue body is large.

### Review a pull request

```bash
holon solve https://github.com/holon-run/holon/pull/753 \
  --goal "Review only: check for security issues and publish one review"
```

When the goal mentions review, Holon adds guardrails that require the agent to
publish exactly one review or one PR comment, then stop. This keeps review runs
single-shot and predictable.

### Fix a PR from review feedback

```bash
holon solve https://github.com/holon-run/holon/pull/753 \
  --base "feature-branch"
```

Use `--base` to set a different base branch for the fix branch. The default
base is `main`.

## Configuration

Solve reads the standard Holon runtime configuration (`~/.holon/config.json`).
At minimum you need a model and credentials. Use the CLI to configure:

```bash
# Set the default model
holon config set model.default "anthropic/claude-sonnet-4-6"

# Store credentials securely (recommended)
holon config credentials set --kind api_key --stdin anthropic
# Paste your API key and press Enter, then Ctrl+D
```

Or use environment variables for quick setup:

```bash
export ANTHROPIC_AUTH_TOKEN="your-api-key"
holon config set model.default "anthropic/claude-sonnet-4-6"
```

Holon also needs a GitHub token. It reads `GITHUB_TOKEN` or `GH_TOKEN` from
the environment:

```bash
export GITHUB_TOKEN="ghp_..."
holon solve holon-run/holon#42
```

Verify your configuration is complete:

```bash
holon config doctor
holon config models list
```

## How solve works

When you run `holon solve`, these steps happen inside the runtime:

1. **Parse the target** — Holon extracts owner, repo, issue/PR number, and kind
   from the ref you provide.

2. **Prepare directories** — An output directory is created (default:
   `$TMPDIR/holon-output-<uuid>`) and a `github-context/` subdirectory holds
   input metadata.

3. **Create the agent** — Holon creates an agent from the built-in
   `holon-github-solve` template, which includes the `github-issue-solve`,
   `github-pr-fix`, `github-review`, and `ghx` skills.

4. **Run the prompt** — The runtime constructs a prompt describing the target
   and goal, then runs the agent with the configured trust level and turn
   limit.

5. **Collect artifacts** — When the run finishes, Holon writes these files
   into the output directory:

   | File | Content |
   |------|---------|
   | `manifest.json` | Outcome metadata: provider, status, outcome, target |
   | `summary.md` | Human-readable summary of what the agent did |
   | `run.json` | Full structured run response |

## Full flag reference

```
holon solve <REF> [OPTIONS]
```

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `REF` (positional) | string | (required) | GitHub URL, `owner/repo#NN`, or `#NN` |
| `--repo` | string | — | Repository for numeric-only refs (e.g. `holon-run/holon`) |
| `--base` | string | `main` | Base branch for fix branches |
| `--goal` | string | — | Override the agent's interpretation of the target |
| `--role` | string | — | Additional role context passed to the agent |
| `--agent` | string | `github-solve` | Agent ID to use or create |
| `--template` | string | `holon-github-solve` | Template for the agent |
| `--model` | string | — | Override the configured model (sets `HOLON_MODEL`) |
| `--max-turns` | integer | — | Maximum agent turns before forced stop |
| `--trust` | string | `trusted-operator` | Trust level for the run |
| `--json` | flag | false | Print output as JSON instead of text |
| `--home` | path | `~/.holon` | Holon home directory |
| `--workspace` | path | — | Working directory for the agent |
| `--cwd` | path | — | Current working directory for the agent |
| `--input` | path | — | Directory for input context (overrides default) |
| `--output` | path | — | Directory for output artifacts (overrides default) |

## Differences from holon run

`holon run` and `holon solve` both execute agents in headless mode, but they
serve different purposes:

| | `holon run` | `holon solve` |
|---|---|---|
| Use case | General headless tasks | GitHub issues and PRs |
| Input | Free-text prompt | GitHub target ref |
| Agent template | `holon-default` | `holon-github-solve` (GitHub skills pre-loaded) |
| Output | Text or JSON to stdout | Structured artifacts in output directory |
| GitHub integration | Manual (`gh` CLI) | Automatic context collection and skill dispatch |
| Pipeline-friendly | Use `--json` for structured output | Built-in manifest and summary files |

Use `holon run` for general automation and scripting. Use `holon solve` when
your task starts from a GitHub issue or pull request and you want automatic
skill selection.

## Scripting with solve

### JSON output for scripting

```bash
holon solve holon-run/holon#42 --json | jq '.final_status'
# "completed"
```

### Custom output directory

```bash
holon solve holon-run/holon#42 --output ./solve-results/
cat ./solve-results/run.json
# { "provider": "holon-solve", "status": "completed", ... }
```


### CI integration sketch

```bash
#!/bin/bash
# Run solve and fail on incomplete outcome

OUTPUT=$(mktemp -d)
holon solve "$ISSUE_URL" --output "$OUTPUT" --json

STATUS=$(jq -r '.final_status' "$OUTPUT/run.json")
if [ "$STATUS" != "completed" ]; then
  echo "Solve did not complete: $STATUS"
  exit 1
fi

# The agent may have committed changes; push them
git push origin HEAD
```

## See also

- [Holon CLI reference](/reference/cli) — full command tree
- [Configuration reference](/reference/configuration) — model and provider setup
- [Integration guide](/guides/integration) — HTTP control plane access
- [Multi-agent collaboration](/guides/multi-agent) — spawning child agents
