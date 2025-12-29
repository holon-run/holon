# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Holon is a standardized runner for AI-driven software engineering. It bridges the gap between AI agent probability and engineering determinism by providing a "Brain-in-a-Sandbox" environment. Holon treats AI agent sessions as standardized batch jobs rather than interactive chatbots, enabling deterministic execution with clear inputs/outputs for CI/CD integration.

### Core Architecture

**"Brain-in-Body" Principle**: The AI logic (Brain) runs inside the same container (Body) as the code it's working on, ensuring atomicity and perfect context.

- **Go Framework**: Main CLI and orchestration logic in Go 1.24+
- **Docker Runner**: Container-based execution environment
- **TypeScript Agent**: Claude Code integration via TypeScript agent
- **Spec-Driven Execution**: Declarative YAML task definitions

## Development Commands

### Essential Build & Test Commands
```bash
# Build main CLI binary
make build

# Run all tests (TypeScript agent + Go)
make test

# Run only Go tests
go test ./... -v

# Build agent bundle (required before first run)
cd agents/claude && npm run bundle

# Run example with ANTHROPIC_AUTH_TOKEN set
export ANTHROPIC_AUTH_TOKEN=your_key_here
make run-example

# Clean build artifacts
make clean
```

### Running Holon
```bash
# Basic usage with spec file
./bin/holon run --spec examples/fix-bug.yaml --workspace . --output ./holon-output

# Quick goal-based execution
./bin/holon run --goal "Fix the division by zero error in examples/buggy.go" --image golang:1.22

# With custom Docker image and environment
./bin/holon run --spec task.yaml --image python:3.11 --env VAR=value --log-level debug
```

### Logging

Holon uses structured logging with zap, providing consistent, leveled console output. The logger supports multiple levels for different verbosity needs.

**Log Levels:**
- `debug`: Detailed debugging information (most verbose)
- `info`: General informational messages
- `progress`: Progress and status updates (default)
- `minimal` or `warn`: Warnings and errors only
- `error`: Error messages only

**Setting Log Level:**
```bash
# Via CLI flag
holon run --goal "Fix the bug" --log-level debug

# Via project config
cat > .holon/config.yaml << 'EOF'
log_level: "debug"
EOF

# Default level is "progress"
```

**Logging in Code:**
```go
import holonlog "github.com/holon-run/holon/pkg/log"

// Initialize logger (usually done in main)
holonlog.Init(holonlog.Config{Level: holonlog.LevelDebug})

// Use leveled logging
holonlog.Debug("detailed debug info", "key", "value")
holonlog.Info("general info", "item", "data")
holonlog.Progress("operation progress", "percent", 50)
holonlog.Warn("warning message", "issue", "description")
holonlog.Error("error occurred", "error", err)
```

### Project Configuration File

Holon supports project-level configuration via `.holon/config.yaml` to reduce repeated command-line flags. The config file is automatically searched for in the current directory and its parent directories.

**Config File Format:**
```yaml
# .holon/config.yaml
# Base container toolchain image
# Set to "auto" or "auto-detect" to enable automatic detection (default behavior)
base_image: "python:3.11"

# Default agent bundle reference (path, URL, or alias)
agent: "default"

# Holon log level (debug, info, progress, minimal)
log_level: "debug"

# Git identity overrides for container operations
git:
  author_name: "Holon Bot"
  author_email: "holon@example.com"
```

### Preflight Checks

Holon includes preflight checks that run before execution to verify prerequisites are available. These checks help surface issues early rather than failing mid-execution.

**Default Checks (run command):**
- `docker`: Verifies Docker is installed and the daemon is reachable
- `git`: Verifies Git is installed
- `workspace`: Verifies workspace path is accessible
- `output`: Verifies output directory is writable

**Additional Checks (solve command):**
- `github-token`: Verifies GitHub token is available (from `GITHUB_TOKEN`, `GH_TOKEN`, or `gh auth token`)

**Skipping Preflight Checks:**
```bash
# Skip all preflight checks (not recommended)
holon run --goal "Fix the bug" --no-preflight
holon solve holon-run/holon#123 --skip-checks
```

**Check Levels:**
- `error`: Critical failure that prevents execution
- `warn`: Warning that should be addressed but doesn't block execution
- `info`: Informational output

**Configuration Precedence** (highest to lowest):
1. CLI flags (e.g., `--image`, `--agent`, `--log-level`)
2. Project config file (`.holon/config.yaml`)
3. Auto-detection (when no image is specified and not disabled)
4. Hardcoded defaults (golang:1.22)

