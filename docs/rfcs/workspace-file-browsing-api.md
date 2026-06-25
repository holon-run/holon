---
title: RFC: Workspace File Browsing API
date: 2026-06-25
status: draft
issue:
  - 1796
---

# RFC: Workspace File Browsing API

## Summary

Holon should expose a RESTful file browsing API scoped to registered
workspaces, authenticated via `RemoteAccess`, so that a Web GUI can browse
directory trees, preview text and image files, and download binary files
without requiring a separate control token.

## Motivation

Issue #1796 requests workspace-scoped file browsing for the Web GUI. In a
remote scenario, the operator needs to inspect files in an agent's active
workspace through the browser. The current HTTP surface has no file-system
browsing capability — workspace endpoints are limited to control-plane
attach/exit/detach operations that require `AuthKind::Control`.

The Web GUI holds a remote access session token, not a control token. File
browsing is a high-frequency operator action, so it should use the same auth
surface as other Web GUI reads (`AuthKind::RemoteAccess`).

## Core Decisions

### 1. Workspace is an independent resource

Files belong to a workspace, not to an agent. The route namespace reflects
this:

```
GET /workspaces/{workspace_id}/files/{path:path}
```

This avoids coupling file operations to agent lifecycle. A workspace persists
in the host registry independent of which agents are attached to it.

### 2. RemoteAccess authentication

File browsing uses `AuthKind::RemoteAccess`, the same auth kind used by
`/agents/list`, `/agents/{id}/status`, and event streams. The Web GUI can use
its existing session token. No additional control token is required.

### 3. Single endpoint with content negotiation

A single RESTful endpoint serves all file operations. The response shape is
determined by the target path type (directory vs file), the file's MIME type,
and query parameters:

| Condition | Response |
|-----------|----------|
| Path is a directory | `application/json` directory listing |
| Text file, `Accept: application/json` | `{ content, size, mime_type, truncated }` |
| Text file, `Accept: text/plain` or default | Raw body with correct `Content-Type` |
| Image file | Raw bytes with image MIME type |
| `?download=1` | `Content-Disposition: attachment` |
| `?meta=1` | Metadata only, no content body |

### 4. Execution root selection

A workspace may have multiple execution roots (canonical root and isolated git
worktrees). The optional `execution_root_id` query parameter selects which
root to browse:

```
GET /workspaces/{workspace_id}/files/{path}?execution_root_id=<id>
```

When omitted, the workspace's canonical anchor path is used.

### 5. No dotfile hiding

Directory listings return all entries, including dotfiles. This matches
browser file-explorer expectations and avoids surprise hiding.

### 6. File size limit

Text file reads are capped at `READ_LIMIT_BYTES = 1 MB` (1048576 bytes). When
the limit is exceeded:
- JSON mode returns `truncated: true` + the first 1 MB of content + `total_size`
- Raw body mode returns the truncated content with `X-Content-Truncated: true`
  response header

Binary and image downloads are streamed without truncation. Future support for
range reads (`?offset` + `?limit`) can be added without breaking the current
contract.

## API Design

### Route

```
GET /workspaces/{workspace_id}/files/{path:path}
```

`{path:path}` captures multi-segment paths (e.g. `src/http/mod.rs`). The root
of the workspace is addressed as `/workspaces/{workspace_id}/files` or
`/workspaces/{workspace_id}/files/`.

### Query Parameters

| Parameter | Default | Description |
|-----------|---------|-------------|
| `execution_root_id` | canonical anchor | Select an isolated execution root |
| `download` | `false` | Force `Content-Disposition: attachment` |
| `meta` | `false` | Return only metadata (size, MIME type, type) without content |

### Response: Directory Listing

```json
{
  "type": "directory",
  "path": "src/http",
  "workspace_id": "ws-abc123",
  "entries": [
    { "name": "mod.rs", "type": "file", "size": 1092, "mime_type": "text/x-rust" },
    { "name": "control.rs", "type": "file", "size": 5000, "mime_type": "text/x-rust" },
  ]
}
```

Each entry includes `name`, `type` (`file` | `directory` | `symlink`),
`size` (bytes, 0 for directories), and `mime_type` (best-effort inference).

### Response: Text File (JSON mode)

```json
{
  "type": "file",
  "path": "src/http/mod.rs",
  "workspace_id": "ws-abc123",
  "content": "...file content...",
  "size": 1092,
  "mime_type": "text/x-rust",
  "truncated": false
}
```

When `truncated` is `true`, the response also includes `total_size`.

### Response: Metadata Only (`?meta=1`)

```json
{
  "type": "file",
  "path": "logo.png",
  "workspace_id": "ws-abc123",
  "size": 40960,
  "mime_type": "image/png",
  "truncated": false
}
```

### Response: Raw File Body

For `Accept: text/plain` or no `Accept` header (default), text files return
the raw body with the appropriate `Content-Type`. Image files always return
raw bytes. Binary files with `?download=1` return raw bytes with
`Content-Disposition: attachment`.

## Path Security

All requested paths are resolved against the workspace's execution root using
the existing `normalize_path` function from `src/system/workspace.rs`. The
normalized path must start with the execution root prefix. Any path that
escapes the execution root returns `403 Forbidden`.

This protects against path traversal attacks (`../../../etc/passwd`) without
requiring a separate validation layer.

## MIME Type Inference

MIME types are inferred from file extensions using the `mime_guess` crate
(already in the dependency tree). Unknown extensions fall back to
`application/octet-stream`.

## Access Scope

Phase 1 allows browsing all registered workspaces. The host workspace registry
is the source of truth — any workspace that exists in the registry can be
browsed. Per-workspace ACL can be layered on in a future phase without
changing the route structure.

## OpenAPI Registration

The new route is registered in the OpenAPI route table with
`AuthKind::RemoteAccess`. The response schema varies by content type, so the
OpenAPI entry documents the JSON envelope shape; raw-body responses are
described in the route summary.

## Non-Goals

- File mutation, upload, or deletion (read-only browsing only)
- Per-workspace access control lists
- Full-text search or indexing
- Archive/zip download
- WebDAV or other standard remote file protocols

## Implementation Plan

### Commit 1: Workspace file lookup + route skeleton

- Add `workspace_file_entries` query to `RuntimeHost` (workspace lookup by id
  from the registry)
- Register `GET /workspaces/{workspace_id}/files/{path:path}` route with
  `RemoteAccess` auth
- Directory listing (JSON)
- OpenAPI spec entry

### Commit 2: File metadata + text content read

- File metadata response (`?meta=1`)
- Text file content reading with `Accept` content negotiation
- MIME type inference via `mime_guess`
- Read limit (1 MB) and truncation handling

### Commit 3: Binary + image download

- Raw body response for images and binary files
- `Content-Disposition: attachment` for `?download=1`
- `X-Content-Truncated` header for truncated raw reads

### Commit 4: OpenAPI snapshot + tests

- Update OpenAPI snapshot test
- Integration tests for directory listing, text read, binary download, and
  path traversal protection
