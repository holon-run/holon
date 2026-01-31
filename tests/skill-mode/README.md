# Skill-Mode Tests

This directory contains integration-style tests for the skill-mode pipeline that do NOT require LLM or network access.

## Purpose

These tests validate the skill-mode workflow (collector/publisher scripts) with stubbed dependencies, allowing for:
- Fast, reliable testing in CI without external dependencies
- Validation of error handling and edge cases
- Testing of the runner's skill-mode path

## Test Structure

### Shell Script Tests

- **test_helper_drift.sh**: Ensures only the shared `skills/github-context/scripts/lib/helpers.sh` exists (prevents forked copies)
- **test_collector.sh**: Tests the collector script (`skills/github-context/scripts/collect.sh`)
  - Missing dependencies (gh, jq)
  - Issue collection
  - Empty comments
  - Error handling

- **test_publisher.sh**: Tests the publisher script (`skills/github-publish/scripts/publish.sh`)
  - Help functionality
  - Missing intent files
  - Invalid JSON
  - Dry-run mode

### Test Fixtures

- **fixtures/**: Contains JSON fixture data for testing
  - `issue_*.json`: Sample issue data
  - `pr_*.json`: Sample PR data
  - `*_comments.json`: Sample comments data

### Stub Scripts

- **scripts/gh-stub**: Stubbed `gh` CLI that returns fixture data
- **scripts/jq-stub**: Stubbed `jq` that falls back to real `jq` if available

These stubs allow testing without:
- GitHub API access
- Network calls
- Real authentication tokens

## Running Tests

### All Skill-Mode Tests

```bash
make test-skill-mode
```

### Individual Test Suites

```bash
# Test collector only
make test-skill-mode-collector

# Test publisher only
make test-skill-mode-publisher
```

### Direct Execution

```bash
./tests/skill-mode/test_collector.sh
./tests/skill-mode/test_publisher.sh
```

## Integration Tests

The testscript tests in `tests/integration/testdata/` validate the runner's skill-mode behavior:

- `skill-mode-basic.txtar`: Basic skill-mode execution
- `skill-mode-with-context.txtar`: Skill mode with pre-provided context
- `skill-mode-missing-outputs.txtar`: Error handling for missing outputs

These run as part of the standard integration test suite:

```bash
make test-integration
```

## Adding New Tests

### Adding Shell Script Tests

1. Create a new test function in `test_collector.sh` or `test_publisher.sh`
2. Use the provided assertion helpers (e.g., `assert_file_exists`, `assert_file_not_empty`, `assert_json_valid`, `assert_contains`)
3. Add the test to the `main()` function

### Adding Test Fixtures

1. Create JSON files in `fixtures/`
2. Use descriptive names (e.g., `issue_520.json`)
3. Ensure valid JSON structure

### Adding Integration Tests

1. Create new `.txtar` files in `tests/integration/testdata/`
2. Follow the testscript format (see existing examples)
3. Use `mkdir`, `exec`, `exists`, `! exec` for test operations

## CI/CD Integration

These tests are designed to run in CI without:
- LLM API keys
- GitHub tokens
- Network access
- Docker (for shell tests)

This makes them fast, reliable, and cost-effective for continuous integration.

## Coverage

The tests cover:

- **Collector Script**:
  - Dependency checking (gh, jq)
  - Reference parsing (URL, owner/repo#num, numeric)
  - Issue/PR metadata fetching
  - Comments and review threads
  - Output file creation and validation
  - Manifest generation

- **Publisher Script**:
  - Intent file parsing
  - Dry-run mode
  - Error handling for missing/invalid files
  - Help functionality

- **Runner Skill-Mode**:
  - Skill invocation
  - Context discovery
  - Output validation
  - Error handling for missing outputs

## Limitations

These tests do NOT cover:
- Actual GitHub API interactions (covered by integration tests with tokens)
- LLM-generated patches/intent (covered by manual or smoke tests)
- Real network calls

For full end-to-end testing, see the integration test suite in `tests/integration/`.
