//! Transaction failure certainty, independent of SQLite's crash durability policy.

use std::fmt;

/// What a failed transaction attempt proves about its database writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistenceCertainty {
    /// The callback did not run, or its transaction explicitly rolled back.
    DefinitelyNotCommitted,
    /// The caller must reconcile the committed state before replaying the command.
    Indeterminate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistenceFailurePhase {
    BeforeBegin,
    Begin,
    Callback,
    Rollback,
    Commit,
}

/// Typed context attached to the original `anyhow::Error` without replacing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistenceFailure {
    certainty: PersistenceCertainty,
    phase: PersistenceFailurePhase,
    message: String,
    rollback_error: Option<String>,
}

impl PersistenceFailure {
    pub fn certainty(&self) -> PersistenceCertainty {
        self.certainty
    }

    pub fn phase(&self) -> PersistenceFailurePhase {
        self.phase
    }

    pub fn rollback_error(&self) -> Option<&str> {
        self.rollback_error.as_deref()
    }
}

impl fmt::Display for PersistenceFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Keep existing error messages while exposing certainty through downcast.
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PersistenceFailure {}

/// Untagged errors carry no persistence guarantee.
///
/// This describes one transaction attempt, not external effects in its callback,
/// enqueue acceptance, or durability across an OS or power failure.
pub fn persistence_certainty(error: &anyhow::Error) -> Option<PersistenceCertainty> {
    error
        .downcast_ref::<PersistenceFailure>()
        .map(PersistenceFailure::certainty)
}

/// Preserve legacy untagged OCC retries, but never replay an ambiguous transaction.
pub(crate) fn retryable_state_transition_conflict(
    error: &anyhow::Error,
) -> Option<&crate::runtime_db::RuntimeStateTransitionConflict> {
    if persistence_certainty(error) == Some(PersistenceCertainty::Indeterminate) {
        return None;
    }
    error
        .downcast_ref::<crate::runtime_db::RuntimeStateTransitionConflict>()
        .filter(|conflict| conflict.retryable())
        .or_else(|| {
            error.chain().find_map(|source| {
                source
                    .downcast_ref::<crate::runtime_db::RuntimeStateTransitionConflict>()
                    .filter(|conflict| conflict.retryable())
            })
        })
}

