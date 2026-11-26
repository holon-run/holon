//! SQLite infrastructure for Holon's runtime storage.

pub mod agent_registry;
pub mod agent_state;
pub mod audit;
pub mod authentication;
pub mod blocking;
pub mod codec;
pub mod connection;
pub mod content_reports;
pub mod conversation;
pub mod diagnostics;
pub mod episode_continuity;
pub mod evidence;
pub mod execution;
pub mod index_outbox;
#[doc(hidden)]
pub mod legacy_scheduler_wire;
pub mod maintenance;
pub mod migrations;
pub mod notifications;
pub mod observer_sync;
pub mod persistence;
pub mod read_block;
mod read_scope;
mod reader_config;
pub mod reader_pool;
pub mod reclamation;
pub mod retention_config;
pub mod retired_scheduler_cleanup;
pub mod schema;
pub mod sequences;
pub mod storage_domain;
pub mod task;
pub mod turn;
pub mod unread;
pub mod usage;
pub mod wait;
pub mod work_item;
pub mod workspace_context;
pub mod write_callback_phases;
pub mod write_queue;

mod errors;

use std::time::Duration;

pub use errors::{
    ReaderReprepareRequested, RuntimeDbProtectionError, RuntimeDbProtectionState,
    RuntimeDbProtectionStatus, RuntimeDbRetryableError, RuntimeStateTransitionConflict,
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

#[cfg(test)]
mod tests;
