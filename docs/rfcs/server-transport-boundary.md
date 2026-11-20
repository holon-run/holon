# Server transport boundary

The private holon-server crate owns TCP and Unix socket serving and the
shared HTTP layers. Root server composition passes an assembled Axum router,
configuration and shutdown signal into the crate. TCP request handlers
receive peer addresses through Axum ConnectInfo.

The server crate owns transport-level behavior for requests and responses.
This includes browser origin and credential header parsing, JSON rejection
and error envelopes, request trace context, conditional file responses, the
avatar renderer and cache, and SSE framing, heartbeats and public event
encoding. Callers supply already-open files, authorized avatar identities and
admitted event streams. Request authentication, file and workspace access,
session admission, event replay and subscriptions, and runtime error
classification are decided by the root runtime.

The server crate resolves and serves the Web and App SDK bundles from their
existing repository build paths. A fresh checkout can build without either
artifact. A configured Web distribution override remains authoritative for
Web requests.

OpenAPI route metadata and document generation live in the server crate.
The root supplies its API version, runtime limits, and schemas for the
concrete endpoint types. Route registration and handlers stay with the root
while they compose RuntimeHost, RuntimeDb, and daemon services.
