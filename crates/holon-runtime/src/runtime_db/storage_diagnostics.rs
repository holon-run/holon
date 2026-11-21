use std::time::Duration;

use holon_storage::diagnostics::{StorageDiagnostics, StorageStage};

use crate::diagnostics::{self, attribution, io_probe};

pub(super) static DIAGNOSTICS: StorageDiagnostics = StorageDiagnostics {
    record_open_attempt: io_probe::record_db_open,
    observe_connection: io_probe::observe_sql_connection,
    record_open_duration: diagnostics::record_runtime_db_connection_open,
    record_sidecar_scan: diagnostics::record_runtime_db_sidecar_consistency_scan,
    record_stage,
    reader_pool: diagnostics::record_reader_pool_event,
};

fn record_stage(stage: StorageStage, elapsed: Duration) {
    let metric = match stage {
        StorageStage::Connection => &attribution::CONNECTION,
        StorageStage::SqliteOpen => &attribution::SQLITE_OPEN,
        StorageStage::Configure => &attribution::CONFIGURE,
        StorageStage::Sidecar => &attribution::SIDECAR,
        StorageStage::ReaderCheckout => &attribution::READER_CHECKOUT,
        StorageStage::ReaderLease => &attribution::READER_LEASE,
        StorageStage::ReaderOperation => &attribution::READER_OPERATION,
        StorageStage::ReaderAdmission => &attribution::READER_ADMISSION,
        StorageStage::ReaderBlockingAdmission => &attribution::READER_BLOCKING_ADMISSION,
    };
    metric.record(elapsed, 0);
}
