# Web search and retrieval in holon-web-retrieval

`holon-web-retrieval` owns WebSearch routing, local and remote WebFetch, and
their network policies and response limits. Local fetch checks each redirect
and pins the HTTP client to the approved addresses for that hop. Remote
fetch follows the existing explicit provider contract.

The application resolves credentials and authorizes tool calls. It passes
the resolved web configuration and its runtime error classifier to the
retrieval code. Public application entry points retain their existing paths.

Run the independent suite with
`cargo test --locked --manifest-path crates/holon-web-retrieval/Cargo.toml`.
