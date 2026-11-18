use anyhow::{anyhow, Result};
use holon_storage::{
    connection::is_retryable_db_error, RuntimeDbRetryableError, RuntimeStateTransitionConflict,
};
use rusqlite::ffi::ErrorCode;

#[test]
fn runtime_db_retryable_error_classification_survives_context() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let db_path = temp.path().join("runtime.sqlite");
    let error: anyhow::Error = RuntimeDbRetryableError::new(
        "starting immediate transaction",
        &db_path,
        "database is locked",
    )
    .into();
    let error = error.context("processing message");
    assert!(is_retryable_db_error(&error));
    assert!(!is_retryable_db_error(&anyhow!("not a db lock")));
    Ok(())
}

#[test]
fn runtime_db_retryable_state_transition_conflict_classification_survives_context() {
    let conflict = RuntimeStateTransitionConflict::revision(
        "work-item-1",
        "agent_state_revision_conflict",
        Some(1),
        Some(2),
        true,
    );
    let error = anyhow::Error::new(conflict).context("processing message");
    assert!(is_retryable_db_error(&error));
    let non_retryable = anyhow::Error::new(RuntimeStateTransitionConflict::new(
        "agent_state",
        "agent-1",
        "active",
        "deleted",
    ));
    assert!(!is_retryable_db_error(&non_retryable));
}

#[test]
fn runtime_db_raw_sqlite_lock_errors_are_retryable() {
    let locked: anyhow::Error = rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error {
            code: ErrorCode::DatabaseLocked,
            extended_code: 0,
        },
        Some("database is locked".to_string()),
    )
    .into();
    let busy: anyhow::Error = rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error {
            code: ErrorCode::DatabaseBusy,
            extended_code: 0,
        },
        Some("database is busy".to_string()),
    )
    .into();
    let constraint: anyhow::Error = rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error {
            code: ErrorCode::ConstraintViolation,
            extended_code: 0,
        },
        None,
    )
    .into();

    assert!(is_retryable_db_error(
        &locked.context("inserting audit event")
    ));
    assert!(is_retryable_db_error(
        &busy.context("updating transcript entry")
    ));
    assert!(!is_retryable_db_error(&constraint));
}
