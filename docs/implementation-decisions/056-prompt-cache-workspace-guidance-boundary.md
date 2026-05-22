# 056 Prompt Cache Workspace and Guidance Boundary

## Choice

Prompt cache identity is tied to rendered prompt content, tool surfaces, loaded
guidance content, and execution fields that affect visible prompt text or
relative tool behavior. Workspace bookkeeping ids are not semantic cache inputs
unless they are rendered to the model or change execution semantics.

Guidance is loaded as three explicit layers at prompt assembly time:

1. global operator guidance from `~/.agents/AGENTS.md` when present;
2. agent-home guidance from `<agent_home>/AGENTS.md` when present;
3. active workspace guidance from `<workspace_anchor>/AGENTS.md`, falling back
   to `<workspace_anchor>/CLAUDE.md` only when `AGENTS.md` is absent.

`UseWorkspace` changes active workspace state. The next prompt assembly reloads
the guidance stack from disk; switching workspaces replaces only the workspace
layer and does not suppress global or agent-home guidance.

## Reason

Prompt caches should invalidate when model-visible instructions, loaded guidance
content, tool contracts, paths, cwd, projection mode, access mode, or worktree
roots change. They should not invalidate merely because an internal
`execution_root_id` changes while the rendered prompt and tool semantics remain
the same.

Keeping guidance as layered prompt sections makes repeated workspace switching
legible: unchanged global and agent-home layers stay stable, while the active
workspace layer changes or refreshes when its file content changes.

## Boundary

`workspace_id` remains in the cache payload while it is rendered in the
execution environment summary. `execution_root_id` remains available in runtime
state and diagnostics, but it is not part of prompt cache identity because it is
bookkeeping rather than a relative-path or projection semantic.
