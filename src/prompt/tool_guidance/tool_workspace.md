Workspace is explicit runtime state, not just a shell directory. The active workspace is the default long-lived project context: it defines the instruction root, default cwd/execution root, scoped AGENTS.md or CLAUDE.md guidance, workspace-scoped memory/policy context, and the base for relative ApplyPatch paths. It is not a global prohibition against explicit filesystem targets outside the workspace. Every agent always has exactly one active workspace. `agent_home` is the built-in fallback workspace for durable agent-local state; it is not a substitute for project work.

`workspace://<workspace_id>/<relative/path>` is Holon's Markdown/file-reference URI for files inside an attached workspace, including agent-home workspace ids such as `agent_home:<agent_id>`. Treat it as a local workspace reference, not a remote URL. The path is percent-decoded relative to the named workspace root and must not be absolute or escape with `..`. Use this form for durable Markdown references to local media when the runtime, provider lowering, or Web GUI needs to resolve the file with workspace authority/auth instead of relying on unauthenticated HTML file URLs.

When the agent operates in a git worktree (not the canonical workspace root),
file references may include an optional `?root=<execution_root_id>` query
parameter: `workspace://<workspace_id>/<path>?root=<execution_root_id>`.
This parameter is an opaque server-issued token that identifies the specific
worktree execution root. When absent, the URI resolves to the canonical
workspace anchor (backward compatible). When present, the resolver looks up
the root in the runtime's execution root registry — the value is never parsed
for path information. A future tool that writes to a worktree root should
include `?root=` by calling `build_execution_root_id` and appending it.

Use UseWorkspace to make the right workspace active when you will inspect, edit, or verify a project over more than a one-off explicit path. Call `UseWorkspace({"path":"/repo/or/subdir"})` when the operator gave you a project path or you need to discover/adopt a directory. Call `UseWorkspace({"workspace_id":"agent_home"})` to return to AgentHome, or `UseWorkspace({"workspace_id":"ws-..."})` to switch to a known workspace id from agent state. Provide exactly one of `path` or `workspace_id`. Use `mode="isolated"` only when you need a runtime-managed isolated execution root, and provide an `isolation_label` as an intent/branch hint rather than inventing a worktree path.

Shell `cd` affects only that shell command process. It does not redefine the active workspace, instruction root, AGENTS.md loading scope, or relative ApplyPatch base. Switching workspaces does not delete files, remove bindings, or clean up retained isolated roots; cleanup is a separate explicit lifecycle action.
