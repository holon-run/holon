# 043 Task-Owned Worktree Cleanup

Runtime-created worktrees for `child_agent_task` with
`workspace_mode = worktree` are task-owned artifacts.

Holon no longer exposes a dedicated destructive model-facing discard tool for
these artifacts. The supervising task records worktree path, branch, changed
files, and cleanup status in task detail metadata. Terminal child task cleanup
and `TaskStop` both run the same best-effort cleanup path:

- clean worktrees are removed with their ephemeral task branch
- already-removed paths are treated as completed cleanup
- changed or mismatched worktrees are retained and recorded with an audit event

This keeps `ExitWorkspace` non-destructive, keeps child-agent execution separate
from artifact ownership, and leaves manual early cleanup to ordinary git
commands in the local environment.
