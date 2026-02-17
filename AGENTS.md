# Repository Guidelines

`AGENTS.md` is the source of truth for repository-specific guidance for agent tooling. `CLAUDE.md` is intentionally minimal and points here.

For deeper contributor references (debugging, contracts, detection), see `docs/development.md`.

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

- **Always handle errors**: Every function returning `(result, error)` must check the error
- **No error ignoring**: Never use `err, _` unless you add a comment explaining why
- **Proper error wrapping**: Use `fmt.Errorf("context: %w", err)` to add context

**Example:**
```go
data, err := os.ReadFile(filename)
if err != nil {
    return "", fmt.Errorf("failed to read file %s: %w", filename, err)
}

// Best-effort cleanup: failure to remove temp file is not critical
_ = os.Remove(tempFile)
```

## Testing Guidelines

- Go unit tests live alongside packages as `*_test.go`.
- Integration tests use `github.com/rogpeppe/go-internal/testscript` under `tests/integration/testdata/*.txtar`.
- Prefer unit tests for logic that should not depend on Docker/LLM connectivity; keep Docker-dependent tests scoped to integration.

## Commit & Pull Request Guidelines

- Commit messages generally use short, imperative summaries (often with issue/PR references like `(#123)`); keep them specific.
- PRs should link the relevant issue, describe behavior changes, and mention how you validated (e.g., `make test`, `make test-integration`).
- If your change affects automation, include notes about workflows touched under `.github/workflows/`.

## GitHub CLI Conventions

- For `gh issue comment/create/edit` and `gh pr create/edit/review`, always write the body to a file first and pass it via `--body-file` (or `-F`).
- Do not use inline multi-line `--body "..."` for substantive content; this avoids shell escaping and newline/backtick interpolation issues.

## Agent-Specific Notes

Holon runtime is now centered on `agent_home`:
- persona and role files live under agent home (for example `AGENTS.md`, `ROLE.md`)
- runtime/skill behavior should rely on runtime contract variables and system-recommended directories
- skills should avoid hardcoded Holon-specific filesystem paths/env names unless the contract explicitly guarantees them

When changing runtime path semantics, update:
- `README.md`
- `rfc/README.md` + affected RFC status/notes
- relevant docs under `docs/` (especially architecture and operator guides)
