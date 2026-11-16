Create a linked Git worktree for an attached workspace from an explicit branch
and base_ref. By default the runtime activates it. If exactly one live
worktree already checks out the branch, on_existing=reuse safely reuses it
without applying base_ref, reset, force, or checkout; conflicts are returned
without changing Git or active state.
