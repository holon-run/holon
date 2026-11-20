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

The storage crate also performs verified backups, WAL checkpoints, and
vacuum operations with the application's protected connections. Backup
preflight runs before acquiring the source connection. RuntimeDb acquires
the offline maintenance lock and migrates the database before compaction.
Online retention commits canonical removals and publishes their deltas before
the separate space-reclamation pass.

Agent registry storage owns identity rows and reservations, canonical
relation persistence and legacy projection, backfill checkpoints, and Agent
list facts. Identity-fenced list reads use the original deferred transaction
for both the identity and its records. The runtime selects the history
window and owns the read lease and Agent posture projection.

The runtime supplies the original resource accounting callbacks before
storage allocation and retains returned material guards. It composes
identity, episode continuity, audit and lifecycle changes in the same
transaction, and owns the per-Agent transaction sequence for relation
backfill.

Run the independent storage tests with
`cargo test --locked --manifest-path crates/holon-storage/Cargo.toml`.
Root tests continue to cover the facade, runtime-qualified upgrades, and
cross-repository commit behavior.
