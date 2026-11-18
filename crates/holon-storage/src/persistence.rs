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

impl PersistenceFailurePhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BeforeBegin => "before_begin",
            Self::Begin => "begin",
            Self::Callback => "callback",
            Self::Rollback => "rollback",
            Self::Commit => "commit",
        }
    }
}

/// Typed context attached to the original `anyhow::Error` without replacing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistenceFailure {
    certainty: PersistenceCertainty,
    phase: PersistenceFailurePhase,
    message: String,
    rollback_error: Option<String>,
    operation: Option<&'static str>,
    table: Option<&'static str>,
    sqlite_extended_code: Option<i32>,
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

    pub fn operation(&self) -> Option<&'static str> {
        self.operation
    }

    pub fn table(&self) -> Option<&'static str> {
        self.table
    }

    pub fn sqlite_extended_code(&self) -> Option<i32> {
        self.sqlite_extended_code
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

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
struct OriginalInputRecoveryFailure {
    message: String,
}

pub fn mark_original_input_recovery_failure(error: anyhow::Error) -> anyhow::Error {
    let message = error.to_string();
    error.context(OriginalInputRecoveryFailure { message })
}

/// Recovery transaction certainty cannot release the original business capsule.
/// Its receipt/owner protocol must resolve that original attempt first.
pub fn original_input_attempt_certainty(error: &anyhow::Error) -> Option<PersistenceCertainty> {
    if error.is::<OriginalInputRecoveryFailure>() {
        None
    } else {
        persistence_certainty(error)
    }
}

/// Preserve legacy untagged OCC retries, but never replay an ambiguous transaction.
pub fn retryable_state_transition_conflict(
    error: &anyhow::Error,
) -> Option<&crate::RuntimeStateTransitionConflict> {
    if persistence_certainty(error) == Some(PersistenceCertainty::Indeterminate) {
        return None;
    }
    error
        .downcast_ref::<crate::RuntimeStateTransitionConflict>()
        .filter(|conflict| conflict.retryable())
        .or_else(|| {
            error.chain().find_map(|source| {
                source
                    .downcast_ref::<crate::RuntimeStateTransitionConflict>()
                    .filter(|conflict| conflict.retryable())
            })
        })
}

pub fn mark_persistence_failure(
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
        operation: None,
        table: None,
        sqlite_extended_code: sqlite_extended_code(&error),
    };
    error.context(failure)
}

pub fn sqlite_extended_code(error: &anyhow::Error) -> Option<i32> {
    error
        .chain()
        .find_map(|source| match source.downcast_ref::<rusqlite::Error>() {
            Some(rusqlite::Error::SqliteFailure(failure, _)) => Some(failure.extended_code),
            _ => None,
        })
}

pub(crate) fn attach_write_context(
    error: anyhow::Error,
    operation: &'static str,
    table: &'static str,
) -> anyhow::Error {
    let Some(mut failure) = error.downcast_ref::<PersistenceFailure>().cloned() else {
        return error;
    };
    failure.operation.get_or_insert(operation);
    failure.table.get_or_insert(table);
    error.context(failure)
}

#[cfg(test)]
mod tests;
