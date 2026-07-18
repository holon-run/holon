---
title: RFC: Execution Root Registry
date: 2026-07-14
status: draft
issue:
  - 2238
---

# RFC: Execution Root Registry

## Summary

Extend `workspace://` file reference URIs with an optional `?root=`
query parameter carrying an opaque `execution_root_id`, backed by a durable
runtime_db registry that maps root IDs to filesystem paths. This lets
worktree-generated links resolve correctly even after the agent switches to
a different execution root.

## Motivation

`workspace://<workspace_id>/<relative_path>` URIs only identify the logical
workspace, not the concrete execution root. When multiple worktrees share
the same `workspace_id`, a link generated in one worktree resolves against
the canonical or currently active root — opening the wrong path or failing.

The HTTP file API scanned all agents' `active_workspace_entry` to find a
matching `execution_root_id`, which is fragile and breaks when agents switch
roots.

## Design

### URI Format

```
workspace://<workspace_id>/<relative_path>?root=<execution_root_id>
```

- `?root=` is **optional**. When absent, resolves to the canonical root
  (backward compatible with all existing `workspace://` links).
- When present, the value is percent-encoded and treated as an **opaque
  lookup key** — the resolver never parses embedded path information.
- Canonical-root links omit `?root=` and stay backward-compatible.

### Execution Root Registry

New `execution_root_entries` table in runtime_db:

```sql
CREATE TABLE execution_root_entries (
  execution_root_id TEXT PRIMARY KEY,
  workspace_id      TEXT NOT NULL,
  filesystem_path   TEXT NOT NULL,
  root_kind         TEXT NOT NULL,
  created_at        TEXT NOT NULL,
  removed_at        TEXT,
  payload_json      TEXT NOT NULL
);
```

Lifecycle:
- **Canonical root**: registered lazily when the agent enters a workspace.
  `removed_at` is always `None`.
- **Worktree root**: registered when `enter_workspace` (GitWorktreeRoot) or
  `enter_existing_git_worktree` is called.
- **Worktree cleanup**: `exit_worktree` marks `removed_at` (soft-delete).
- **Resolution**: look up by `execution_root_id`; if `removed_at` is set →
  HTTP 410 Gone; if not found → 404 Not Found.

The serialized payload may also carry worktree artifact metadata without
changing the lookup columns:

- provenance (`runtime_created` or `discovered`);
- branch ref and tip;
- requested base ref and resolved commit when known;
- worktree common-directory identity;
- lifecycle state and latest cleanup evidence.

This registry is the durable artifact lookup used by `GetWorkspaceState`,
`SwitchWorkspace`, `CreateWorktree`, and `RemoveWorktree`. Retained entries
remain discoverable after switching away or detaching a workspace binding.
Removed entries remain tombstoned for audit and return 410 through
file-resolution surfaces.

### HTTP File API

`resolve_workspace_root` now looks up the registry instead of scanning agent
state. Accepts both `?root=` and `?execution_root_id=` query parameters.

### Provider Turn Resolver

`resolve_markdown_image_src` parses `?root=` from `workspace://` URIs and
resolves via `ExecutionSnapshot.execution_roots`, which is populated from
the registry for the agent's attached workspaces.

## Security Model

- `execution_root_id` is an **opaque lookup key**. The resolver never parses
  embedded path information from it.
- Fabricating an ID does not grant filesystem access — the registry must
  contain a matching entry with a valid, existing path.
- Only roots registered through the trusted worktree lifecycle are
  resolvable.
- Removed worktree roots return 410 Gone, not the path.

## Backward Compatibility

- All existing `workspace://` URIs without `?root=` continue to resolve to
  the canonical workspace anchor.
- `ExecutionSnapshot.execution_roots` defaults to empty vec; existing
  deserialization is unaffected.
- The HTTP file API `?execution_root_id=` query param continues to work.
