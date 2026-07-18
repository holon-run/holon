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

Use `GetWorkspaceState` before acting when workspace identity, retained
worktrees, or occupancy is uncertain. Use `AttachWorkspace` only to add a new
workspace binding; it does not switch.

Use `SwitchWorkspace` to activate an existing attached workspace or registered
execution root. Provide exactly one of `workspace_id`, `execution_root_id`, or
`path`. A Git subdirectory resolves to its worktree root while remaining the
default cwd. A linked worktree belongs to its canonical origin workspace.
`SwitchWorkspace` never attaches a new repository or creates a worktree.

Use `CreateWorktree` with explicit `workspace_id`, `branch`, and `base_ref`.
It creates and activates by default. A unique live worktree for the branch may
be safely reused, but existing branch-only or ambiguous state is a conflict and
must not be reset or forced.

Use `RemoveWorktree` for safe registered cleanup. It refuses dirty, locked,
unregistered, canonical, or occupied roots. An active worktree requires
`return_to`. Use `DetachWorkspace` to remove a binding; active detach first
returns to `agent_home`, and retained worktree artifacts are not deleted.

`UseWorkspace` is a deprecated compatibility alias and should not be used in
new workflows.

Shell `cd` affects only that shell command process. It does not redefine the active workspace, instruction root, AGENTS.md loading scope, or relative ApplyPatch base. Switching workspaces does not delete files, remove bindings, or clean up retained isolated roots; cleanup is a separate explicit lifecycle action.
