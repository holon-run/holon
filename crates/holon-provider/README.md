# holon-provider

Internal model provider adapters for Holon. This crate is published to
support installing Holon through Cargo. Its interface evolves with Holon.

The Chat Completions adapter owns the wire request and response.
Holon supplies resolved configuration, request material and guarded HTTP
execution. The reusable model client lives in `holon-model-client`.

Run the independent tests with `cargo test --locked --manifest-path
crates/holon-provider/Cargo.toml`.