**Usage Examples:**
```bash
# Create config file in project root
mkdir -p .holon
cat > .holon/config.yaml << 'EOF'
base_image: "python:3.11"
log_level: "debug"
git:
  author_name: "My Bot"
  author_email: "bot@example.com"
EOF

# Config is automatically loaded
holon run --goal "Fix the bug"
# Output with structured logging shows config resolution (simplified representation)
# [timestamp] INFO config base_image=python:3.11 source=config
# [timestamp] INFO config log_level=debug source=config

# CLI flags override config
holon run --goal "Fix the bug" --log-level info
# Output: Config values are logged with structured fields
# [timestamp] INFO config base_image=python:3.11 source=config
# [timestamp] INFO config log_level=info source=cli
```

**Supported Fields:**
- `base_image`: Default container toolchain image
- `agent`: Default agent bundle reference
- `log_level`: Default logging verbosity
- `git.author_name`: Override git user.name for containers
- `git.author_email`: Override git user.email for containers

**Config Search Path:**
- Holon searches upward from the current directory for `.holon/config.yaml`
- First config file found is used
- If no config file is found, defaults are used

### Automatic Base Image Detection

Holon can automatically detect the appropriate Docker base image for your workspace by analyzing project files. This feature is enabled by default when no image is explicitly specified.

**Root-First Detection Strategy:**

Holon uses a root-first detection strategy to ensure accurate image selection:

1. **Root Directory Scan** (depth 1, files only): First scans only root-level files
   - If signals are found, uses them and stops scanning
   - This prevents dependency directories (`deps/`, `vendor/`) from overriding root project configuration

2. **Full Recursive Scan** (fallback): If no root signals found, performs full workspace scan
   - Scans all subdirectories (excluding `node_modules`, `vendor`, etc.)
   - Uses priority-based selection to choose the best signal

**Benefits:**
- **More Accurate**: Root signals define the project's primary technology
- **Faster**: Root scan is ~10 files in <1ms vs full scan of 10,000+ files in 100-500ms
- **Monorepo-Friendly**: Root workspace files (`pnpm-workspace.yaml`) are prioritized over dependency signals

**Example** - TypeScript project with Go dependencies:
```
project/
├── pnpm-workspace.yaml     # Root workspace (TypeScript/pnpm) ← DETECTED
├── package.json            # Root package.json ← DETECTED
└── deps/
    └── x402_upstream/
        └── go.mod          # Go dependency ← IGNORED (not scanned due to root signals)
```

**Detection Heuristics:**
- `go.mod` → `golang:1.23` (detects `go <version>` directive for version-specific images)
- `Cargo.toml` → `rust:1.83` (version detection not implemented)
- `pyproject.toml` → `python:3.13` (detects `requires-python` or Poetry `python` version)
- `requirements.txt` → `python:3.13` (no version detection)
- `pnpm-workspace.yaml` / `pnpm-workspace.yml` → `node:22` (detects pnpm workspace, higher priority for monorepos)
- `package.json` → `node:22` (detects `engines.node` for version-specific images)
- `.nvmrc` → detected but skipped (hidden file)
- `.node-version` → detected but skipped (hidden file)
- `pom.xml` → `eclipse-temurin:21-jdk` (detects `maven.compiler.source`, `target`, or `release`)
- `build.gradle` / `build.gradle.kts` → `eclipse-temurin:21-jdk` (detects `sourceCompatibility` or `JavaLanguageVersion`)
- `gradle.properties` → `eclipse-temurin:21-jdk` (detects Java version properties)
- `Gemfile` → `ruby:3.3` (version detection not implemented)
- `composer.json` → `php:8.3` (version detection not implemented)
- `*.csproj` → `mcr.microsoft.com/dotnet/sdk:8.0` (version detection not implemented)
- `Dockerfile` → `docker:24` (version detection not implemented)

**Version Detection Details:**

For supported languages (Go, Node.js, Python, Java), Holon attempts to parse version hints from project files and construct version-specific Docker images:

| Language | Version Sources | Example Detection |
|----------|----------------|-------------------|
| **Go** | `go.mod`: `go 1.22` | `go.mod` with `go 1.22` → `golang:1.22` |
| **Node.js** | `package.json`: `engines.node` field | `engines.node: ">=18.0.0"` → `node:18` |
| **Python** | `pyproject.toml`: `requires-python` or Poetry `python` field | `requires-python = ">=3.11"` → `python:3.11` |
| | `.python-version`: File content | `.python-version: "3.12"` → `python:3.12` |
| | `runtime.txt`: Heroku format | `python-3.11.4` → `python:3.11` |
| **Java** | `pom.xml`: `maven.compiler.source/target/release` | `<release>17</release>` → `eclipse-temurin:17-jdk` |
| | `build.gradle`: `sourceCompatibility` or `JavaLanguageVersion` | `JavaLanguageVersion.of(21)` → `eclipse-temurin:21-jdk` |
| | `gradle.properties`: Java version properties | `java.version=17` → `eclipse-temurin:17-jdk` |

