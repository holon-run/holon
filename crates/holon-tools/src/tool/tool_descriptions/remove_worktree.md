Safely remove a registered linked Git worktree. Provide exactly one of
`execution_root_id` or `path`; paths must resolve to a unique registered
worktree in an attached workspace. Within that scope, cleanup is allowed
regardless of which agent created the worktree; no creator or supervisor grant
is required. Only clean up resources whose task scope permits removal.
The runtime refuses arbitrary paths,
canonical roots, dirty or locked worktrees, and roots occupied by another
agent/task. An active target requires `return_to`, which may be `canonical`,
`agent_home`, an execution-root ID, an attached canonical path, or a registered
worktree path. Branch deletion is optional and only occurs when merge ancestry
is proven.
