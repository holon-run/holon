Create a linked Git worktree for an attached workspace from an explicit branch
and base_ref. By default the runtime activates it. If exactly one live
worktree already checks out the branch, on_existing=reuse safely reuses it
without applying base_ref, reset, force, or checkout; conflicts are returned
without changing Git or active state.

An optional absolute `path` sets the exact target directory of this worktree:
nothing is appended to it and `label` does not move it. Omitted, null, or
blank `path` keeps the runtime-managed default directory and naming. Targets
that already exist (including empty directories) and paths conflicting with
the canonical root, another worktree, or a registered execution root are
rejected before any Git mutation. Relative paths, `~`, and environment
variables are not resolved.