**Version Normalization:**
- Range operators like `^`, `~`, `>=` are stripped (e.g., `^1.22` → `1.22`)
- Wildcards (`1.x`, `*`, `any`) fall back to documented LTS versions per language
- If no version hint is found, falls back to static defaults listed above
- Detection results are logged with source file and field information

**Precedence:**
1. CLI `--image` flag (highest priority)
2. Project config `base_image` setting
3. Auto-detection (when no image specified and not disabled)
4. Default fallback (`golang:1.22`)

**Usage Examples:**
```bash
# Auto-detect enabled (default) with root-only scan
holon run --goal "Fix the bug"
# Output: Config: Detected image: golang:1.24 (signals: go.mod) - Detected Go module (go.mod) (version: 1.24 (go.mod: go, line 3: 1.24))

# TypeScript/pnpm project with Go dependencies - root signals win
holon run --goal "Build"
# Output: Config: Detected image: node:22 (signals: pnpm-workspace.yaml, package.json) - Detected pnpm workspace (pnpm-workspace.yaml)
# Note: deps/ directory with go.mod files is NOT scanned (root-only mode)

# Monorepo without root config - falls back to full scan
holon run --goal "Test"
# Output: Config: Detected image: golang:1.23 (signals: go.mod) - Detected Go module (go.mod)
# Note: packages/*/go.mod was found via full-recursive scan

# Disable auto-detection via CLI
holon run --goal "Fix the bug" --image-auto-detect=false
# Output: Config: base_image = "golang:1.22" (source: default)

# Explicit image overrides auto-detection
holon run --goal "Fix the bug" --image python:3.13
# Output: Config: base_image = "python:3.13" (source: cli)
```

**Project Config Examples:**
```yaml
# .holon/config.yaml
# Enable explicit auto-detection
base_image: "auto"

# Or leave empty to use auto-detection as default
# base_image: ""
```

**Solve Command:**
The `holon solve` command also supports auto-detection, analyzing the cloned/fetched repository workspace to select the appropriate base image:
```bash
# Auto-detect from remote repository
holon solve holon-run/holon#123
# Output: Config: Detected image: golang:1.23 (signals: go.mod) - Detected Go module (go.mod)
```

### Detect Command

Holon provides a `holon detect` command to test and debug image auto-detection. This is useful for understanding why a particular image was detected or for verifying detection before running Holon.

**Usage:**
```bash
# Basic usage - detect image for current workspace
holon detect image

# With custom workspace
holon detect image --workspace /path/to/project

# Debug mode with detailed scan information
holon detect image --debug

# JSON output for programmatic use
holon detect image --json
```

**Output Examples:**

Default output (root-only scan):
```
$ holon detect image
✓ Detected image: node:22
  Signals: pnpm-workspace.yaml, package.json
  Rationale: Detected pnpm workspace (pnpm-workspace.yaml)
  Scan mode: root-only
  Workspace: /home/user/project
```

Default output (full-recursive scan - monorepo without root config):
```
$ holon detect image
✓ Detected image: golang:1.23
  Signals: go.mod
  Rationale: Detected Go module (go.mod)
  Scan mode: full-recursive
  Workspace: /home/user/project
```

Debug mode (root-only scan):
```
$ holon detect image --debug
Scanning workspace: /home/user/project
  ✓ Found signal: pnpm-workspace.yaml (priority: 95)
    Path: pnpm-workspace.yaml
  ✓ Found signal: package.json (priority: 90)
    Path: package.json
  ✓ Language version detected:
    Language: node
    Version: 18
    Source: package.json:engines.node
    Line: 3
    Raw: ">=18.0.0"
  ✓ Total files scanned: 1,247
  ✓ Signals found: 2
  ✓ Scan mode: root-only
  ✓ Best signal: pnpm-workspace.yaml (priority: 95)

✓ Detected image: node:22
  Signals: pnpm-workspace.yaml, package.json
  Rationale: Detected pnpm workspace (pnpm-workspace.yaml) (version: >=18.0.0 from package.json:engines.node)
  Version: 18
  Workspace: /home/user/project
  Files scanned: 1,247
  Scan duration: 45ms
  Scan mode: root-only
```

JSON output:
```json
{
  "success": true,
  "image": "node:22",
  "signals": ["pnpm-workspace.yaml", "package.json"],
  "rationale": "Detected pnpm workspace (pnpm-workspace.yaml) (version: >=18.0.0 from package.json:engines.node)",
  "version": {
    "language": "node",
    "version": "18",
    "source_file": "package.json",
    "source_field": "engines.node",
    "line_number": 3,
    "raw_value": ">=18.0.0"
  },
  "workspace": "/home/user/project",
  "disabled": false,
  "scan_stats": {
    "files_scanned": 1247,
    "duration_ms": 45,
    "signals_found": 2,
    "scan_mode": "root-only"
  }
}
```

