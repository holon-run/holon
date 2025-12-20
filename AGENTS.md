# Repository Guidelines

## Project Structure & Module Organization

- `cmd/holon/`: Go CLI entrypoint (`holon`).
- `pkg/`: Go libraries used by the CLI (API spec, prompt compilation, runtime).
- `images/adapter-claude/`: Python-based adapter image (Claude Agent SDK/Claude Code runtime integration).
- `tests/integration/`: Go `testscript` integration tests (`*.txtar`).
- `holonbot/`: Node-based GitHub App/bot (separate CI workflow).
- `rfc/`: Design notes and proposals.
- `.github/workflows/`: CI and automation workflows; `action.yml` defines the local GitHub Action.

## Build, Test, and Development Commands

- `make build`: Build the Go CLI to `bin/holon`.
- `make test`: Run full adapter tests (`make test-adapter`, via `pytest`) followed by Go tests (`go test ./...`).
- `make test-adapter`: Only checks `images/adapter-claude/adapter.py` for syntax errors.
- `make build-adapter-image`: Build the Docker image `holon-adapter-claude`.
- `make test-integration`: Run integration tests (requires Docker).
- `make run-example`: Run an example spec (requires Docker and Anthropic credentials).

## Coding Style & Naming Conventions

- Go: run `gofmt` on all `.go` files; keep exported identifiers and package names idiomatic.
- Python adapter: keep changes minimal and deterministic; avoid committing `__pycache__/` and `*.pyc` (maintain `.gitignore`).
- Files/paths: prefer explicit, stable artifact names in `holon-output/` (e.g., `diff.patch`, `summary.md`).

## Testing Guidelines

- Go unit tests live alongside packages as `*_test.go`.
- Integration tests use `github.com/rogpeppe/go-internal/testscript` under `tests/integration/testdata/*.txtar`.
- Prefer unit tests for logic that should not depend on Docker/LLM connectivity; keep Docker-dependent tests scoped to integration.

## Commit & Pull Request Guidelines

- Commit messages generally use short, imperative summaries (often with issue/PR references like `(#123)`); keep them specific.
- PRs should link the relevant issue, describe behavior changes, and mention how you validated (e.g., `make test`, `make test-integration`).
- If your change affects automation, include notes about workflows touched under `.github/workflows/`.

## Agent-Specific Notes

Holon runs adapters in containers with a standardized layout: workspace at `/holon/workspace`, inputs under `/holon/input/`, and artifacts under `/holon/output/`. Design changes that affect these paths should update relevant RFCs and keep backward compatibility where feasible.
