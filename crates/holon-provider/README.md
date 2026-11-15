# holon-provider

Internal model provider implementations for Holon. This crate is published to
support installing Holon through Cargo. Its interface evolves with Holon.

The crate owns OpenAI Chat Completions, OpenAI Responses, Codex Responses,
Anthropic Messages, and Gemini GenerateContent. It also owns transport
timeouts, HTTP tracing, credential refresh, request lowering, image
generation, continuation, retry classification, quota coordination, and model
discovery.

The application resolves endpoints and model policy, checks runtime admission
before dispatch, and supplies the resource observations that follow requests
and results. The reusable model client lives in `holon-model-client`.

Run the independent tests with:

```sh
cargo test --locked --manifest-path crates/holon-provider/Cargo.toml
```
