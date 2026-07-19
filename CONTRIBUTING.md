# Contributing

Thanks for contributing to Holon. This file captures the baseline expectations for PRs and validation.

## Pull Requests

- Link the relevant issue or clearly describe the motivation.
- Summarize behavior changes and highlight any user-visible impact.
- Validation:
  - Required: `make ci`
  - Web GUI changes: `make web-ci` for the focused Vitest and production-build
    check used by the main CI job.
  - Runtime lifecycle, task, wait, SSE, or HTTP task changes:
    `make test-concurrent`
  - CLI, HTTP, OpenAPI, or model tool contract changes:
    `make snapshots-refresh`, review the diff, then run `make snapshots-check`.
  - Run focused `cargo test ...` commands for the Rust modules or integration
    tests touched by the change when a full test run is too broad.
  - If you cannot run a required check, state why in the PR description.
- If automation changes, mention the workflows touched under `.github/workflows/`.

## Development Commands

Web GUI development and validation require Node.js 24 LTS. The root `.nvmrc`
selects the supported version.

```bash
# Run the full local CI suite, including Web GUI tests and build
make ci

# Build main CLI
make build

# Run Web GUI Vitest and the production build after one clean install
make web-ci

# Run Rust tests
make test

# Run selected runtime lifecycle integration tests with Rust's default threads
make test-concurrent

# Stress the core concurrent lifecycle suite; stops at the first failure
make test-concurrent-repeat CONCURRENT_REPEATS=3

# Check all checked-in Rust-generated snapshots
make snapshots-check

# Refresh all checked-in Rust-generated snapshots after an intentional change
make snapshots-refresh

# Run the baseline and configured-provider live smoke tests
make test-live

# Run focused provider/runtime live suites when their credentials are configured
make test-live-openai
make test-live-anthropic
make test-live-codex
make test-live-xai
make test-live-images
make test-live-runtime

# Check formatting
make fmt

# Type-check without producing binaries
make check
```

## Reference Docs

- `AGENTS.md` is the source of truth for repository-specific guidance; `CLAUDE.md` exists as a pointer for Claude Code and other agent tooling. For deeper contributor references, see `docs/development.md`.
