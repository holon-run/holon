use std::path::Path;

use anyhow::Result;
use rusqlite::Connection;

use super::storage_diagnostics::DIAGNOSTICS;

pub use holon_storage::connection::{is_retryable_db_error, is_sqlite_locked};

pub(crate) use holon_storage::connection::{
    configure_new_database_auto_vacuum, configure_persistent_database,
};

pub(crate) fn open_connection(path: &Path) -> Result<Connection> {
    holon_storage::connection::open_connection(path, &DIAGNOSTICS)
}

pub(crate) fn ensure_runtime_db_sidecars_are_consistent(path: &Path) -> Result<()> {
    holon_storage::connection::ensure_runtime_db_sidecars_are_consistent(path, &DIAGNOSTICS)
}