pub(crate) fn mark_persistence_failure(
    error: anyhow::Error,
    certainty: PersistenceCertainty,
    phase: PersistenceFailurePhase,
    rollback_error: Option<String>,
) -> anyhow::Error {
    // A successful outer rollback cannot resolve an already ambiguous inner error.
    if persistence_certainty(&error) == Some(PersistenceCertainty::Indeterminate) {
        return error;
    }
    let failure = PersistenceFailure {
        certainty,
        phase,
        message: error.to_string(),
        rollback_error,
    };
    error.context(failure)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::path::Path;
    use std::time::Duration;

    use anyhow::{anyhow, Result};
    use rusqlite::{Connection, Transaction};

    use crate::runtime_db::connection::{
        is_retryable_db_error, is_retryable_transaction_error, run_transaction_on_connection,
    };
    use crate::runtime_db::write_queue::RuntimeDbWriteContext;
    use crate::runtime_db::{RuntimeDbRetryableError, RuntimeStateTransitionConflict};

    fn transaction<T>(
        connection: &Connection,
        f: impl FnOnce(&Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        run_transaction_on_connection(
            connection,
            Path::new("persistence-test.sqlite"),
            RuntimeDbWriteContext::sync("persistence_test", "test_rows"),
            Duration::ZERO,
            Duration::ZERO,
            f,
        )
    }

    #[test]
    fn conflict_retry_respects_certainty_through_outer_context() {
        for certainty in [
            Some(PersistenceCertainty::Indeterminate),
            Some(PersistenceCertainty::DefinitelyNotCommitted),
            None,
        ] {
            let error = anyhow::Error::new(RuntimeStateTransitionConflict::revision(
                "agent-1",
                "agent_state_revision_conflict",
                Some(1),
                Some(2),
                true,
            ));
            let error = match certainty {
                Some(certainty) => mark_persistence_failure(
                    error,
                    certainty,
                    PersistenceFailurePhase::Rollback,
                    None,
                ),
                None => error,
            }
            .context("outer command context");

            let conflict = retryable_state_transition_conflict(&error);
            assert_eq!(
                conflict.is_some(),
                certainty != Some(PersistenceCertainty::Indeterminate)
            );
            if let Some(conflict) = conflict {
                assert_eq!(conflict.code(), "agent_state_revision_conflict");
                assert_eq!(conflict.record_id(), "agent-1");
            }
            assert!(error
                .downcast_ref::<RuntimeStateTransitionConflict>()
                .is_some());
        }

        let non_retryable = anyhow::Error::new(RuntimeStateTransitionConflict::revision(
            "agent-1",
            "agent_state_revision_conflict",
            Some(1),
            Some(2),
            false,
        ));
        assert!(retryable_state_transition_conflict(&non_retryable).is_none());
        assert!(retryable_state_transition_conflict(&anyhow!("not a conflict")).is_none());
    }

    #[test]
    fn persistence_context_preserves_original_downcast_and_message() {
        let conflict = RuntimeStateTransitionConflict::revision(
            "agent-1",
            "agent_state_revision_conflict",
            Some(1),
            Some(2),
            true,
        );
        let error = anyhow::Error::new(conflict).context("processing command");
        let message = error.to_string();
        let error = mark_persistence_failure(
            error,
            PersistenceCertainty::DefinitelyNotCommitted,
            PersistenceFailurePhase::Callback,
            None,
        )
        .context("caller context");

        assert!(error
            .downcast_ref::<RuntimeStateTransitionConflict>()
            .is_some());
        assert!(error.downcast_ref::<PersistenceFailure>().is_some());
        assert_eq!(
            persistence_certainty(&error),
            Some(PersistenceCertainty::DefinitelyNotCommitted)
        );
        assert!(error.chain().any(|source| source
            .downcast_ref::<RuntimeStateTransitionConflict>()
            .is_some()));
        assert_eq!(error.chain().nth(1).unwrap().to_string(), message);
        assert!(is_retryable_db_error(&error));
        assert!(!is_retryable_transaction_error(&error));
        assert_eq!(persistence_certainty(&anyhow!("untagged")), None);
    }

    #[test]
    fn persistence_begin_failure_never_runs_callback() -> Result<()> {
        let connection = Connection::open_in_memory()?;
        connection.execute_batch("BEGIN IMMEDIATE;")?;
        let called = Cell::new(false);
        let error = transaction(&connection, |_| {
            called.set(true);
            Ok(())
        })
        .expect_err("nested BEGIN must fail before the callback");

        assert!(!called.get());
        assert_eq!(
            persistence_certainty(&error),
            Some(PersistenceCertainty::DefinitelyNotCommitted)
        );
        assert_eq!(
            error.downcast_ref::<PersistenceFailure>().unwrap().phase(),
            PersistenceFailurePhase::Begin
        );
        assert!(error.downcast_ref::<rusqlite::Error>().is_some());
        connection.execute_batch("ROLLBACK;")?;
        Ok(())
    }

    #[test]
    fn persistence_callback_failure_requires_explicit_rollback() -> Result<()> {
        let connection = Connection::open_in_memory()?;
        connection.execute_batch("CREATE TABLE test_rows (value INTEGER);")?;
        let error = transaction(&connection, |tx| {
            tx.execute("INSERT INTO test_rows VALUES (1)", [])?;
            Err::<(), _>(anyhow!("force rollback"))
        })
        .expect_err("callback failure must return an error");

        assert_eq!(error.to_string(), "force rollback");
        assert_eq!(
            persistence_certainty(&error),
            Some(PersistenceCertainty::DefinitelyNotCommitted)
        );
        let failure = error.downcast_ref::<PersistenceFailure>().unwrap();
        assert_eq!(failure.phase(), PersistenceFailurePhase::Callback);
        assert_eq!(failure.rollback_error(), None);
        assert!(connection.is_autocommit());
        let rows: u64 =
            connection.query_row("SELECT count(*) FROM test_rows", [], |row| row.get(0))?;
        assert_eq!(rows, 0);
        Ok(())
    }

    #[test]
    fn persistence_commit_failure_remains_indeterminate_after_drop_cleanup() -> Result<()> {
        let connection = Connection::open_in_memory()?;
        connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE parent (id INTEGER PRIMARY KEY);
             CREATE TABLE child (
               parent_id INTEGER REFERENCES parent(id) DEFERRABLE INITIALLY DEFERRED
             );",
        )?;
        let error = transaction(&connection, |tx| {
            tx.execute("INSERT INTO child VALUES (1)", [])?;
            Ok(())
        })
        .expect_err("deferred constraint must fail at COMMIT");

        assert_eq!(
            persistence_certainty(&error),
            Some(PersistenceCertainty::Indeterminate)
        );
        assert_eq!(
            error.downcast_ref::<PersistenceFailure>().unwrap().phase(),
            PersistenceFailurePhase::Commit
        );
        assert!(error.downcast_ref::<rusqlite::Error>().is_some());
        assert!(connection.is_autocommit());
        let rows: u64 = connection.query_row("SELECT count(*) FROM child", [], |row| row.get(0))?;
        assert_eq!(rows, 0);
        Ok(())
    }

    #[test]
    fn persistence_failed_rollback_preserves_callback_error_and_diagnostic() -> Result<()> {
        let connection = Connection::open_in_memory()?;
        connection.execute_batch("CREATE TABLE test_rows (value INTEGER);")?;
        let error = transaction(&connection, |tx| {
            tx.execute("INSERT INTO test_rows VALUES (1)", [])?;
            // Deliberate fault injection: application callbacks must not control the transaction.
            tx.execute_batch("COMMIT;")?;
            Err::<(), _>(
                RuntimeDbRetryableError::new(
                    "test callback",
                    Path::new("persistence-test.sqlite"),
                    "database is locked",
                )
                .into(),
            )
        })
        .expect_err("the explicit rollback must fail after the injected commit");

        assert_eq!(
            persistence_certainty(&error),
            Some(PersistenceCertainty::Indeterminate)
        );
        let failure = error.downcast_ref::<PersistenceFailure>().unwrap();
        assert_eq!(failure.phase(), PersistenceFailurePhase::Rollback);
        assert!(failure.rollback_error().is_some());
        assert!(error.downcast_ref::<RuntimeDbRetryableError>().is_some());
        assert!(!is_retryable_transaction_error(&error));
        assert!(!is_retryable_db_error(&error));
        let rows: u64 =
            connection.query_row("SELECT count(*) FROM test_rows", [], |row| row.get(0))?;
        assert_eq!(rows, 1);
        Ok(())
    }

    #[test]
    fn persistence_indeterminate_blocks_lock_and_occ_retries_and_cannot_be_downgraded() {
        let lock = RuntimeDbRetryableError::new(
            "committing transaction",
            Path::new("persistence-test.sqlite"),
            "database is busy",
        );
        let conflict = RuntimeStateTransitionConflict::revision(
            "agent-1",
            "agent_state_revision_conflict",
            Some(1),
            Some(2),
            true,
        );
        for error in [anyhow::Error::new(lock), anyhow::Error::new(conflict)] {
            let error = mark_persistence_failure(
                error,
                PersistenceCertainty::Indeterminate,
                PersistenceFailurePhase::Commit,
                None,
            );
            let error = mark_persistence_failure(
                error.context("caller context"),
                PersistenceCertainty::DefinitelyNotCommitted,
                PersistenceFailurePhase::Callback,
                None,
            );
            assert_eq!(
                persistence_certainty(&error),
                Some(PersistenceCertainty::Indeterminate)
            );
            assert_eq!(
                error.downcast_ref::<PersistenceFailure>().unwrap().phase(),
                PersistenceFailurePhase::Commit
            );
            assert!(!is_retryable_transaction_error(&error));
            assert!(!is_retryable_db_error(&error));
        }
    }
}
