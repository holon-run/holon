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
