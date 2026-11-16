# SQLite memory projection in holon-memory-index

`holon-memory-index` owns the `memory.v2.sqlite3` schema, FTS search,
read-only and writer connections, the per-path writer coordinator, and
persisted rebuild progress. It works with prepared documents and source
changes. The application supplies its existing writer metrics callbacks.

The application resolves canonical sources through `AppStorage` and
`RuntimeDb`, drives bounded rebuild passes, and reports freshness from both
databases. It resolves exact `MemoryGet` content through runtime references.

The index applies an outbox change and updates its applied cursor in the
same SQLite transaction. The application acknowledges the committed prefix
in the runtime outbox, retaining a failed row and its successors for retry.
Rebuild finalization similarly commits checkpoints and the applied cursor
before the application acknowledges rows covered by the rebuild.

Run the store and coordinator tests with
`cargo test --locked --manifest-path crates/holon-memory-index/Cargo.toml`.
The root memory tests exercise canonical reads, outbox retries, and rebuild
orchestration against both stores.
