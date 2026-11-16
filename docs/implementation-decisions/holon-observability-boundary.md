# Runtime instrumentation in holon-observability

`holon-observability` owns process metrics, stage attribution, the recent
trace buffer and diagnostics SQLite writer, OTLP export, and OpenMetrics
rendering. These facilities share one set of process-local counters, recent
traces, and writer handles.

The application owns startup and shutdown, control route authorization, and
resource reads that participate in runtime input budgeting. Its diagnostics
module converts a RuntimeDb WAL checkpoint report into a metrics snapshot.
The observability crate consumes the snapshot through its metrics API.

Shared trace and metrics DTOs live in `holon-domain`. The client re-exports
them through `holon_client::wire`, and the application keeps its existing
diagnostics, trace, and exporter module paths.

Run the instrumentation tests independently with
`cargo test --locked --manifest-path crates/holon-observability/Cargo.toml`.
