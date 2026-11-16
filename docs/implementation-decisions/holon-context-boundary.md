# Context and prompt rendering in holon-context

`holon-context` owns deterministic planning, history window selection,
recent Turn and archived episode rendering, prompt sections, tool guidance,
and prompt fingerprints. It receives the already captured records and
borrows the qualified history observations. The budget planner and the
legacy JSON fingerprint encoding retain their existing behavior.

The application owns resident state and storage capture, evidence and
replay qualification, resource observations and their allocation guards,
and provider requests. Context and prompt entry points still capture and
assemble the application snapshot. The renderer uses that snapshot without
performing additional reads or retaining new copies of the source records.

Run the independent suite with
`cargo test --locked --manifest-path crates/holon-context/Cargo.toml`.
The root suites continue to check capture, reprojection, and the effective
prompt built from runtime-owned state.