**Monorepo Detection:**

Holon detects common monorepo patterns to better identify TypeScript/JavaScript projects:

| Pattern | Priority | Example |
|---------|----------|---------|
| `pnpm-workspace.yaml` | 95 | pnpm workspace configuration |
| `package.json` | 90 | Standard Node.js project |
| `packages/*/package.json` | 85 | Monorepo with packages directory |
| `apps/*/package.json` | 85 | Monorepo with apps directory |
| `typescript/*/package.json` | 85 | Monorepo with typescript directory |
| `workspaces/*/package.json` | 85 | Monorepo with workspaces directory |

For monorepo projects without root-level workspace files (like `pnpm-workspace.yaml`), Holon falls back to detecting `package.json` files in common monorepo directory structures.

### Key CLI Flags
- `--spec` / `-s`: Path to holon spec file
- `--goal` / `-g`: Goal description (alternative to spec)
- `--image` / `-i`: Docker base image (default: auto-detect)
- `--image-auto-detect`: Enable automatic base image detection (default: true)
- `--agent`: Agent bundle reference (path to .tar.gz, URL, or alias)
- `--workspace` / `-w`: Workspace path (default: .)
- `--output` / `-O`: Output directory (default: creates temp dir to avoid polluting workspace)
- `--env` / `-e`: Environment variables (K=V format)
- `--log-level`: Logging verbosity (debug, info, progress, minimal)
- `--no-preflight` / `--skip-checks`: Skip preflight checks (not recommended)
- `--agent-config-mode`: Agent config mount mode (default: no)
  - `no`: Never mount (default, safest for CI/container use)
  - `auto`: Mount host ~/.claude if it exists and is compatible, silent skip if not
  - `yes`: Always attempt to mount, warn if missing

### Agent Config Mounting

**WARNING**: This feature exposes your personal Claude login and session to the container. Only use this for local development, never in CI or shared environments.

The `--agent-config-mode` flag controls whether Holon mounts your existing Claude Code configuration from the host machine into the container. Currently applies to Claude agents; future agents will have their own config handling.

**Modes:**

- **`no`** (default): Never mount. Always use environment-based configuration. This is the safest default for CI/container environments and prevents accidental credential exposure.
- **`auto`**: If `~/.claude` exists on the host AND appears compatible (not headless/container Claude), mount it read-only into the container at `/root/.claude`. Skips mounting with a warning if the config appears incompatible.
- **`yes`**: Always attempt to mount, even if the config appears incompatible. Emits a warning if `~/.claude` doesn't exist.

**Compatibility Guard:**

Holon includes a compatibility guard to prevent mounting incompatible configurations. The `auto` mode will skip mounting and log a warning if your `~/.claude` config contains indicators of headless/container Claude in its settings.json file (e.g., `"container": true`, `"headless": true`, or `"IS_SANDBOX": "1"`). This prevents failures that can occur when container-specific configs are mounted into containers.

To force mount even with an incompatible config, use `--agent-config-mode=yes`.

**Usage:**
```bash
# Default: never mount (safest for CI and container use)
./bin/holon run --goal "Fix the bug"

# Explicit auto mode: mount if compatible, skip with warning if not
./bin/holon run --goal "Fix the bug" --agent-config-mode auto

# Force mount (even if config appears incompatible)
./bin/holon run --goal "Fix the bug" --agent-config-mode yes

# Explicitly disable mounting (same as default)
./bin/holon run --goal "Fix the bug" --agent-config-mode no

# With solve command (default: no mount)
./bin/holon solve holon-run/holon#123

# Solve with config mounting enabled
./bin/holon solve holon-run/holon#123 --agent-config-mode auto
```

**How it works:**
1. Holon checks `--agent-config-mode` setting (default: `no`)
2. For `auto`/`yes` modes: checks if `~/.claude` exists on the host
3. For `auto` mode: checks if config appears compatible (skips if incompatible)
4. If mounting is enabled and compatible: mounts it read-only into the container
5. Sets `HOLON_MOUNTED_CLAUDE_CONFIG=1` environment variable
6. Agent detects this variable and skips settings sync
7. Mounted config is used first, env vars as fallback

**When to use mounting:**

Use `--agent-config-mode=auto` or `yes` when:
- You want to use your existing Claude Code settings (API keys, preferences)
- Running locally for development (NOT in CI or shared environments)
- You understand the security implications of mounting credentials

Use the default `no` mode when:
- Running in CI environments (GitHub Actions, etc.)
- Running in shared or untrusted environments
- You prefer explicit environment variable configuration
- You want to avoid any chance of credential exposure

