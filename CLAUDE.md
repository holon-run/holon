# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Holon is a standardized, atomic execution unit for AI-driven software engineering. It bridges the gap between AI agent probability and engineering determinism by providing a "Brain-in-a-Sandbox" environment. Holon treats AI agent sessions as standardized batch jobs rather than interactive chatbots, enabling deterministic execution with clear inputs/outputs for CI/CD integration.

### Core Architecture

**"Brain-in-Body" Principle**: The AI logic (Brain) runs inside the same container (Body) as the code it's working on, ensuring atomicity and perfect context.

- **Go Framework**: Main CLI and orchestration logic in Go 1.24+
- **Docker Runtime**: Container-based execution environment
- **TypeScript Adapter**: Claude Code integration via TypeScript adapter
- **Spec-Driven Execution**: Declarative YAML task definitions

## Development Commands

### Essential Build & Test Commands
```bash
# Build main CLI binary
make build

# Run all tests (TypeScript adapter + Go)
make test

# Run only Go tests
go test ./... -v

# Build adapter Docker image (required before first run)
make build-adapter-image

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
- `--adapter-image`: Adapter Docker image (default: holon-adapter-claude-ts)
- `--workspace` / `-w`: Workspace path (default: .)
- `--out` / `-o`: Output directory (default: ./holon-output)
- `--env` / `-e`: Environment variables (K=V format)
- `--log-level`: Logging verbosity (debug, info, progress, minimal)

## Code Architecture

### Core Components

**Main CLI Entry Point**: `cmd/holon/main.go`
- Uses Cobra CLI framework
- Single `run` command with comprehensive flags
- Delegates to Runner for execution logic

**Docker Runtime**: `pkg/runtime/docker/runtime.go`
- `NewRuntime()`: Initialize Docker client
- `RunHolon()`: Main execution orchestrator
- `buildComposedImage()`: Dynamically combines base + adapter images

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

**TypeScript Adapter**: `images/adapter-claude-ts/`
- Entry point: `/app/dist/adapter.js`
- Pre-installed Claude Code CLI and GitHub CLI
- Standardized I/O paths: `/holon/input/`, `/holon/workspace/`, `/holon/output/`

### Execution Flow
1. **Workspace Snapshot**: Copy workspace to isolated location
2. **Image Composition**: Build composed image from base + adapter
3. **Container Creation**: Start container with mounted volumes
4. **Agent Execution**: Run Claude Code adapter with injected prompts
5. **Artifact Validation**: Verify required outputs exist

### Directory Structure
```
cmd/holon/          # Main Go CLI entry point
pkg/                # Core Go libraries
  ├── api/v1/       # HolonSpec and HolonManifest types
  ├── runtime/docker/ # Docker runtime implementation
  └── prompt/       # Prompt compilation system
images/adapter-claude-ts/ # TypeScript Claude adapter Docker image
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

### Adapter Tests
```bash
# Build/check the TypeScript adapter
make test-adapter
```

## Environment Setup

### Required Environment Variables
- `ANTHROPIC_API_KEY`: Claude API authentication (required for execution)
- Optional: `HOLON_SNAPSHOT_BASE`: Custom snapshot location

### Development Prerequisites
- Docker installed and running
- Go 1.22+
- Node.js (for holonbot development)

## Key Implementation Details

### Holon Execution Artifacts
Each execution produces standardized outputs:
- `manifest.json`: Execution metadata and status
- `diff.patch`: Code changes (if any)
- `summary.md`: Human-readable summary
- Additional artifacts as specified in the spec

### Dynamic Image Composition
The runtime dynamically combines base images with the adapter image at runtime, enabling any standard Docker image to become a Holon execution environment without modification.

### Spec vs Goal Execution
- **Spec mode**: Use `--spec` for structured, reproducible task definitions
- **Goal mode**: Use `--goal` for quick, ad-hoc task execution (auto-generates spec)

### Adapter Pattern
While v0.1 focuses on Claude Code, the architecture supports future adapters through:
- Standardized I/O interface (`/holon/input/`, `/holon/workspace/`, `/holon/output/`)
- Pluggable adapter images via `--adapter-image` flag
- Common prompt compilation system in `pkg/prompt/`

## Common Development Patterns

### Adding New Examples
Create YAML files in `examples/` following the pattern in `fix-bug.yaml`:
1. Define context workspace and required files
2. Provide clear goal description
3. Specify required output artifacts

### Debugging Execution
- Use `--log-level debug` for verbose execution details
- Check generated `manifest.json` for execution status
- Examine container logs via Docker if adapter fails
- Review `diff.patch` for generated changes

### Extending the Runtime
Core runtime extensions in `pkg/runtime/docker/`:
- Modify `buildComposedImage()` for custom image composition
- Extend `RunHolon()` for new execution patterns
- Update artifact validation for new output types
