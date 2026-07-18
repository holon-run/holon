Safely remove a registered linked Git worktree. The runtime refuses arbitrary
paths, canonical roots, dirty or locked worktrees, and roots occupied by
another agent/task. An active target requires return_to. Branch deletion is
optional and only occurs when merge ancestry is proven.
