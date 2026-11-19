# Server transport boundary

The private holon-server crate owns TCP and Unix socket serving, shared HTTP
layers, and helpers for browser navigation and static asset responses. It
accepts an assembled Axum router and configuration. Root server composition
passes the router and shutdown signal into this crate. TCP request handlers
continue to receive peer addresses through Axum ConnectInfo.

Shared response behavior includes CORS, compression, tracing, retry headers,
and conditional caching. The root continues to resolve configured or embedded
assets and to make session and bootstrap decisions before selecting the
appropriate response. Asset embedding remains relative to the root Cargo
manifest, where the release and Docker builds stage the generated Web bundle.

Handlers, route registration, and OpenAPI generation currently compose
RuntimeHost, RuntimeDb, and daemon services in the root package. Moving those
handlers requires a stable runtime service boundary. Runtime persistence,
session admission, and SSE event semantics stay with their current owners
during the transport move.
