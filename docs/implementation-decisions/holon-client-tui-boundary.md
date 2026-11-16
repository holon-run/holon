# Client and terminal crate ownership

`holon-client` owns HTTP, Unix-socket, and SSE transport and the shared
request and response types it consumes. The root crate derives connection
settings from `AppConfig`. HTTP handlers continue to assemble the shared
response values from runtime observations.

`holon-tui` owns the operator console, onboarding terminal, display
projections, and presentation reducer. The CLI supplies the client and launch
options, or the onboarding choices. Runtime startup, state observation,
authentication, and application of onboarding settings remain in the daemon
and application.

Run the independent suites with

```sh
cargo test --locked --manifest-path crates/holon-client/Cargo.toml
cargo test --locked --manifest-path crates/holon-tui/Cargo.toml
```

The root HTTP, CLI, and OpenAPI checks continue to exercise the shared
contract, including the serialized payloads and transport selection.