**Agent-agnostic design:**
- Flag is named `--agent-config-mode` (not Claude-specific) for future extensibility
- Non-Claude agents will ignore this flag
- When new agents are added, they can opt into this mounting mechanism

### Agent Bundle Management
- `holon agent install <url> --name <alias>`: Install an agent alias
- `holon agent list`: List installed agent aliases
- `holon agent remove <alias>`: Remove an agent alias
- `holon agent info default`: Show information about builtin default agent

## Code Architecture

### Core Components

**Main CLI Entry Point**: `cmd/holon/main.go`
- Uses Cobra CLI framework
- Single `run` command with comprehensive flags
- Delegates to Runner for execution logic

**Docker Runner**: `pkg/runtime/docker/runtime.go`
- `NewRuntime()`: Initialize Docker client
- `RunHolon()`: Main execution orchestrator
- `buildComposedImageFromBundle()`: Dynamically combines base image + agent bundle

**Agent Resolver System**: `pkg/agent/resolver/`
- Supports multiple resolution strategies: local files, HTTP URLs, aliases, and builtin default
- Automatic caching of downloaded bundles with integrity verification
- CLI commands for managing agent aliases
- **BuiltinResolver**: Provides automatic fallback to default agent when none specified

**Agent Cache**: `pkg/agent/cache/`
- Manages local bundle storage in `$HOLON_CACHE_DIR` or `~/.holon/cache`
- SHA256 checksum verification for integrity
- Metadata tracking and cache deduplication

**Holon Specification**: `pkg/api/v1/spec.go`
```yaml
version: "v1"
kind: Holon
metadata:
  name: "task-name"
context:
  workspace: "/workspace"
  files: ["file1", "file2"]
  env: {"VAR": "value"}
goal:
  description: "Task description"
  issue_id: "123"
output:
  artifacts:
    - path: "manifest.json"
      required: true
    - path: "diff.patch"
      required: true
```

**Preflight System**: `pkg/preflight/`
- `NewChecker()`: Creates a new preflight checker with configurable checks
- `DockerCheck`: Verifies Docker is installed and daemon is reachable
- `GitCheck`: Verifies Git is installed
- `GitHubTokenCheck`: Verifies GitHub token (from env or gh CLI)
- `AnthropicTokenCheck`: Verifies Anthropic API key
- `WorkspaceCheck`: Verifies workspace path is accessible
- `OutputCheck`: Verifies output directory is writable

**TypeScript Agent**: `agents/claude/`
- Entry point inside composed image: `/holon/agent/bin/agent`
- Claude Code runtime installed during composition
- Standardized I/O paths: `/holon/input/`, `/holon/workspace/`, `/holon/output/`

### Execution Flow
1. **Preflight Checks**: Verify prerequisites (Docker, Git, tokens, paths) - can be skipped with `--no-preflight`
2. **Agent Resolution**: Resolve agent bundle reference (local file, URL, or alias)
3. **Bundle Download/Cache**: Download remote bundles and cache locally with integrity verification
4. **Workspace Snapshot**: Copy workspace to isolated location
5. **Image Composition**: Build composed image from base + agent bundle
6. **Container Creation**: Start container with mounted volumes
7. **Agent Execution**: Run Claude Agent SDK agent with injected prompts
8. **Artifact Validation**: Verify required outputs exist

### Directory Structure
```
cmd/holon/          # Main Go CLI entry point
pkg/                # Core Go libraries
  ├── api/v1/       # HolonSpec and HolonManifest types
  ├── agent/        # Agent resolver and cache system
  │   ├── resolver/ # Bundle resolution logic
  │   └── cache/    # Bundle caching and alias management
  ├── preflight/    # Preflight check system
  ├── publisher/    # Publisher system for publishing outputs
  │   ├── github/   # GitHub PR comment/reply publisher
  │   └── githubpr/ # GitHub PR creation/update publisher
  ├── runtime/docker/ # Docker runtime implementation
  └── prompt/       # Prompt compilation system
agents/claude/      # TypeScript Claude agent (bundle source)
tests/integration/  # testscript integration tests
examples/           # Example specification files
rfc/               # RFC documentation
holonbot/          # Node.js GitHub App
```

## Testing

### Integration Tests
- Uses `testscript` framework in `tests/integration/`
- Test files in `tests/integration/testdata/` with `.txtar` format
- Docker-dependent tests skip automatically if Docker unavailable
- Run with: `go test ./tests/integration/... -v`

### Agent Tests
```bash
# Build/check the TypeScript agent
make test-agent
```

## Environment Setup

