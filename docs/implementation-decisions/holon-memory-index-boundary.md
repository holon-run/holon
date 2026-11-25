# SQLite memory projection in holon-memory-index

`holon-memory-index` owns the `memory.v2.sqlite3` schema, FTS search,
read-only and writer connections, the per-path writer coordinator, and
persisted rebuild progress. It constructs memory documents from acquired
records, including their source references, searchable excerpts, and metadata.
The application supplies resolved file content and timestamps, its existing
command preview formatter, and writer metrics callbacks. Document layout and
projection version are preserved so existing index rows remain valid.

The application resolves canonical sources through `AppStorage` and
`RuntimeDb`, schedules bounded maintenance, and resolves exact `MemoryGet`
content through runtime references. It supplies prepared source pages and
samples the canonical outbox and dirty marker. The index advances its persisted
rebuild phases, applies bounded source batches, and derives search freshness
from the observations.

The index applies an outbox change and updates its applied cursor in the
same SQLite transaction. The application acknowledges the committed prefix
in the runtime outbox, retaining a failed row and its successors for retry.
Rebuild finalization similarly commits checkpoints and the applied cursor
before the application acknowledges rows covered by the rebuild.

Run the store and coordinator tests with
`cargo test --locked --manifest-path crates/holon-memory-index/Cargo.toml`.
The runtime memory tests exercise canonical reads, outbox retries, and rebuild
orchestration against both stores.
