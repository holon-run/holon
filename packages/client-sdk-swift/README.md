# Swift client SDK

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
The SDK has no third-party runtime dependencies or generated HTTP client.
`HolonClient` is an actor with stable `Sendable` handshake, roster, session, user
and lossless error adapters. It is not a platform credential store or UI/sync
coordinator.

## Transport ownership

`HolonEndpoint` takes the **complete API base URL**, such as
`https://host.example/proxy/holon/api`; it preserves that prefix and encodes each
route segment separately. HTTPS uses standard certificate validation. HTTP is
permitted for loopback development; other HTTP hosts require the caller's explicit
`allowInsecureHTTP` confirmation. URL user info, query/fragment and traversal
prefixes are rejected. HTTP redirects are errors, not credential transfers.
Sessions use no shared cookies, credential storage or disk cache.

Every response carries network/runtime/user/visibility identity and a unique
connection generation. `bindIdentity` cancels old HTTP/SSE tasks and rejects late
HTTP results; consumers must compare the returned identity before publishing
buffered stream events. A different base URL requires a different client.
Credential exchange returns a redacted session value without persisting or
installing it. A transport failure or ordinary 403 never clears credentials.

HTTP reads and SSE connection establishment use no retries by default.
`HolonRetryPolicy` explicitly enables a bounded, cancellable transient retry
budget; an optional observer exposes the scheduled attempt and delay.
POST exchanges/revocations never retry automatically. TLS, protocol, permission
and cancellation failures are not retried.

`openEventStream` owns one foreground connection. Its returned stream must be
closed when the owner leaves the foreground/selection or changes networks.
Frame size and event buffering are bounded: overflow fails rather than silently
dropping frames. An incomplete EOF frame is not published. EOF is an explicit
`streamEnded` error; callers bootstrap/reopen deliberately. The transport does
not deduplicate events, commit a replay checkpoint, rebuild projections or hide
a reconnect loop. `Last-Event-ID` is caller supplied; it is not a history cursor.

`make ios-sdk-test` compiles the package and reads fixtures directly from
`tests/fixtures/client-wire`; tests must not copy them into a second resource tree.
Live tests skip unless explicitly enabled by `make ios-contract-test`. That entry
builds the current checkout with `cargo build --bin holon` by default. An explicit
`HOLON_CONTRACT_BIN` can select another binary, but its result does not verify
the current Rust source. The entry starts only an isolated
loopback Rust daemon with a temporary HOME, no provider credentials or production
agents, and cleans up its own process and files on exit.

The independent P0 `LiveDaemonProbeTests` use URLSession directly: real handshake
and fresh default-agent roster decoding, HTTP requests through a real prefix-stripping
proxy, HTTPS rejection against a plaintext endpoint, SSE connection closure,
fresh bootstrap, reconnection and explicit cancellation. The proxy deliberately
closes a connected upstream SSE response without synthesizing events.
P1 additionally verifies production SDK handshake/roster through the real
prefix proxy, explicit SSE EOF/bootstrap/reopen and stream close. Unit tests
cover fixtures, identity rejection, cancellation, bounded retry and parsing.
CI runs `make ios-wire-check` and `make ios-sdk-test` on a macOS Swift toolchain.
These checks are not an event replay, successful HTTPS/certificate-trust,
or iOS ATS/device acceptance test.
No TLS validation bypass or production daemon reconfiguration is performed.