### Required Environment Variables
- `ANTHROPIC_AUTH_TOKEN`: Claude API authentication (required for execution)
  - Legacy: `ANTHROPIC_API_KEY` is also supported for backward compatibility
- Optional: `HOLON_SNAPSHOT_BASE`: Custom snapshot location
- Optional: `HOLON_CACHE_DIR`: Custom cache directory (default: `~/.holon/cache`)
- Optional: `HOLON_AGENT`: Default agent bundle reference
- Optional: `HOLON_NO_AUTO_INSTALL`: Disable builtin agent auto-install (set to "1" or "true")

### Development Prerequisites
- Docker installed and running
- Go 1.24+ (required by go.mod)
- Node.js (for holonbot development)

## Key Implementation Details

### Holon Execution Artifacts
Each execution produces standardized outputs:
- `manifest.json`: Execution metadata and status
- `diff.patch`: Code changes (if any)
- `summary.md`: Human-readable summary
- Additional artifacts as specified in the spec

### Dynamic Image Composition
The runner dynamically combines base images with the agent bundle at runtime, enabling any standard Docker image to become a Holon execution environment without modification.

### Agent Bundle Usage Examples

**Using local bundle:**
```bash
holon run --agent ./my-agent.tar.gz --goal "Fix the bug"
```

**Using remote URL with integrity verification:**
```bash
holon run --agent https://github.com/example/agent/releases/download/v1.0.0/agent.tar.gz#sha256=abcd1234 --goal "Analyze code"
```

**Using installed alias:**
```bash
holon agent install https://github.com/example/agent/releases/download/v1.0.0/agent.tar.gz --name myagent
holon run --agent myagent --goal "Refactor component"
```

**Cache Management:**
- Bundles are cached in `$HOLON_CACHE_DIR` or `~/.holon/cache`
- Use `holon agent list` to see installed aliases
- Cache avoids re-downloading on repeated runs

### Builtin Agent Auto-Install

Holon includes a builtin default agent that provides out-of-the-box functionality without manual agent bundle setup.

**Default Agent Resolution:**
When no `--agent` is specified, Holon automatically:
1. Attempts to resolve/install the builtin default agent
2. Falls back to local build system if auto-install fails
3. Requires explicit `--agent` if both fail

**Usage Examples:**
```bash
# Uses builtin agent automatically
holon run --goal "Fix the division by zero error in examples/buggy.go"

# Explicitly use builtin agent
holon run --agent default --goal "Analyze the codebase"

# Disable auto-install (requires explicit agent or local build)
HOLON_NO_AUTO_INSTALL=1 holon run --goal "Fix the bug"

# Check builtin agent configuration
holon agent info default
```

**Auto-Install Control:**
- **Enabled by default**: Builtin agent downloads and caches automatically
- **Disable with**: `HOLON_NO_AUTO_INSTALL=1` environment variable
- **Strict environments**: Set this variable to prevent network downloads

**Builtin Agent Metadata:**
- **Location**: `pkg/agent/builtin.go` - embedded in binary
- **Configuration**: Release URL + SHA256 checksum for integrity verification
- **Caching**: Uses same cache system as remote URL installs
- **Updates**: Updated with each Holon release to point to the latest agent release
- **Staleness Detection**: When the builtin agent is used, Holon automatically checks GitHub for newer releases and logs warnings if the embedded version is behind the latest release. This is a best-effort check that runs in the background and doesn't block agent resolution.

### Mock Driver for Testing

Holon includes a mock driver for deterministic end-to-end testing without requiring Anthropic API credentials.

**Mock Mode Usage:**
```bash
# Set environment variables to enable mock mode
export HOLON_CLAUDE_DRIVER=mock
export HOLON_CLAUDE_MOCK_FIXTURE=/path/to/fixture.json

# Run holon (no API key needed)
holon run --goal "Fix the bug" --workspace . --output ./test-output
```

**Fixture File Format:**
```json
{
  "version": "v1",
  "description": "Task description",
  "operations": [
    {
      "type": "write_file",
      "path": "relative/path/to/file",
      "content": "file content"
    }
  ],
  "outcome": {
    "success": true,
    "result_text": "Result message",
    "summary": "Optional summary.md content"
  }
}
```

**Operation Types:**
- `write_file`: Create or overwrite a workspace file
- `append_file`: Append content to an existing file
- `delete_file`: Remove a file
- `write_output`: Write to `/holon/output/` (e.g., summary.md, pr-fix.json)

**Testing Without API Keys:**
```bash
# Run integration tests with mock driver
go test ./tests/integration/... -v -run Mock
```

**Environment Variables:**
- `HOLON_CLAUDE_DRIVER`: Set to `mock` to use mock driver, otherwise uses real SDK
- `HOLON_CLAUDE_MOCK_FIXTURE`: Path to fixture file (required in mock mode)

