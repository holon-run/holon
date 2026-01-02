# Holon Workspace Manifest Format

## Overview

The `workspace.manifest.json` file is written by the Holon runtime after workspace preparation. It contains metadata about how the workspace was created, including the preparation strategy used, source origin, git reference information, and repository state.

**Location:** `/holon/output/workspace.manifest.json`

**Note:** Prior to v0.8.0, this file was written to the workspace root. It is now written to the output directory for better isolation.

## Purpose

- **Workspace Provenance:** Documents the origin and preparation method of the workspace
- **Git State Tracking:** Records the git reference, commit SHA, and repository state
- **Debugging Support:** Provides context about workspace creation (shallow clone, history availability)
- **Reproducibility:** Captures timestamps and preparation notes for debugging and audit trails

## Schema

The canonical JSON Schema is maintained at [`schemas/workspace.manifest.schema.json`](../schemas/workspace.manifest.schema.json).

### Structure

```json
{
  "strategy": "git-clone",
  "source": "https://github.com/holon-run/holon",
  "ref": "main",
  "head_sha": "6a06914c4603fe4bf33c0a5a2931f10be38544b2",
  "created_at": "2026-01-02T12:34:56Z",
  "has_history": true,
  "is_shallow": false
}
```

### Field Reference

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `strategy` | string | Yes | Workspace preparation strategy used (e.g., `"git-clone"`, `"snapshot"`, `"existing"`) |
| `source` | string | Yes | Origin of the workspace content (repository URL or local path) |
| `ref` | string | No | Git reference that was checked out (branch, tag, or SHA) |
| `head_sha` | string | No | Full 40-character commit SHA of the workspace |
| `created_at` | string | Yes | Timestamp of workspace preparation (RFC 3339 format) |
| `has_history` | boolean | Yes | Whether full git history is available |
| `is_shallow` | boolean | Yes | Whether the git repository is a shallow clone |
| `notes` | array | No | Additional information or warnings about preparation |

## Strategy Types

### Git-Clone Strategy
Used when cloning a git repository (from GitHub or any other git host):

```json
{
  "strategy": "git-clone",
  "source": "https://github.com/holon-run/holon",
  "ref": "main",
  "head_sha": "6a06914c4603fe4bf33c0a5a2931f10be38544b2",
  "created_at": "2026-01-02T12:34:56Z",
  "has_history": true,
  "is_shallow": false
}
```

### Snapshot Strategy
Used when copying workspace files without git history:

```json
{
  "strategy": "snapshot",
  "source": "/local/path/to/workspace",
  "created_at": "2026-01-02T08:00:00Z",
  "has_history": false,
  "is_shallow": false
}
```

### Existing Strategy
Used when using an existing directory as the workspace:

```json
{
  "strategy": "existing",
  "source": "/existing/workspace",
  "created_at": "2026-01-02T07:00:00Z",
  "has_history": true,
  "is_shallow": false
}
```

## Git Repository States

### Full History Repository
A complete clone with full git history:

```json
{
  "strategy": "git-clone",
  "source": "https://github.com/user/repo",
  "has_history": true,
  "is_shallow": false
}
```

### Shallow Clone
A shallow clone (depth=1) for faster checkout:

```json
{
  "strategy": "git-clone",
  "source": "https://github.com/holon-run/holon",
  "ref": "refs/pull/123/head",
  "head_sha": "abc123def456789abc123def456789abc123def4",
  "created_at": "2026-01-02T10:00:00Z",
  "has_history": false,
  "is_shallow": true,
  "notes": [
    "Repository cloned with depth=1",
    "Fetched PR #123 from upstream"
  ]
}
```

## Examples

### Main Branch Checkout
```json
{
  "strategy": "git-clone",
  "source": "https://github.com/holon-run/holon",
  "ref": "main",
  "head_sha": "6a06914c4603fe4bf33c0a5a2931f10be38544b2",
  "created_at": "2026-01-02T12:34:56Z",
  "has_history": true,
  "is_shallow": false
}
```

### Pull Request Checkout
```json
{
  "strategy": "git-clone",
  "source": "https://github.com/holon-run/holon",
  "ref": "refs/pull/423/head",
  "head_sha": "def456789abc123def456789abc123def456789a",
  "created_at": "2026-01-02T11:00:00Z",
  "has_history": false,
  "is_shallow": true,
  "notes": [
    "Repository cloned with depth=1",
    "Fetched PR #423 from upstream"
  ]
}
```

### Tag Checkout
```json
{
  "strategy": "git-clone",
  "source": "https://github.com/user/repo",
  "ref": "v1.2.3",
  "head_sha": "123abc456def789123abc456def789123abc456d",
  "created_at": "2026-01-02T09:00:00Z",
  "has_history": true,
  "is_shallow": false
}
```

### Snapshot from Local Directory
```json
{
  "strategy": "snapshot",
  "source": "/home/user/projects/myproject",
  "created_at": "2026-01-02T08:30:00Z",
  "has_history": false,
  "is_shallow": false
}
```

## Schema-First Design

To ensure type safety between Go (runtime) and any external consumers, the workspace manifest follows a **Schema-First** approach:

1. **Single Source of Truth:** [`schemas/workspace.manifest.schema.json`](../schemas/workspace.manifest.schema.json) defines the canonical format
2. **Go Struct Alignment:** The `Manifest` struct in [`pkg/workspace/types.go`](../pkg/workspace/types.go) is kept in sync with the schema
3. **Validation:** JSON Schema validation can be used to verify manifest files

## Validation

### JSON Schema Validation
Validate manifest files against the schema using `ajv`:

```bash
npx ajv validate \
  -s schemas/workspace.manifest.schema.json \
  -d path/to/workspace.manifest.json
```

### Using Other Validators

```bash
# Using jsonschema (Python)
jsonschema -i workspace.manifest.json schemas/workspace.manifest.schema.json

# Using jq (with ajv-cli)
ajv validate -s schemas/workspace.manifest.schema.json -d workspace.manifest.json
```

## Type Definitions

### Go (Runtime)
```go
type Manifest struct {
    Strategy   string    `json:"strategy"`
    Source     string    `json:"source"`
    Ref        string    `json:"ref,omitempty"`
    HeadSHA    string    `json:"head_sha,omitempty"`
    CreatedAt  time.Time `json:"created_at"`
    HasHistory bool      `json:"has_history"`
    IsShallow  bool      `json:"is_shallow"`
    Notes      []string  `json:"notes,omitempty"`
}
```
Location: [`pkg/workspace/types.go`](../pkg/workspace/types.go)

## Related Documentation

- [Run Manifest Format](./manifest-format.md) - Execution output manifest (`manifest.json`)
- [Workspace Preparation](../pkg/workspace/) - Go implementation of workspace strategies

## Version History

### v0.8.0
- Output location changed from workspace root to `/holon/output/` directory
- Schema-first design introduced with JSON Schema definition
