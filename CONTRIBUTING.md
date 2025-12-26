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

# Run Go tests only (with structured output)
make test-go

# Run integration tests (requires Docker)
make test-integration

# Build the Claude agent bundle
cd agents/claude && npm run bundle
```

## Structured Test Output

Holon uses [gotestfmt](https://github.com/gotesttools/gotestfmt) for structured, readable test output. The test targets automatically detect and use gotestfmt if available.

### Installation

```bash
# Install gotestfmt
go install github.com/gotesttools/gotestfmt/v2/cmd/gotestfmt@latest

# Or use the Makefile target
make install-gotestfmt
```

### Test Behavior

- **With gotestfmt**: Tests run with `go test -json` and pipe to gotestfmt for formatted output
- **Without gotestfmt**: Falls back to plain `go test -v` output
- **Exit codes**: Properly preserved in both modes for CI/CD integration

### Manual Testing

When you use the Makefile test targets or `./scripts/test.sh`, stderr is already redirected into the formatter, so you do not need to add `2>&1` yourself. When running go test directly, you'll need to redirect stderr to capture all test output.

```bash
# Run tests directly with gotestfmt (requires stderr redirection)
go test ./... -json -v 2>&1 | gotestfmt

# Test specific packages
go test ./pkg/... -json -v 2>&1 | gotestfmt

# Use the test wrapper script (handles redirection automatically)
./scripts/test.sh ./pkg/...

# Or use flags directly (wrapper auto-detects them as test args)
./scripts/test.sh -run TestMyFunc
```

## Reference Docs

- `AGENTS.md` and `CLAUDE.md` provide repository-specific guidance for agents and tooling.
