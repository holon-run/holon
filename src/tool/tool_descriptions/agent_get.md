Read an agent-plane summary, including identity visibility, ownership, profile preset, lifecycle, active work focus, waiting state, and visible child-agent lineage.

When no agent_id is provided, returns the current agent summary.
When agent_id is provided, returns the summary for the specified agent.

Under the current simplified trust model, local/operator control API access
is trusted, so private child agents may be observed by knowing their agent_id.
Remote/multi-user authorization is deferred to future work.
