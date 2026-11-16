# Built-in tools in holon-tools

`holon-tools` owns the built-in tool catalog, names and compatibility routes,
input schemas and decoding, tool descriptions, outcome envelopes, and model
receipt rendering. The patch engine runs in the tool crate over the file-read
and write-admission operations supplied by the caller.

The application implements `ToolRuntime` in a private adapter over `RuntimeHandle`. It passes
the authorized call to the runtime operation for that tool and owns the
execution lease, durable effects, and provider or network access. The tool
registry still checks the capability family before invoking the handler.
Prepared WorkItem settlements, wait settlements, and resource material travel
as typed application state beside the serialized tool outcome. The runtime
keeps their original lifetime and owns when a prepared transition commits.

Run the independent suite with
`cargo test --locked --manifest-path crates/holon-tools/Cargo.toml`. The root
tests exercise the runtime host and the generated tool schema inventory.
