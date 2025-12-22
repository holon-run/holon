# Holon

Holon runs an AI coding agent inside a Docker sandbox and emits **standard artifacts** (`diff.patch`, `summary.md`, `manifest.json`) so you can wire probabilistic agent work into deterministic automation (e.g. **Issue → PR**).

## What it does
- **Runs headlessly** in Docker (`/holon/workspace` + `/holon/output`)
- **Keeps your repo unchanged by default**: the agent edits an isolated snapshot, then outputs a patch you can apply explicitly
- **Fits CI**: ship outputs as artifacts, post summaries to logs, and open/update PRs in workflows

## Outputs (artifacts)
Holon writes to the directory specified by `--out` (default `./holon-output`). Common artifacts:

| Path | Purpose | Notes |
| --- | --- | --- |
| `manifest.json` | machine-readable run metadata/status | always expected |
| `diff.patch` | patch representing workspace changes | produced when requested by spec/goal |
| `summary.md` | human summary for PR body / step summary | optional/required depending on spec |
| `prompt.compiled.system.md` | compiled system prompt (debug) | written by runner |
| `prompt.compiled.user.md` | compiled user prompt (debug) | written by runner |

See `rfc/0002-agent-scheme.md` for the agent artifact contract.

## Quickstart (local)
Prereqs:
- Docker
- Go toolchain (this repo uses Go 1.24; see `go.mod`)
- Anthropic API key: `ANTHROPIC_API_KEY` (or `ANTHROPIC_AUTH_TOKEN`)

Pick a base toolchain image that matches your repo (via `--image`). Holon composes it with the agent bundle at runtime.

Run the included example:
```bash
export ANTHROPIC_API_KEY=...
make build
(cd agents/claude && npm run bundle)
BUNDLE_PATH=$(ls -t agents/claude/dist/agent-bundles/*.tar.gz | head -n 1)
./bin/holon run --spec examples/fix-bug.yaml --image golang:1.22 --agent "$BUNDLE_PATH" --workspace . --out ./holon-output
```

Apply the patch to your working tree (explicit, outside the sandbox):
```bash
git apply --3way holon-output/diff.patch
```

Run with an ad-hoc goal (no spec file needed):
```bash
./bin/holon run --goal "Fix the bug in foo.go" --workspace . --out ./holon-output
```

## Quickstart (GitHub Actions)
This repo ships a composite action (`action.yml`) that:
1) extracts the Issue title/body from the event
2) runs `holon run --goal ...`
3) outputs artifacts under `holon-output/`

See `.github/workflows/holon-issue.yml` for a working end-to-end flow that applies `diff.patch` and creates/updates a PR.

Minimal usage:
```yaml
- uses: jolestar/holon@main
  with:
    anthropic_api_key: ${{ secrets.ANTHROPIC_API_KEY }}
```

## Configuration
CLI flags (most used):
- `--goal` / `--spec`: task input
- `--image`: base toolchain image (e.g. `golang:1.22`, `node:20`, ...)
- `--agent`: agent bundle reference (`.tar.gz`)
- `--agent-bundle`: deprecated alias for `--agent`
- `--workspace`: repo/workspace path (default `.`)
- `--context`: extra context dir mounted at `/holon/input/context`
- `--role`: prompt persona (`developer`, `reviewer`)
- `--log-level`: `debug|info|progress|minimal`
- `--env K=V`: pass env into the sandbox

Agent selection env vars (optional):
- `HOLON_AGENT`: agent bundle reference (`.tar.gz`)
- `HOLON_AGENT_BUNDLE`: deprecated alias for `HOLON_AGENT`

Claude agent env (optional):
- `HOLON_MODEL`, `HOLON_FALLBACK_MODEL`
- `HOLON_QUERY_TIMEOUT_SECONDS`, `HOLON_HEARTBEAT_SECONDS`, `HOLON_RESPONSE_IDLE_TIMEOUT_SECONDS`, `HOLON_RESPONSE_TOTAL_TIMEOUT_SECONDS` (see `agents/claude/README.md`)

## Spec format (v1)
Tasks can be defined via `--spec` using `spec.yaml` (see `examples/*.yaml`). At minimum you declare:
- goal (`goal.description`)
- expected outputs (`output.artifacts`)

## Execution modes (design)
We’re converging on a single user-facing knob: `mode` (`execute` / `plan` / `review`) that binds prompts + hard sandbox semantics + required artifacts. Design notes live in `docs/modes.md`.

Today, the CLI supports `--role` (prompt persona) and `--log-level`. `--mode` is tracked as a follow-up (see issue #94).

## Terminology
Project terms and naming are being stabilized for public release. See `docs/terminology.md`.

## Development
Useful commands:
```bash
make build          # build ./bin/holon
make test           # Go tests + agent build check
make test-agent     # TypeScript agent build check
make test-integration  # integration tests (requires Docker)
```

Repo layout:
- `cmd/holon/`: runner CLI
- `pkg/`: spec parsing, prompt compilation, docker runner
- `agents/claude/`: Claude agent (TypeScript, Claude Agent SDK)
- `examples/`: runnable specs
- `rfc/`: design docs / contracts