### Spec vs Goal Execution
- **Spec mode**: Use `--spec` for structured, reproducible task definitions
- **Goal mode**: Use `--goal` for quick, ad-hoc task execution (auto-generates spec)

### Agent Pattern
While v0.1 focuses on Claude Code, the architecture supports future agents through:
- Standardized I/O interface (`/holon/input/`, `/holon/workspace/`, `/holon/output/`)
- Pluggable agent bundles via `--agent` flag
- Resolver system for flexible agent discovery
- Common prompt compilation system in `pkg/prompt/`

### Publisher System

Holon includes a publish system for publishing Holon execution outputs to external systems like GitHub.

**Publisher Interface**: `pkg/publisher/types.go`
- `Publisher` interface defines the contract for all publishers
- `PublishRequest` contains target, artifacts, and manifest
- `PublishResult` tracks actions, errors, and success status

**Available Publishers**:

1. **GitHub Publisher** (`pkg/publisher/github/`):
   - Publishes comments and review replies to existing PRs
   - Target format: `owner/repo/pr/123` or `owner/repo#123`
   - Posts summary comments with idempotency
   - Replies to review comments based on `pr-fix.json`
   - **Deferred work support**: Can create follow-up issues for non-blocking refactor requests
     - New status `deferred` with 🔜 emoji for valid but non-blocking requests
     - Publisher creates issues for any `follow_up_issues` entries without an `issue_url`
     - Agent can optionally create issues and populate `issue_url` if it has token access
     - Follow-up issues include context, rationale, and implementation guidance

2. **GitHub PR Publisher** (`pkg/publisher/githubpr/`):
   - Creates or updates GitHub PRs from Holon outputs
   - Target format: `owner/repo[:base_branch]` (defaults to main)
   - Applies `diff.patch`, creates branch, commits, and pushes
   - Creates PR or updates existing one with same branch
   - Uses `summary.md` for PR title and body
   - References issues via `metadata.issue` or `goal.issue_id`

**Usage**:
```bash
# List available publishers
holon publish list

# Publish to existing PR (comments/replies)
holon publish --provider github --target holon-run/holon/pr/123 --output ./holon-output

# Create/update PR from diff
export GITHUB_TOKEN=ghp_xxx
holon publish --provider github-pr --target holon-run/holon:main --output ./holon-output

# Dry-run (validate without publishing)
holon publish --provider github-pr --target holon-run/holon --dry-run --output ./holon-output
```

**Publisher Registry**:
- Publishers register via `publisher.Register()` in `init()` functions
- Registered in `cmd/holon/main.go` using blank imports
- Thread-safe global registry with `Get()`, `List()`, `Unregister()`

**Environment Variables**:
- `GITHUB_TOKEN`: GitHub authentication token (for both GitHub publishers)
- `HOLON_GITHUB_TOKEN`: Legacy alternative to `GITHUB_TOKEN`
- `HOLON_WORKSPACE`: Workspace directory (for git operations, defaults to `.`)

**Publish Result Output**:
- `publish-result.json` written to output directory
- Contains actions taken, errors, success status, timestamps
- Includes metadata like PR numbers, commit hashes, branch names

**Adding New Publishers**:
1. Create package in `pkg/publisher/<name>/`
2. Implement `Publisher` interface
3. Register in `init()` function
4. Add blank import in `cmd/holon/main.go`

### GitHub Helper System

Holon includes a unified GitHub helper system (`pkg/github/`) that provides a consistent interface for interacting with the GitHub API across collectors and publishers.

**Core Client**: `pkg/github/client.go`
- Unified client supporting both direct HTTP and go-github v68 SDK
- Lazy-loaded go-github client via `GitHubClient()` method
- Automatic rate limit tracking (when enabled via `WithRateLimitTracking`)
- Retry logic with exponential backoff (when configured via `WithRetryConfig`)
- Custom base URL support for testing

**Operations**: `pkg/github/operations.go`
Uses go-github SDK under the hood:
- `FetchPRInfo()` - Pull request information
- `FetchIssueInfo()` - Issue information
- `FetchIssueComments()` - Issue comments with pagination
- `FetchReviewThreads()` - Review comment threads with grouping
- `FetchPRDiff()` - Unified diff
- `FetchCheckRuns()` - CI/check runs
- `FetchCombinedStatus()` - Combined commit status
- `CreateIssue()` - Create a new GitHub issue (used for follow-up issues from pr-fix mode)

**Types**: `pkg/github/types.go`
Custom type definitions that mirror GitHub API responses:
- `PRInfo`, `ReviewThread`, `Reply`, `IssueInfo`
- `CheckRun`, `CheckRunOutput`, `CombinedStatus`, `Status`
- Conversion functions from go-github types to custom types

