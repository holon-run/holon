# SQLite storage in holon-storage

`holon-storage` depends on `holon-domain` for persistent record types and
Conversation pagination contracts. It owns SQLite connection setup and
sidecar protection, bounded reader pools, serialized writes, and transaction
failure classification. Readers retain the writer's file identity and protection
owner. Transaction observers and workspace registry publication share the
writer queue.

The storage crate owns Conversation queries, canonical Evidence and Index
Outbox rows, Task and WorkItem revision checks, and the SQLite writes for Turn,
Wait, Agent state, and execution protocol partitions. It also owns schema
definitions, the migration ledger, the release baseline, and historical
schema reconciliation.

`holon::runtime_db` leases connections and composes the queries and writes
inside the existing transactions. It binds Conversation cursors to the caller
and active identity. It supplies the resource checks at their original read
and write sites and retains ownership of runtime admission, publication, and
resident state. During a schema upgrade, the storage engine invokes runtime
qualification for the matching historical data inside the same transaction,
before it records the migration. Errors roll back the schema and the capture
together.

Run the independent storage tests with
`cargo test --locked --manifest-path crates/holon-storage/Cargo.toml`.
Root tests continue to cover the facade, runtime-qualified upgrades, and
cross-repository commit behavior.
