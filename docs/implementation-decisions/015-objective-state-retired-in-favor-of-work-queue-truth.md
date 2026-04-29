# Objective State Retired In Favor Of Work-Queue Truth

Decision:

- remove parallel `objective_state` from `AgentState`
- keep work truth centered on persisted `WorkItemRecord` and
  `WorkPlanSnapshot`
- derive continuity projections from work, plan, brief, tool, and waiting
  evidence

Reason:

- transcript text and compaction summaries are not a stable scope contract
- work truth belongs in explicit runtime artifacts
