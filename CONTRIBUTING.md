# Contributing

Thanks for contributing to Holon. This file captures the baseline expectations for PRs and validation.

## Pull Requests

- Link the relevant issue or clearly describe the motivation.
- Summarize behavior changes and highlight any user-visible impact.
- Validation:
  - Required: `make test`
  - Run focused `cargo test ...` commands for the Rust modules or integration
    tests touched by the change when a full test run is too broad.
  - If you cannot run a required check, state why in the PR description.
- If automation changes, mention the workflows touched under `.github/workflows/`.

## Development Commands

```bash
# Build main CLI
make build

# Run Rust tests
make test

# Run live-provider tests when credentials are configured
make test-live

# Check formatting
make fmt

# Type-check without producing binaries
make check
```

## Reference Docs

- `AGENTS.md` is the source of truth for repository-specific guidance; `CLAUDE.md` exists as a pointer for Claude Code and other agent tooling. For deeper contributor references, see `docs/development.md`.
