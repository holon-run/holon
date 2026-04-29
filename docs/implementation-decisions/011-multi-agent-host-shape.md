# Multi-Agent Host Shape

Decision:

- use a `RuntimeHost` registry that lazily creates per-agent runtimes
- keep agent storage under `.holon/agents/<agent_id>/`
- keep one runtime loop per agent

Reason:

- this is the minimum clean shape for multi-agent isolation
- it avoids overloading a single runtime with cross-agent branching
