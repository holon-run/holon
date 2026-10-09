Read an agent-plane summary, including identity, lifecycle, active work focus,
waiting state, and child-agent lineage. The default response is a compact
runtime summary. Pass `{"detail":"full"}` when complete execution, skills,
loaded guidance, token usage, and diagnostic fields are required.

When called without arguments, returns the current agent's summary. When called
with `agent_id`, returns the requested agent's summary. Read-only inspection
does not start an unloaded target runtime.

For the current agent, `subagent_cleanup` lists up to 16 supervised ephemeral
children and their current database blockers. This is a read-only responsibility
list, not authorization to delete. `observed_since` is absent until background
observation exists. Inspect the target with full detail for `identity.incarnation`
before requesting `DeleteAgent`; filesystem/worktree checks can add blockers.
