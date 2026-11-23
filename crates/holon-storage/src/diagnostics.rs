//! Application-supplied instrumentation for SQLite operations.

use std::time::{Duration, Instant};

use rusqlite::Connection;

pub use holon_domain::diagnostics::{ReaderPoolEvent, ReaderPoolOutcome, ReaderPoolRole};

#[derive(Clone, Copy)]
pub enum StorageStage {
    Connection,
    SqliteOpen,
    Configure,
    Sidecar,
    ReaderCheckout,
    ReaderLease,
    ReaderOperation,
    ReaderAdmission,
    ReaderBlockingAdmission,
    RosterLatestBriefs,
    AgentLatestBrief,
}

/// Observers travel with the connection owner and are shared by its readers.
pub struct StorageDiagnostics {
    pub record_open_attempt: fn(),
    pub observe_connection: fn(&Connection) -> rusqlite::Result<()>,
    pub record_open_duration: fn(Duration),
    pub record_sidecar_scan: fn(Duration),
    pub record_stage: fn(StorageStage, Duration),
    pub reader_pool: fn(ReaderPoolRole, ReaderPoolEvent),
}

pub static NO_DIAGNOSTICS: StorageDiagnostics = StorageDiagnostics {
    record_open_attempt: || {},
    observe_connection: |_| Ok(()),
    record_open_duration: |_| {},
    record_sidecar_scan: |_| {},
    record_stage: |_, _| {},
    reader_pool: |_, _| {},
};

impl StorageDiagnostics {
    pub(crate) fn start(&self, stage: StorageStage) -> StageTimer {
        StageTimer {
            record: self.record_stage,
            stage,
            started: Instant::now(),
        }
    }
}

pub(crate) struct StageTimer {
    record: fn(StorageStage, Duration),
    stage: StorageStage,
    started: Instant,
}

impl Drop for StageTimer {
    fn drop(&mut self) {
        (self.record)(self.stage, self.started.elapsed());
    }
}
