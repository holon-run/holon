# SQLite infrastructure in holon-storage

`holon-storage` owns SQLite connection setup and sidecar protection,
the ordinary and preparation reader pools, serialized writes, and transaction
failure classification. Readers still retain the writer's file identity and
protection owner. Transaction observers and the per-database workspace registry
publication retain the same shared writer queue.

`holon::runtime_db` owns schema migration, repositories, and the rules for
holding the workspace publication across a commit. It supplies the existing
connection probes and stage counters when opening the writer, and the readers
use those same observers. The main crate re-exports its existing database
error and reader types so callers share the originals for downcasting.

Run SQLite infrastructure tests with
`cargo test --locked --manifest-path crates/holon-storage/Cargo.toml`.
Tests that construct the runtime database facade or assert runtime error
classification remain with Holon.
