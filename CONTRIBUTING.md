# Contributing

Thanks for contributing to Holon. This file captures the baseline expectations for PRs and validation.

## Pull Requests

- Link the relevant issue or clearly describe the motivation.
- Summarize behavior changes and highlight any user-visible impact.
- Validation:
  - Required: `make test`
  - Run `make test-integration` when you change CLI flags/output, runner/runtime behavior, or `tests/integration/` fixtures.
  - If you cannot run a required check, state why in the PR description.
- If CLI flags or output change, update the integration fixtures under `tests/integration/testdata/*.txtar`.
- If automation changes, mention the workflows touched under `.github/workflows/`.

## Development Commands

```bash
# Build main CLI
make build

# Run all tests (agent + Go)
make test

# Run integration tests (requires Docker)
make test-integration

# Build the Claude agent bundle
cd agents/claude && npm run bundle
```

## Reference Docs

- `AGENTS.md` and `CLAUDE.md` provide repository-specific guidance for agents and tooling.
