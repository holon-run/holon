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

# Run example with ANTHROPIC_API_KEY set
export ANTHROPIC_API_KEY=your_key_here
make run-example

# Clean build artifacts
make clean
```

### Running Holon
```bash
# Basic usage with spec file
./bin/holon run --spec examples/fix-bug.yaml --workspace . --out ./holon-output

# Quick goal-based execution
./bin/holon run --goal "Fix the division by zero error in examples/buggy.go" --image golang:1.22

# With custom Docker image and environment
./bin/holon run --spec task.yaml --image python:3.11 --env VAR=value --log-level debug
```

### Key CLI Flags
- `--spec` / `-s`: Path to holon spec file
- `--goal` / `-g`: Goal description (alternative to spec)
- `--image` / `-i`: Docker base image (default: golang:1.22)
- `--agent`: Agent bundle reference (path to .tar.gz, URL, or alias)
- `--agent-bundle`: Deprecated alias for `--agent`
- `--workspace` / `-w`: Workspace path (default: .)
- `--out` / `-o`: Output directory (default: ./holon-output)
- `--env` / `-e`: Environment variables (K=V format)
- `--log-level`: Logging verbosity (debug, info, progress, minimal)

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

**TypeScript Agent**: `agents/claude/`
- Entry point inside composed image: `/holon/agent/bin/agent`
- Claude Code runtime installed during composition
- Standardized I/O paths: `/holon/input/`, `/holon/workspace/`, `/holon/output/`

### Execution Flow
1. **Agent Resolution**: Resolve agent bundle reference (local file, URL, or alias)
2. **Bundle Download/Cache**: Download remote bundles and cache locally with integrity verification
3. **Workspace Snapshot**: Copy workspace to isolated location
4. **Image Composition**: Build composed image from base + agent bundle
5. **Container Creation**: Start container with mounted volumes
6. **Agent Execution**: Run Claude Agent SDK agent with injected prompts
7. **Artifact Validation**: Verify required outputs exist

### Directory Structure
```
cmd/holon/          # Main Go CLI entry point
pkg/                # Core Go libraries
  ├── api/v1/       # HolonSpec and HolonManifest types
  ├── agent/        # Agent resolver and cache system
  │   ├── resolver/ # Bundle resolution logic
  │   └── cache/    # Bundle caching and alias management
  ├── runtime/docker/ # Docker runtime implementation
  └── prompt/       # Prompt compilation system
agents/claude/ # TypeScript Claude agent (bundle source)
tests/integration/  # testscript integration tests
examples/          # Example specification files
rfc/              # RFC documentation
holonbot/         # Node.js GitHub App
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
- `ANTHROPIC_API_KEY`: Claude API authentication (required for execution)
- Optional: `HOLON_SNAPSHOT_BASE`: Custom snapshot location
- Optional: `HOLON_CACHE_DIR`: Custom cache directory (default: `~/.holon/cache`)
- Optional: `HOLON_AGENT`: Default agent bundle reference
- Optional: `HOLON_AGENT_BUNDLE`: Legacy default agent bundle (deprecated)
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
- **Updates**: Tied to Holon releases (configurable per version)

### Spec vs Goal Execution
- **Spec mode**: Use `--spec` for structured, reproducible task definitions
- **Goal mode**: Use `--goal` for quick, ad-hoc task execution (auto-generates spec)

### Agent Pattern
While v0.1 focuses on Claude Code, the architecture supports future agents through:
- Standardized I/O interface (`/holon/input/`, `/holon/workspace/`, `/holon/output/`)
- Pluggable agent bundles via `--agent` flag
- Resolver system for flexible agent discovery
- Common prompt compilation system in `pkg/prompt/`

### Prompt Compiler Architecture

The prompt compiler system (`pkg/prompt/`) uses a **layered assembly** approach to build system prompts from composable assets.

**Layer Order (bottom to top):**
1. **Common Contract** (`contracts/common.md`): Base sandbox rules and physics
2. **Mode Contract** (`modes/{mode}/contract.md`): Mode-specific behavioral overlay (optional)
3. **Role Definition** (`roles/{role}.md` or `modes/{mode}/roles/{role}.md`): Role-specific behavior

**Asset Structure:**
```
pkg/prompt/assets/
├── manifest.yaml           # Default configuration
├── contracts/
│   └── common.md          # Common contract (required)
├── modes/
│   ├── execute/           # Default execution mode
│   │   ├── contract.md    # Execute mode overlay
│   │   └── roles/         # Mode-specific role overrides
│   └── pr-fix/            # PR-fix mode
│       ├── contract.md    # PR-fix mode contract
│       ├── pr-fix.schema.json  # Canonical output schema
│       └── overlays/      # Role-specific overlays
└── roles/
    ├── coder.md           # Senior software engineer role
    └── default.md         # Generic assistant role
```

**Manifest Configuration (`manifest.yaml`):**
```yaml
version: 1.0.0
defaults:
  mode: execute    # Default execution mode
  role: coder     # Default role (developer maps to coder)
  contract: v1    # Legacy field (for backward compatibility)
```

**Mode System:**
- Modes define execution patterns (e.g., `execute`, `pr-fix`, `plan`)
- Each mode can have its own contract overlay and role-specific overrides
- Mode contracts are **optional** - if missing, only common contract is used
- Role files are looked up first in `modes/{mode}/overlays/`, then in `roles/`

**Role Aliases:**
- `developer` → `coder` (alias for backward compatibility)
- New roles can be added by creating markdown files in `roles/` or mode-specific subdirectories

**CLI Integration:**
```bash
# Use default mode (execute) and role (coder)
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
