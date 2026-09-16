Send one durable asynchronous message to an existing authorized agent.

Sending succeeds when the runtime persistently accepts the delivery. It does
not create a task handle, wait for a reply, or mean that the target finished
any business work. The recipient may respond with its own `SendAgentMessage`
call.

The runtime binds the caller identity, provenance, authority, and idempotency
key. Do not include a reply handle or attempt to claim another sender identity.
