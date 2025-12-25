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

### Mock Driver for Testing

Holon includes a mock driver for deterministic end-to-end testing without requiring Anthropic API credentials.

**Mock Mode Usage:**
```bash
# Set environment variables to enable mock mode
export HOLON_CLAUDE_DRIVER=mock
export HOLON_CLAUDE_MOCK_FIXTURE=/path/to/fixture.json

# Run holon (no API key needed)
holon run --goal "Fix the bug" --workspace . --out ./test-output
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
holon publish --provider github --target holon-run/holon/pr/123 --out ./holon-output

# Create/update PR from diff
export GITHUB_TOKEN=ghp_xxx
holon publish --provider github-pr --target holon-run/holon:main --out ./holon-output

# Dry-run (validate without publishing)
holon publish --provider github-pr --target holon-run/holon --dry-run --out ./holon-output
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
