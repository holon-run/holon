# GitHub Helper Test Fixtures

This directory contains recorded HTTP fixtures for testing the GitHub helper layer without network access.

## Overview

The test harness uses [go-vcr](https://github.com/dnaeon/go-vcr) to record and replay GitHub API interactions. This allows tests to run deterministically without requiring network access or live API calls.

## Fixture Files

Fixtures are stored in YAML format in the `fixtures/` subdirectory. Each fixture file contains:

- HTTP requests made to GitHub API
- HTTP responses from GitHub API
- Headers (including rate limit information)
- Response bodies

### Current Fixtures

The following fixtures are referenced in tests:

- `fetch_pr_info.yaml` - Pull request information
- `fetch_issue_info.yaml` - Issue information
- `fetch_issue_comments.yaml` - Issue comments with pagination
- `fetch_review_threads_all.yaml` - All review comment threads
- `fetch_review_threads_unresolved.yaml` - Unresolved review threads only
- `fetch_pr_diff.yaml` - Pull request unified diff
- `fetch_check_runs_all.yaml` - All check runs for a commit
- `fetch_check_runs_limited.yaml` - Limited check runs (max 5)
- `fetch_combined_status.yaml` - Combined status for a ref
- `rate_limit_tracking.yaml` - Request with rate limit headers
- `error_not_found.yaml` - 404 error response
- `pagination_issue_comments.yaml` - Issue comments with multiple pages
- `pagination_review_threads.yaml` - Review threads with multiple pages

## Recording New Fixtures

### Prerequisites

1. A valid GitHub token with appropriate permissions
2. Access to the target repository (default: `holon-run/holon`)

### Recording Fixtures Locally

To record new fixtures or update existing ones:

```bash
# Set record mode and provide your GitHub token
export HOLON_VCR_MODE=record
export GITHUB_TOKEN=your_token_here

# Run the specific test you want to record
go test -v ./pkg/github/ -run TestFetchPRInfo

# Or run all tests (will record/overwrite all fixtures)
go test -v ./pkg/github/
```

### Recording Fixtures Using Holon

You can also record fixtures using holon in a containerized environment:

```bash
# Use holon to run tests in record mode with the token from the container
holon run --goal "Record GitHub VCR fixtures" \
  --env HOLON_VCR_MODE=record \
  --env GITHUB_TOKEN \
  --image golang:1.24 \
  --workspace .
```

The token from the host's `GITHUB_TOKEN` environment variable will be passed to the container and used for recording.

### Example: Recording a New Fixture

If you add a new test that requires a fixture:

1. Create the test with a unique fixture name:
```go
client, rec := setupTestClient(t, "my_new_fixture")
defer rec.Stop()
```

2. Run in record mode:
```bash
HOLON_VCR_MODE=record GITHUB_TOKEN=your_token go test -v ./pkg/github/ -run TestMyNewTest
```

3. Verify the fixture was created:
```bash
ls -la pkg/github/testdata/fixtures/my_new_fixture.yaml
```

## Replaying Fixtures

By default, tests run in replay mode (no network access required):

```bash
# Run all GitHub helper tests
go test ./pkg/github/...

# Run a specific test
go test -v ./pkg/github/ -run TestFetchPRInfo

# Run in short mode (skips integration tests)
go test -short ./pkg/github/
```

## Updating Fixtures

When GitHub API responses change or you need to refresh fixtures:

1. Delete or backup the old fixture:
```bash
rm pkg/github/testdata/fixtures/old_fixture.yaml
# or
mv pkg/github/testdata/fixtures/old_fixture.yaml pkg/github/testdata/fixtures/old_fixture.yaml.bak
```

2. Record a new fixture (see "Recording New Fixtures" above)

3. Commit the updated fixture to the repository

## Fixture Format

Fixtures are stored in YAML format. Example structure:

```yaml
http_interactions:
- request:
    body: null
    headers:
      Accept:
      - application/vnd.github.v3+json
    method: GET
    url: https://api.github.com/repos/holon-run/holon/pulls/123
  response:
    body:
      string: '{"number":123,"title":"Test PR",...}'
    headers:
      X-RateLimit-Limit:
      - "5000"
      X-RateLimit-Remaining:
      - "4999"
      X-RateLimit-Reset:
      - "1234567890"
    status: 200 OK
    code: 200
```

## Security Considerations

- **Tokens are filtered**: The test harness automatically removes `Authorization` headers from recorded fixtures
- **No sensitive data**: Fixtures should not contain sensitive information
- **Public repos only**: Use public repositories for fixtures when possible
- **Review changes**: Always review fixture changes before committing

## Best Practices

1. **Minimal fixtures**: Keep fixtures focused and minimal
2. **Real data**: Use realistic data from actual GitHub API responses
3. **Documentation**: Document what each fixture represents in test code
4. **Version control**: Commit fixtures to version control for reproducibility
5. **Updates**: Update fixtures when GitHub API versions change
6. **Privacy**: Never record fixtures from private repositories with sensitive data

## Troubleshooting

### Tests fail with "no such file or directory"

The fixture file doesn't exist. You need to record it first:
```bash
HOLON_VCR_MODE=record GITHUB_TOKEN=your_token go test -v ./pkg/github/...
```

### Tests fail with "API error" in replay mode

The fixture may be outdated or the API response has changed. Re-record the fixture.

### Tests pass in record mode but fail in replay mode

This suggests the fixture wasn't saved properly or there's a difference in behavior. Check:
1. Fixture file exists in `testdata/fixtures/`
2. Fixture file contains valid YAML
3. Fixture file has the correct HTTP interactions

### Rate limiting during recording

If you hit rate limits while recording:
1. Wait for the rate limit to reset (typically 1 hour)
2. Use a token with higher rate limits
3. Record fixtures in smaller batches

## CI/CD Integration

In CI/CD pipelines, tests should run in replay mode (default):

```yaml
# Example GitHub Actions
- name: Test GitHub helper
  run: go test ./pkg/github/...
```

No network access or tokens required in replay mode.

## Related Documentation

- [Go-VCR Documentation](https://github.com/dnaeon/go-vcr)
- [GitHub REST API Documentation](https://docs.github.com/en/rest)
- [Client implementation](../../pkg/github/)
