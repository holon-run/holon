Read an agent-plane summary, including identity, lifecycle, active work focus,
waiting state, and child-agent lineage. The default response is a compact
runtime summary. Pass `{"detail":"full"}` when complete execution, skills,
loaded guidance, token usage, and diagnostic fields are required.

When called without arguments, returns the current agent's summary. When called
with `agent_id`, returns the requested agent's summary. Read-only inspection
does not start an unloaded target runtime.
