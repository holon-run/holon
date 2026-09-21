Safely remove a registered linked Git worktree. Provide exactly one of
`execution_root_id` or `path`; paths must resolve to a unique registered
worktree in an attached workspace. The runtime refuses arbitrary paths,
canonical roots, dirty or locked worktrees, and roots occupied by another
agent/task. An active target requires `return_to`. Branch deletion is optional
and only occurs when merge ancestry is proven.
