# GitHub Context Collector

This package provides functionality to collect GitHub Pull Request review context for use in Holon agent executions.

## Features

- Fetch PR metadata (title, description, author, refs, SHAs)
- Retrieve review comment threads with proper parent-reply grouping
- Support pagination for large PRs
- Fetch unified diff format
- Optional filtering for unresolved threads only
- Generate multiple output formats

## Usage

### As a Library

```go
import "github.com/holon-run/holon/pkg/context/github"

config := github.CollectorConfig{
    Owner:          "holon-run",
    Repo:           "holon",
    PRNumber:       42,
    Token:          "ghp_...",
    OutputDir:      "./context",
    UnresolvedOnly: false,
    IncludeDiff:    true,
}

collector := github.NewCollector(config)
err := collector.Collect(context.Background())
```

### CLI Command

```bash
# Collect context for a specific PR
holon context collect-pr \
  --owner holon-run \
  --repo holon \
  --pr 42 \
  --token $GITHUB_TOKEN \
  --out ./context

# From GitHub Actions environment
holon context collect-pr --from-env --out ./holon-input/context
```

## Output Structure

```
output-dir/
└── github/
    ├── pr.json              # PR metadata (JSON)
    ├── review_threads.json  # Review threads with replies (JSON)
    ├── pr.diff              # Unified diff (optional)
    └── review.md            # Human-readable summary (Markdown)
```

## Data Structures

### PRInfo
Contains basic pull request information including number, title, description, author, refs, and SHAs.

### ReviewThread
Represents a review comment thread with:
- Comment metadata (ID, URL, author, timestamps)
- Location (file path, line number, side)
- Diff context
- Comment body
- Replies

### Reply
Represents a reply to a review comment with comment ID, body, author, and timestamps.

## Testing

```bash
go test ./pkg/context/github -v
go test ./pkg/context/github -cover
```

## GitHub Actions Integration

The package is designed to work seamlessly with GitHub Actions:

```yaml
- name: Prepare PR context
  run: |
    ./bin/holon context collect-pr \
      --owner "${{ github.repository_owner }}" \
      --repo "${{ github.event.repository.name }}" \
      --pr "${{ github.event.number }}" \
      --token "${{ secrets.GITHUB_TOKEN }}" \
      --out "./holon-input/context"
```

## Environment Variables

When using `--from-env`:
- `GITHUB_REPOSITORY`: Repository in `owner/repo` format
- `PR_NUMBER`: Pull request number
- `GITHUB_TOKEN` or `GH_TOKEN`: GitHub access token
- `UNRESOLVED_ONLY`: Set to "true" to filter (optional)
- `INCLUDE_DIFF`: Set to "false" to skip diff (optional)
