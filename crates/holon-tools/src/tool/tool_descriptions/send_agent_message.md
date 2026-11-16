Send one durable asynchronous message to an existing authorized agent.

Sending succeeds when the runtime persistently accepts the delivery. It does
not create a task handle, wait for a reply, or mean that the target finished
any business work. The recipient may respond with its own `SendAgentMessage`
call.

The runtime binds the caller identity, provenance, authority, and idempotency
key. Do not include a reply handle or attempt to claim another sender identity.

Optional `reply_context_id` selects a runtime-provided original message ID
admitted in the current live turn; it cannot supply sender, correlation, or
authority. One eligible incoming route preserves implicit replies. With multiple
routes, replies and forwarding require an explicit selector; ambiguity errors
list the eligible IDs. Unknown, foreign-turn, and terminal selectors are rejected.

Set `forward` to `true` only when intentionally relaying the selected incoming
agent message to a different agent. Without it, routing metadata is inherited
only when sending to that message's trusted sender or reply target; unrelated
notifications start a new direct route, even with a selector. Pure task results
do not provide agent-message routes. Routing identities remain runtime-bound.
