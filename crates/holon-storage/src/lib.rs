//! SQLite infrastructure for Holon's runtime storage.

pub mod blocking;
pub mod connection;
pub mod diagnostics;
pub mod persistence;
pub mod read_block;
mod read_scope;
mod reader_config;
pub mod reader_pool;
pub mod workspace_context;
pub mod write_queue;

mod errors;

use std::time::Duration;

pub use errors::{
    RuntimeDbProtectionError, RuntimeDbProtectionState, RuntimeDbProtectionStatus,
    RuntimeDbRetryableError, RuntimeStateTransitionConflict,
};
pub use read_scope::RuntimeDbReadScope;
pub use reader_config::{
    RuntimeDbReaderConfig, DEFAULT_ORDINARY_MAX_CONNECTIONS, DEFAULT_PREPARATION_MAX_CONNECTIONS,
    MAX_READER_CONNECTIONS,
};

pub const RUNTIME_DB_BUSY_TIMEOUT: Duration = Duration::from_millis(30_000);
const RUNTIME_DB_TRANSACTION_RETRY_INITIAL_DELAY: Duration = Duration::from_millis(25);
const RUNTIME_DB_TRANSACTION_RETRY_MAX_DELAY: Duration = Duration::from_millis(1_000);
const RUNTIME_DB_BEGIN_RETRY_WARN_INTERVAL: Duration = Duration::from_secs(30);
const RUNTIME_DB_TRANSACTION_RETRY_WARN_INTERVAL: Duration = Duration::from_secs(30);
const RUNTIME_DB_APPEND_RETRY_MAX_DELAY: Duration = Duration::from_millis(5_000);
const RUNTIME_DB_WRITE_QUEUE_CAPACITY: usize = 1024;
