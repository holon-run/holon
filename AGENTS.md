# Repository Guidelines

## Project Structure & Module Organization

- `cmd/holon/`: Go CLI entrypoint (`holon`).
- `pkg/`: Go libraries used by the CLI (API spec, prompt compilation, runner/runtime).
- `agents/claude/`: TypeScript-based agent bundle sources (Claude Agent SDK integration).
- `tests/integration/`: Go `testscript` integration tests (`*.txtar`).
- `holonbot/`: Node-based GitHub App/bot (separate CI workflow).
- `rfc/`: Design notes and proposals.
- `.github/workflows/`: CI and automation workflows; `action.yml` defines the local GitHub Action.

## Build, Test, and Development Commands

- `make build`: Build the Go CLI to `bin/holon`.
- `make test`: Run agent checks (`make test-agent`) followed by Go tests (`go test ./...`).
- `make test-agent`: Build/check the TypeScript agent under `agents/claude/`.
- `npm run bundle` (under `agents/claude/`): Build the agent bundle archive.
- `make test-integration`: Run integration tests (requires Docker).
- `make run-example`: Run an example spec (requires Docker and Anthropic credentials).

## Coding Style & Naming Conventions

- Go: run `gofmt` on all `.go` files; keep exported identifiers and package names idiomatic.
- TypeScript agent: keep changes minimal and deterministic; avoid committing `node_modules/` and `dist/` (maintain `.gitignore`).
- Files/paths: prefer explicit, stable artifact names in `holon-output/` (e.g., `diff.patch`, `summary.md`).

## Go Error Handling Requirements

**CRITICAL**: Never ignore returned errors in Go code unless absolutely necessary.

### Mandatory Error Handling
- **Always handle errors returned by functions**: Every function that returns an error must have its error value checked and handled.
- **No `err, _` or bare function calls**: Never use blank identifier `_` to ignore errors unless explicitly justified.
- **Proper error propagation**: Return errors up the call stack using `return fmt.Errorf("context: %w", err)` to add context.

### When Ignoring Errors is Acceptable
You may ignore errors **only** when:
1. The operation is truly non-critical and failure has no meaningful impact
2. You have a comment explicitly explaining why the error can be safely ignored
3. The failure case is handled by other means (e.g., idempotent operations, cleanup that's best-effort)

**Example of acceptable error ignoring with comment:**
```go
// Best-effort cleanup: failure to remove temp file is not critical
// as OS will clean it up eventually
_ = os.Remove(tempFile)
```

### Error Handling Patterns
```go
// GOOD: Handle and return errors with context
data, err := os.ReadFile(filename)
if err != nil {
    return "", fmt.Errorf("failed to read config file %s: %w", filename, err)
}

// BAD: Ignoring errors
data, _ := os.ReadFile(filename) // ERROR: Ignored error!

// GOOD: Handle cleanup errors without masking main error
if err := writeFile(); err != nil {
    if cleanupErr := os.RemoveAll(dir); cleanupErr != nil {
        fmt.Printf("Warning: failed to cleanup directory: %v\n", cleanupErr)
    }
    return fmt.Errorf("failed to write file: %w", err)
}
```

### Required Verification
- All agent contributions must be reviewed for proper error handling
- Use `golangci-lint` with error checking rules to catch unhandled errors
- Review test cases to ensure error paths are tested

## Testing Guidelines

- Go unit tests live alongside packages as `*_test.go`.
- Integration tests use `github.com/rogpeppe/go-internal/testscript` under `tests/integration/testdata/*.txtar`.
- Prefer unit tests for logic that should not depend on Docker/LLM connectivity; keep Docker-dependent tests scoped to integration.

## Commit & Pull Request Guidelines

- Commit messages generally use short, imperative summaries (often with issue/PR references like `(#123)`); keep them specific.
- PRs should link the relevant issue, describe behavior changes, and mention how you validated (e.g., `make test`, `make test-integration`).
- If your change affects automation, include notes about workflows touched under `.github/workflows/`.

## Agent-Specific Notes

Holon runs agents in containers with a standardized layout: workspace at `/holon/workspace`, inputs under `/holon/input/`, and artifacts under `/holon/output/`. Design changes that affect these paths should update relevant RFCs and keep backward compatibility where feasible.
