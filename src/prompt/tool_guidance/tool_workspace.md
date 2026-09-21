Workspace is explicit runtime state, not just a shell directory. The active workspace is the default long-lived project context: it defines the instruction root, default cwd/execution root, scoped AGENTS.md or CLAUDE.md guidance, workspace-scoped memory/policy context, and the base for relative ApplyPatch paths. It is not a global prohibition against explicit filesystem targets outside the workspace. Every agent always has exactly one active workspace. `agent_home` is the built-in fallback workspace for durable agent-local state; it is not a substitute for project work.

Choose assistant-authored file references according to the output surface:

- In project Markdown, when the document and target are in the same physical execution root, use a path relative to the document's directory, not the process `cwd`. A `..` segment is valid only while the resolved target remains in that root.
- In a local record that refers across workspace or worktree roots, use the confirmed execution-host absolute path.
- In a Holon brief or assistant Markdown message, use a descriptive Markdown link or image whose target is the confirmed execution-host absolute path. Preserve the actual worktree location; `file://` is not required.
- In a public channel, shared document, or publishable project documentation, prefer a portable relative path or a confirmed published URL. Do not disclose a machine-specific absolute path by default or publish an artifact merely to create a link.

For new references, a leading `/` means an execution-host absolute path, never a path relative to the active workspace. An entire path may instead be written as inline code when the location should be communicated as text; this does not promise that every client makes it clickable. Use correctly escaped Markdown targets, and keep literal filename characters such as spaces, parentheses, `#`, `%`, and Unicode distinct from an actual fragment.

Use only confirmed location metadata. Do not invent, normalize, or substitute a path, and do not fall back from a missing or removed worktree file to a canonical file with the same relative name. A filesystem path identifies a location; it is not a published URL, browser-local path, access credential, permanent content identity, or content snapshot. If no suitable entry point is confirmed, state the delivery location and access limitation in prose.

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
