# Subagent V1 Shape

Decision:

- implement `subagent_task` as a bounded in-process background agent
- reuse the same provider and task/result rejoin path
- avoid nested task creation in the first implementation

Reason:

- subagents should stay orchestration units, not a separate runtime
- bounded V1 behavior avoids uncontrolled recursion
