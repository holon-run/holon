Send one durable asynchronous message to an existing authorized agent.

Sending succeeds when the runtime persistently accepts the delivery. It does
not create a task handle, wait for a reply, or mean that the target finished
any business work. The recipient may respond with its own `SendAgentMessage`
call.

The runtime binds the caller identity, provenance, authority, and idempotency
key. Do not include a reply handle or attempt to claim another sender identity.

Set `forward` to `true` only when intentionally relaying the latest incoming
agent message to a different agent. Without it, routing metadata is inherited
only when sending to that message's trusted sender or reply target; unrelated
notifications start a new direct route. Routing identities remain runtime-bound.
