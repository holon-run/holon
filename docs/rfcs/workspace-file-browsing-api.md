---
title: RFC: Workspace File Browsing API
date: 2026-06-25
status: draft
issue:
  - 1796
  - 3032
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
| `?download=true` | `Content-Disposition: attachment` |
| `?meta=1` | Metadata only, no content body |

### 4. Execution root selection

A workspace may have multiple execution roots (canonical root and isolated git
worktrees). The optional `execution_root_id` query parameter is reserved for
selecting which root to browse:

```
GET /workspaces/{workspace_id}/files/{path}?execution_root_id=<id>
```

When omitted, the workspace's canonical anchor path is used. Both
`execution_root_id` and the historical `root` alias are accepted. If both are
present they must be equal. The selector is an opaque registry key; it is
never parsed as a filesystem path. Unknown roots return 404, removed roots
return 410, and roots owned by another workspace return 403.

### 5. No dotfile hiding

Directory listings return all entries, including dotfiles. This matches
browser file-explorer expectations and avoids surprise hiding.

### 6. File size limit

Text file reads are capped at `READ_LIMIT_BYTES = 1 MB` (1048576 bytes). When
the limit is exceeded:
- JSON mode returns `truncated: true` + the first 1 MB of content + `total_size`

Raw body mode (binary files, `?download=true`, or direct-link access without
JSON negotiation) streams the complete file from disk without truncation and
supports standard HTTP range and conditional requests (see "Response: Raw
File Body").

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
  "execution_root_id": "canonical_root:ws-abc123",
  "absolute_path": "/srv/holon/workspaces/ws-abc123/src/http",
  "kind": "directory",
  "root_kind": "canonical_root",
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
  "execution_root_id": "git_worktree_root:opaque-token",
  "absolute_path": "/srv/holon/worktrees/issue-3032/src/http/mod.rs",
  "kind": "file",
  "root_kind": "git_worktree_root",
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
  "execution_root_id": "canonical_root:ws-abc123",
  "absolute_path": "/srv/holon/workspaces/ws-abc123/logo.png",
  "kind": "file",
  "root_kind": "canonical_root",
  "size": 40960,
  "mime_type": "image/png",
  "truncated": false
}
```

### Response: Raw File Body

Binary files, explicit downloads, and direct-link access to text files (no
`Accept: application/json` negotiation) stream raw bytes from disk instead of
buffering the file in memory:

- `Accept-Ranges: bytes` is always advertised; a single `Range: bytes=…`
  header returns `206 Partial Content` with `Content-Range`. Malformed or
  multi-range requests fall back to the full `200` body; reversed ranges
  (`last < first`) are invalid specs and are likewise ignored; unsatisfiable
  ranges return `416` with `Content-Range: bytes */<size>`.
- Each response carries a strong `ETag` (path + size + mtime) and
  `Last-Modified`. Matching `If-None-Match` returns `304 Not Modified`;
  `Range` is only honored when `If-Range` matches the current validator
  (strong comparison; weak tags never match).
- `?download=true` sets `Content-Disposition: attachment` (RFC 5987 encoded
  for non-ASCII names); otherwise the file is served inline.
- Every raw response sends `X-Content-Type-Options: nosniff`. Inline
  responses for active content types (`text/html`, `image/svg+xml`, XML
  variants) add `Content-Security-Policy: sandbox` so direct same-origin
  navigation cannot execute workspace-controlled scripts.
- Text responses are served with `charset=utf-8`; internal-only MIME labels
  (`text/typescript`, `text/tsx`) are normalized to `text/plain` for browser
  rendering.

## Path Security

All requested paths are resolved against the workspace's execution root using
the existing `normalize_path` function from `src/system/workspace.rs`. The
normalized path must start with the execution root prefix. Any path that
escapes the execution root returns `403 Forbidden`.

Existing paths are also canonicalized so a symlink cannot escape the selected
root. A failed explicit root lookup never falls back to the canonical root.
Canonical selectors must exactly equal `canonical_root:<workspace_id>`; a
matching prefix is insufficient.

## File Reference Resolution

The browsing API remains the only file-content read surface. A separate thin
endpoint translates references into complete locations:

```
POST /file-references/resolve
```

The request contains up to 64 tagged references:

- `absolute_path`: a Unix host path, used literally rather than URI-decoded;
- `workspace_uri`: a historical `workspace://` URI, decoded once, with an
  optional opaque `?root=` selector;
- `relative_path` plus `base_file`: resolved from the base file's containing
  directory inside the same execution root. Legal `..` segments may move
  within that root but never select another root.

Each result is independently either `resolved` with a `FileLocation` or
`unresolved` with a stable reason (`invalid_reference`, `not_found`,
`root_removed`, `forbidden`, or `ambiguous_root`). The endpoint locates only:
it does not return file contents, directory listings, bearer URLs, or
capabilities. Callers use the returned `workspace_id`, `execution_root_id`,
and root-relative `path` with the existing GET endpoint.

Absolute-path matching considers all registered roots, including removed
tombstones, and selects the unique most-specific path-component match. This
prevents a removed nested root from being silently reinterpreted through a
wider canonical root. Roots are first deduplicated by normalized anchor path,
so multiple root ids claiming the same directory (stale registry rows, or the
legacy `agent_home` alias next to a canonical `agent_home:<id>` anchor)
cannot yield `ambiguous_root`; the live workspace anchor wins over stale
registry rows, non-removed entries over tombstones, and canonical
`agent_home:<id>` over the legacy shared alias. `FileLocation` is a location,
not a permanent content identity; path reuse after cleanup is outside this
contract.

## Optional desktop integration

`holon serve --desktop-integration` (also accepted by `daemon start/restart`)
opts into macOS Finder integration. The flag defaults to off, is retained in
daemon launch arguments, and can be explicitly disabled with
`--desktop-integration=false`. Enabling it requires a numeric loopback listen
address and macOS; other configurations fail before runtime startup.

This is an operator assertion that the instance is being used directly on its
desktop. Do not enable it for a container, forwarded port, or reverse proxy.
Neither a loopback URL nor the existing `connection.mode = local` proves that
the browser and runtime share a filesystem. The GUI therefore calls these
URLs “Loopback address”, and otherwise displays the endpoint host.

- `GET /desktop/capabilities` returns `{ "reveal_in_finder": boolean }` after
  control authentication. It reports true only when explicitly enabled and
  requested through a loopback peer and Host. Missing peer information fails
  closed. Forwarded headers are never used to infer locality.
- `POST /desktop/reveal` accepts `{ workspace_id, execution_root_id, path }`.
  It requires control authentication, the same capability conditions, and an
  explicit Origin matching the loopback Host (including port). Cross-site
  Fetch Metadata is rejected. Missing or null Origin fails closed.
- The server resolves the opaque root and relative path through the existing
  workspace file resolver, validates canonical containment and existence,
  then invokes the fixed `/usr/bin/open -R <canonical path>` argument vector.
  No shell, arbitrary executable, or client-supplied absolute path is accepted.
- Finder availability is advisory; POST rechecks all boundaries. A failed
  launch produces an error shown in the GUI. This API does not edit files.

The GUI displays the action only for a same-origin loopback API that advertises
this capability. Older servers and unsupported platforms retain preview,
copy, and download. Authentication and file/root identity are unchanged.

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
