# holon-storage

SQLite connection setup, bounded readers, serialized writes, and transaction
failure classification for Holon. The application's schema migrations and
runtime repositories use this infrastructure through `holon::runtime_db`.

Run the backend tests with
`cargo test --locked --manifest-path crates/holon-storage/Cargo.toml`.
