Read the current agent-plane summary, including identity visibility, ownership, profile preset, lifecycle, active work focus, waiting state, and visible child-agent lineage.

When called without arguments, returns the current agent's summary (unchanged behavior).
When called with `agent_id`, returns the summary of the requested agent through the local trusted control boundary. This allows inspecting private child agents without exposing them to remote/multi-user access.

Remote/multi-user authorization is deferred to future work; the current behavior relies on the local trusted control API boundary.
