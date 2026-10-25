Invoke an agent asynchronously and return its `agent_id`, whether this request
created it, and a `TaskHandle`. `target` is a strict discriminated union: use
`kind=existing_agent` with `agent_id` and optional `reply_timeout_ms`, or `kind=new_subagent` with optional
`template`, `workspace_mode`, and `model`. `initial_message` is required.

For `existing_agent`, the handle waits for the first later durable message from
that agent, or a runtime-derived final brief strictly bound to this request.
Explicit messages remain preferred; concurrent waits may observe the same
explicit message. Neither channel proves business completion. The target is
never reconfigured or reparented. `target.reply_timeout_ms` defaults to 1800000
(30 minutes); 0 means unlimited. The deadline survives restart. Expiry fails
only the wait with `reply_timeout`, without stopping the agent or resending work.

For `new_subagent`, the handle remains a result-bearing supervised child task.
Caller provenance and authority are bound by the current runtime context and
cannot be supplied in the request.

A newly invoked reusable subagent is retained after its invocation completes.
The current supervising parent owns cleanup: reuse it while needed, then call
`DeleteAgent` with the current identity incarnation. Creation requires no TTL.
