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
- Go toolchain (this repo uses Go 1.24; see `go.mod`) - OR use prebuilt binaries
- Anthropic API key: `ANTHROPIC_API_KEY` (or `ANTHROPIC_AUTH_TOKEN`)

### Option 1: Using prebuilt binaries

Download the latest release for your platform from [GitHub Releases](https://github.com/holon-run/holon/releases):

**Linux (amd64):**
```bash
curl -fsSL https://github.com/holon-run/holon/releases/latest/download/holon-linux-amd64.tar.gz | tar -xz
chmod +x holon-linux-amd64
sudo mv holon-linux-amd64 /usr/local/bin/holon
```

**macOS (Intel):**
```bash
curl -fsSL https://github.com/holon-run/holon/releases/latest/download/holon-darwin-amd64.tar.gz | tar -xz
chmod +x holon-darwin-amd64
sudo mv holon-darwin-amd64 /usr/local/bin/holon
```

**macOS (Apple Silicon):**
```bash
curl -fsSL https://github.com/holon-run/holon/releases/latest/download/holon-darwin-arm64.tar.gz | tar -xz
chmod +x holon-darwin-arm64
sudo mv holon-darwin-arm64 /usr/local/bin/holon
```

Verify your installation:
```bash
holon version
```

### Option 2: Building from source

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
- uses: holon-run/holon@main
  with:
    anthropic_api_key: ${{ secrets.ANTHROPIC_API_KEY }}
```

## Configuration
CLI commands:
- `run`: Run a Holon agent execution
- `solve`: Solve a GitHub Issue or PR reference (high-level workflow)
- `fix`: Alias for `solve` - resolve a GitHub Issue or PR reference
- `version`: Show version information
- `agent`: Manage agent bundles and aliases (`install`, `list`, `remove`, `info`)
- `context`: Context management
- `publish`: Publishing functionality

### `holon solve` - High-level GitHub workflow

The `solve` command provides a single entry point to resolve GitHub Issues or PRs:

```bash
# Solve an issue (creates/updates a PR)
holon solve https://github.com/owner/repo/issues/123

# Solve a PR (fixes review comments)
holon solve https://github.com/owner/repo/pull/456

# Short form references
holon solve owner/repo#789
holon solve 123 --repo owner/repo  # requires --repo for numeric refs
```

**Workflow:**
1. **Collect context** - Fetches issue/PR data from GitHub API
2. **Run Holon** - Executes agent with appropriate mode (`solve` for issues, `pr-fix` for PRs)
3. **Publish results** - Creates/updates PR for issues, or posts fixes for PRs

**Supported reference formats:**
- Full URLs: `https://github.com/<owner>/<repo>/issues/<n>` or `.../pull/<n>`
- Short forms: `<owner>/<repo>#<n>`
- Numeric: `#<n>` or `<n>` (requires `--repo owner/repo`)

**Explicit subcommands:**
```bash
holon solve issue <ref>   # Force issue mode
holon solve pr <ref>      # Force PR mode
```

**Flags:**
- `--repo <owner/repo>`: Default repository for numeric references
- `--base <branch>`: Base branch for PR creation (default: `main`, issue mode only)
- `--agent <bundle>`: Agent bundle reference
- `--image <image>`: Docker base image (default: auto-detect)
- `--image-auto-detect`: Enable automatic base image detection (default: `true`)
- `--out <dir>`: Output directory (default: `./holon-output`)
- `--role <role>`: Role to assume (e.g., `developer`, `reviewer`)
- `--log-level <level>`: Log verbosity (default: `progress`)

**Example: Issue resolution**
```bash
export GITHUB_TOKEN=ghp_xxx
export ANTHROPIC_API_KEY=sk-ant-xxx
holon solve holon-run/holon#123
```

This will:
1. Collect issue context (title, body, comments)
2. Run Holon in "solve" mode to implement a solution
3. Create/commit/push changes to a new branch
4. Create or update a PR referencing the original issue

**Example: PR review fixes**
```bash
holon solve pr https://github.com/holon-run/holon/pull/456
```

This will:
1. Collect PR context (diff, review threads, checks)
2. Run Holon in "pr-fix" mode to address review comments
3. Apply/push changes to the PR branch
4. Post replies based on `pr-fix.json`

### Project Configuration File

You can create `.holon/config.yaml` in your project root to set defaults and reduce repeated flags:

```yaml
# .holon/config.yaml
# Base container toolchain image
# Set to "auto" or "auto-detect" to enable automatic detection (default behavior)
base_image: "python:3.11"

# Default agent bundle reference
agent: "default"

# Holon log level (debug, info, progress, minimal)
log_level: "debug"

# Git identity overrides for container operations
git:
  author_name: "My Bot"
  author_email: "bot@example.com"
```

**Configuration Precedence** (highest to lowest):
1. CLI flags (`--image`, `--agent`, etc.)
2. Project config file (`.holon/config.yaml`)
3. Auto-detection (when no image is specified and not disabled)
4. Hardcoded defaults

### Automatic Base Image Detection

Holon can automatically detect the appropriate Docker base image for your workspace by analyzing project files. When no image is explicitly specified via CLI or config, Holon scans the workspace for language/framework indicators.

**Detection heuristics:**
- `go.mod` → `golang:1.23`
- `Cargo.toml` → `rust:1.83`
- `pyproject.toml` → `python:3.13`
- `requirements.txt` → `python:3.13`
- `package.json` → `node:22`
- `pom.xml` → `eclipse-temurin:21-jdk` (Maven)
- `build.gradle` / `build.gradle.kts` → `eclipse-temurin:21-jdk` (Gradle)
- `Gemfile` → `ruby:3.3`
- `composer.json` → `php:8.3`
- `*.csproj` → `mcr.microsoft.com/dotnet/sdk:8.0`
- `Dockerfile` → `docker:24`

For polyglot repos, the signal with the highest priority is selected. You can override auto-detection by:
- Using `--image <image>` CLI flag
- Setting `base_image: <image>` in `.holon/config.yaml`
- Disabling with `--image-auto-detect=false`

**Example:**
```bash
# Auto-detect from workspace
holon run --goal "Fix the bug"
# Output: Config: Detected image: golang:1.23 (signals: go.mod) - Detected Go module (go.mod)

# Disable auto-detection
holon run --goal "Fix the bug" --image-auto-detect=false
# Output: Config: base_image = "golang:1.22" (source: default)
```

**Supported Fields:**
- `base_image`: Default container toolchain image
- `agent`: Default agent bundle reference
- `log_level`: Default logging verbosity
- `git.author_name`: Override git user.name for containers
- `git.author_email`: Override git user.email for containers

The config file is searched for in the current directory and parent directories.

CLI flags (most used):
- `--goal` / `--spec`: task input
- `--image`: base toolchain image (default: auto-detect)
- `--image-auto-detect`: Enable automatic base image detection (default: true)
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
We're converging on a single user-facing knob: `mode` (`solve` / `plan` / `review`) that binds prompts + hard sandbox semantics + required artifacts. Design notes live in `docs/modes.md`.

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
