# Agent Tests

This directory contains unit tests for the Holon TypeScript Claude agent.

## Test Coverage

### Logging Safety
- **Log level gating**: Verifies that different log levels (debug, info, progress, minimal) correctly filter messages
- **File path sanitization**: Ensures that host paths are sanitized in logs using `path.basename()` to prevent leaking sensitive host information
- **Tool use logging**: Tests that tool use logging respects log level settings

### Environment Variable Parsing
- **Fallback values**: Tests that missing or invalid environment variables return fallback values
- **Valid parsing**: Verifies that valid numeric environment variables are correctly parsed

### Fallback Summary Generation
- **Markdown formatting**: Tests that fallback summaries are generated with proper markdown structure
- **Success/failure handling**: Verifies correct handling of both success and failure cases

### Command Execution
- **Error handling**: Tests that commands fail correctly when `allowFailure: false`
- **Graceful failure**: Verifies that command execution returns results when `allowFailure: true`
- **Success cases**: Tests successful command execution and output capture

### Artifact Generation
- **Manifest generation**: Tests that `manifest.json` is written with correct structure and metadata
- **Patch generation**: Verifies that `diff.patch` is written to the stable path `/holon/output/diff.patch`
- **Summary generation**: Tests that `summary.md` is generated with fallback content when needed

### Error Handling
- **Missing spec file**: Tests graceful handling of missing `/holon/input/spec.yaml`
- **Missing system prompt**: Tests handling of missing `/holon/input/prompts/system.md`
- **Missing user prompt**: Tests handling of missing `/holon/input/prompts/user.md`

### Git Diff Command Generation
- **Command flags**: Verifies that `git diff` uses `--binary --full-index` flags for patch compatibility
- **Graceful failure**: Tests that git diff failures are handled gracefully when no changes exist

### CI Compatibility
- **Docker independence**: Verifies tests can run without Docker daemon
- **Network independence**: Confirms tests don't require network access
- **File presence**: Tests that all required files are present and accessible

## Running Tests

```bash
# Run all tests
npm test

# Run tests in watch mode
npm run test:watch

# Run from project root
make test-agent
```

## Test Framework

This test suite uses Node.js's built-in `node:test` framework, which provides:
- No external dependencies
- Fast, deterministic execution
- Excellent TypeScript support
- Built-in assertion library
- TAP output format

## CI Requirements

All tests are designed to run in CI environments without:
- Docker daemon
- Network access
- External services

This ensures fast, deterministic test execution that doesn't depend on external infrastructure.
