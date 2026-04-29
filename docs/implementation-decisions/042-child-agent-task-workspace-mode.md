# Child Agent Task Workspace Mode

Runtime-created supervised child work uses a single `child_agent_task` kind.
Inherited-workspace and worktree-isolated delegation are separated by
`workspace_mode = inherit | worktree` in task detail and recovery metadata.

Legacy `subagent_task` and `worktree_subagent_task` records remain readable for
restart recovery and old local state, but new runtime creation does not use
those kinds. This preserves recovery compatibility while keeping task kind from
encoding workspace ownership.
