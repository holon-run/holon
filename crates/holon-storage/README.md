# holon-storage

SQLite storage for Holon. This crate owns connection protection, reader pools,
the writer queue, schema upgrades, and transaction-scoped Conversation,
Evidence, Index Outbox, and runtime record operations. The runtime facade
supplies application authorization, resource admission, and upgrade
qualifications.

Run the backend tests with
`cargo test --locked --manifest-path crates/holon-storage/Cargo.toml`.
