# Server transport boundary

The private holon-server crate owns TCP and Unix socket serving and the
shared HTTP layers. The holon application passes an assembled Axum router,
configuration and shutdown signal into the crate. TCP request handlers
receive peer addresses through Axum ConnectInfo.

The server crate owns transport-level behavior for requests and responses.
This includes browser origin and credential header parsing, JSON rejection
and error envelopes, request trace context, conditional file responses, the
avatar renderer and cache, and SSE framing, heartbeats and public event
encoding. Callers supply already-open files, authorized avatar identities and
admitted event streams. Request authentication, file and workspace access,
session admission, event replay and subscriptions, and runtime error
classification are decided by holon-runtime.

The server crate resolves and serves the Web and App SDK bundles from their
existing repository build paths. A fresh checkout can build without either
artifact. A configured Web distribution override remains authoritative for
Web requests.

The private `holon-http-api` crate assembles the concrete OpenAPI document
and the runtime status inventory. It uses typed wire contracts and product
version information from `holon-runtime` and OpenAPI generation primitives
from `holon-server`. The application exports both documents at their
existing paths.

Holon-runtime registers the routes and implements handlers that compose
RuntimeHost, RuntimeDb, and daemon services. The HTTP API crate consumes
runtime contracts through a one-way dependency.
