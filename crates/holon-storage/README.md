# holon-storage

SQLite storage for Holon. This crate owns connection protection, reader pools,
the writer queue, schema upgrades, and transaction-scoped Conversation,
Evidence, Index Outbox, Observer Sync, and runtime record operations. It owns
Episode continuity seeds, bindings and qualification queries, storage domain
checkpoints, Brief read cursors and unread counts, authentication records,
content reports, Operator Inbox projections, and historical database audits.
The runtime facade owns connection and transaction selection, upgrade
coordination, receipt validation, resource admission, and recipient policy.

Run the backend tests with
`cargo test --locked --manifest-path crates/holon-storage/Cargo.toml`.
