# First `holon-model-client` extraction slice

## Decision

Start the extraction with an independent `crates/holon-model-client` crate that
owns one model attempt and an OpenAI Chat Completions-compatible transport.
Keep route parsing (`provider@endpoint/model`), catalog lookup, route chains,
fallback, aggregate budgets, transcript/session state, tool execution, and
runtime trace policy in Holon.

## Reason

The current `src/provider` implementation combines wire fidelity with Holon
runtime policy. Publishing that module unchanged would make the reusable crate
depend on Holon internals and would force downstream users to adopt policies
they do not own. A small contract plus one transport gives us an independently
testable boundary without flattening provider-native continuation, cache,
reasoning, or native-search features into a lowest-common-denominator API.

The first transport slice is intentionally non-streaming. Streaming requires a
stable event/partial-response contract and should be extracted only after the
wire contract is validated against the existing Holon fixtures.

## Holon adapter for Chat Completions

The production adapter and the standalone client use the same protocol
encoder. They supply their own wire options. In production the resolved model
and endpoint determine the output-token field, and function tools carry
`tool_choice: auto`. The encoder preserves omitted fields and null assistant
content for tool-only messages.

The internal `holon-provider` crate owns Chat Completions request construction,
endpoint handling, response decoding, error classification, and the provider
context-window and output-token estimates. The root provider translates
Holon messages, validates tool schemas and image material, and coordinates
continuation and diagnostics. The provider adapter calls the root's
transport with the prepared request. The root transport captures the trace,
serializes and reads with the original material, and checks dispatch admission
immediately before execution. It settles the resulting attempt before the
runtime updates continuation state.

The public entry point in Holon supplies resolved configuration and preserves
its existing constructor and AgentProvider interface. Release publishes the
internal provider crate after domain and model-client and before the root crate.
