Use GetAgent for agent-plane inspection. It returns the current agent when
`agent_id` is omitted, or the requested agent when `agent_id` is provided.
The summary includes identity, active work focus, waiting state, execution
snapshot, and child-agent lineage. This is a read-only query and does not start
an unloaded target runtime. Prefer TaskStatus when inspecting a managed task
handle such as a command task or an InvokeAgent result. Do not use GetAgent as
a transcript dump or as a substitute for TaskOutput.
