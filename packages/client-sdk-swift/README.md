# Swift client foundation

`HolonWire` is generated model-only transport code. The repository's OpenAPI
document remains the source of truth. Run `make transport-types` to regenerate
all transports, or `node web-gui/openapi-tools/generate-swift-wire.mjs` after
installing the OpenAPI tools. `make ios-wire-check` verifies Swift drift without
rewriting committed models.

`HolonClient` is the stable, UI-independent boundary. The initial `WireDocument`
decodes a typed model **and** an open JSON tree: generated enum fallback values
cannot erase the original future enum string, error extension or unknown field.
Domain adapters must preserve those raw values rather than re-encoding a typed
generated model as though it were the original document.

The pinned Swift5 model generator is compiled in Swift 5 language mode;
client/domain code uses Swift 6. Generated reference/transport types are not
implicitly `Sendable` and must not become the shared mutable state of an actor.
The SDK has no third-party runtime dependencies and contains no generated HTTP
client, authentication, hidden retry loop or platform credential store.

`make ios-sdk-test` compiles the package and reads fixtures directly from
`tests/fixtures/client-wire`; tests must not copy them into a second resource tree.
Live tests skip unless explicitly enabled by `make ios-contract-test`. That entry
builds the current checkout with `cargo build --bin holon` by default. An explicit
`HOLON_CONTRACT_BIN` can select another binary, but its result does not verify
the current Rust source. The entry starts only an isolated
loopback Rust daemon with a temporary HOME, no provider credentials or production
agents, and cleans up its own process and files on exit.

The independent `LiveDaemonProbeTests` use URLSession directly: real handshake
and fresh default-agent roster decoding, HTTP requests through a real prefix-stripping
proxy, HTTPS rejection against a plaintext endpoint, SSE connection closure,
fresh bootstrap, reconnection and explicit cancellation. The proxy deliberately
closes a connected upstream SSE response without synthesizing events.
These P0 probes are not a P1 SDK client, retry implementation, event replay
test, successful HTTPS/certificate-trust test, or iOS ATS/device acceptance.
No TLS validation bypass or production daemon reconfiguration is performed.
