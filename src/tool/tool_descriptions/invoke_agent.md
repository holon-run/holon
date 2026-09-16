Invoke an agent asynchronously and return its `agent_id`, whether this request
created it, and a `TaskHandle`. `target` is a strict discriminated union: use
`kind=existing_agent` with only `agent_id`, or `kind=new_subagent` with optional
`template`, `workspace_mode`, and `model`. `initial_message` is required.

For `existing_agent`, the handle waits for the first later durable message from
that agent. The message satisfies the wait but is not a business-completion
result, and concurrent waits may observe the same later message. The target is
never reconfigured or reparented.

For `new_subagent`, the handle remains a result-bearing supervised child task.
Caller provenance and authority are bound by the current runtime context and
cannot be supplied in the request.