**Usage Example**:
```go
import "github.com/holon-run/holon/pkg/github"

// Create client with token
client := github.NewClient(token,
    github.WithRateLimitTracking(true),
    github.WithRetryConfig(github.DefaultRetryConfig()),
)

// Fetch PR information
prInfo, err := client.FetchPRInfo(ctx, "owner", "repo", 123)

// Fetch review threads (unresolved only)
threads, err := client.FetchReviewThreads(ctx, "owner", "repo", 123, true)

// Fetch check runs with max limit
checkRuns, err := client.FetchCheckRuns(ctx, "owner", "repo", "sha", 10)
```

**Environment Variables**:
- `GITHUB_TOKEN`: GitHub authentication token (preferred)
- `HOLON_GITHUB_TOKEN`: Legacy alternative

**Testing Support**:
- `SetBaseURL()` method allows custom base URL for httptest servers
- Compatible with httptest for recording/mocking API responses

### Prompt Compiler Architecture

The prompt compiler system (`pkg/prompt/`) uses a **layered assembly** approach to build system prompts from composable assets.

**Layer Order (bottom to top):**
1. **Common Contract** (`contracts/common.md`): Base sandbox rules and physics - **ACTIVE**
2. **Mode Contract** (`modes/{mode}/contract.md`): Mode-specific behavioral overlay (optional)
3. **Role Definition** (`roles/{role}.md` or `modes/{mode}/overlays/{role}.md`): Role-specific behavior

**IMPORTANT:** The active contract is `pkg/prompt/assets/contracts/common.md`. This is the only contract file used by the compiler.

**Asset Structure:**
```
pkg/prompt/assets/
├── manifest.yaml           # Default configuration
├── contracts/
│   └── common.md          # Common contract (ACTIVE - required)
├── modes/
│   ├── solve/             # Default execution mode
│   │   └── contract.md    # Solve mode overlay
│   └── pr-fix/            # PR-fix mode
│       ├── contract.md    # PR-fix mode contract
│       ├── pr-fix.schema.json  # Canonical output schema
│       └── overlays/      # Role-specific overlays (optional)
└── roles/
    ├── developer.md       # Senior software engineer role (alias: coder)
    └── default.md         # Generic assistant role
```

**Manifest Configuration (`manifest.yaml`):**
```yaml
version: 1.0.0
defaults:
  mode: solve      # Default execution mode
  role: developer  # Default role (alias: coder)
  contract: v1    # Legacy field, NOT used by compiler (kept for backward compatibility)
```

**Mode System:**
- Modes define execution patterns (e.g., `solve`, `pr-fix`, `plan`)
- Each mode can have its own contract overlay and optional role-specific overrides in `overlays/`
- Mode contracts are **optional** - if missing, only common contract is used
- The `overlays/` subdirectory within modes is **optional** - modes like `solve` don't have it
- Role files are looked up first in `modes/{mode}/overlays/` (if it exists), then in `roles/`

**Role Aliases:**
- `developer` → `coder` (alias for backward compatibility)
- New roles can be added by creating markdown files in `roles/` or mode-specific subdirectories

**CLI Integration:**
```bash
# Use default mode (solve) and role (coder)
holon run --goal "Fix the bug"

# Explicit mode selection
holon run --mode pr-fix --goal "Fix the PR review comments"

# Use a specific role
holon run --role architect --goal "Design the system"
```

**Environment Variable:**
- `HOLON_MODE`: Execution mode (set by runner, can be overridden via `--mode` flag)

## Common Development Patterns

### Adding New Examples
Create YAML files in `examples/` following the pattern in `fix-bug.yaml`:
1. Define context workspace and required files
2. Provide clear goal description
3. Specify required output artifacts

### Debugging Execution
- Use `--log-level debug` for verbose execution details
- Check generated `manifest.json` for execution status
- Examine container logs via Docker if agent fails
- Review `diff.patch` for generated changes

### Extending the Runner
Core runner extensions in `pkg/runtime/docker/`:
- Modify `buildComposedImage()` for custom image composition
- Extend `RunHolon()` for new execution patterns
- Update artifact validation for new output types

## Go Development Guidelines

### Error Handling Requirements

**MANDATORY**: Never ignore returned errors in Go code.

- **Always check errors**: Every function returning `(result, error)` must handle the error
- **No error ignoring**: Never use `err, _` unless you add a comment explaining why
- **Proper error wrapping**: Use `fmt.Errorf("context: %w", err)` to add context

```go
data, err := os.ReadFile(filename)
if err != nil {
    return "", fmt.Errorf("failed to read file %s: %w", filename, err)
}

// Best-effort cleanup: failure to remove temp file is not critical
_ = os.Remove(tempFile)
```
