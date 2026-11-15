#[cfg(test)]
pub(crate) use holon_storage::persistence::mark_persistence_failure;
pub(crate) use holon_storage::persistence::{
    mark_original_input_recovery_failure, original_input_attempt_certainty,
    retryable_state_transition_conflict, sqlite_extended_code,
};
pub use holon_storage::persistence::{
    persistence_certainty, PersistenceCertainty, PersistenceFailure, PersistenceFailurePhase,
};

#[cfg(all(test, holon_lib_test_shard = "b"))]
mod tests {
    use super::*;
    use anyhow::anyhow;

    #[test]
    fn recovery_certainty_cannot_release_the_original_attempt() {
        for certainty in [
            PersistenceCertainty::DefinitelyNotCommitted,
            PersistenceCertainty::Indeterminate,
        ] {
            let error = mark_persistence_failure(
                anyhow!(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY_SNAPSHOT),
                    None,
                )),
                certainty,
                PersistenceFailurePhase::Callback,
                None,
            );
            assert_eq!(original_input_attempt_certainty(&error), Some(certainty));
            let recovery = mark_original_input_recovery_failure(error).context("caller context");
            assert_eq!(original_input_attempt_certainty(&recovery), None);
            assert_eq!(persistence_certainty(&recovery), Some(certainty));
            let descriptor = crate::runtime_error::describe_runtime_error(&recovery);
            assert_eq!(
                descriptor.retryable,
                certainty == PersistenceCertainty::DefinitelyNotCommitted
            );
            assert_eq!(descriptor.safe_context["sqlite_extended_code"], "517");
            if certainty == PersistenceCertainty::Indeterminate {
                assert_eq!(descriptor.code, "runtime_db_outcome_unknown");
                let outer = mark_persistence_failure(
                    recovery,
                    PersistenceCertainty::DefinitelyNotCommitted,
                    PersistenceFailurePhase::Callback,
                    None,
                );
                assert_eq!(persistence_certainty(&outer), Some(certainty));
                assert_eq!(original_input_attempt_certainty(&outer), None);
            } else {
                assert_eq!(descriptor.code, "runtime_db_busy");
            }
        }
    }
}
