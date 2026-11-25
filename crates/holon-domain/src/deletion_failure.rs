//! Shared classification for failures raised while erasing an agent's private
//! state.
//!
//! The layer that detects a data-condition invariant marks the error with
//! [`DeletionOwnershipUnresolved`]. The deletion job state machine and the
//! runtime DB write callback both read that marker instead of matching error
//! text, so a failure that needs an operator decision can never be retried
//! forever by one layer while the other reports it as non-retryable.

use anyhow::Error;

/// A deletion failure that retrying cannot resolve: the persisted data needs an
/// explicit ownership or cleanup decision.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct DeletionOwnershipUnresolved(String);

impl DeletionOwnershipUnresolved {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    /// Mark a source error, keeping its deduplicated chain as the message.
    pub fn from_error(error: &Error) -> Self {
        Self(deletion_failure_message(error))
    }
}

/// How a deletion failure must be settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeletionFailureClass {
    /// Transient storage/IO failure; another attempt with backoff may succeed.
    Retryable,
    /// Neutral data condition; only an explicit decision can unblock it.
    NeedsOwnershipResolution,
}

/// Render an error chain once: every distinct link appears at most once, so a
/// context that several nested assertion layers repeat is not concatenated into
/// the persisted `last_error`.
pub fn deletion_failure_message(error: &Error) -> String {
    let mut parts: Vec<String> = Vec::new();
    for cause in error.chain() {
        let text = cause.to_string();
        let text = text.trim();
        if text.is_empty() || parts.last().is_some_and(|last| last == text) {
            continue;
        }
        parts.push(text.to_owned());
    }
    parts.join(": ")
}

/// Mark an error that retrying cannot resolve, keeping its deduplicated chain
/// as the message. The layer that detects a data-condition invariant calls this
/// so that the job state machine and the storage write callback classify the
/// same failure the same way.
pub fn mark_ownership_unresolved(error: Error) -> Error {
    if is_ownership_unresolved(&error) {
        return error;
    }
    Error::new(DeletionOwnershipUnresolved::from_error(&error))
}

/// Whether any link of the chain carries the ownership-resolution marker.
pub fn is_ownership_unresolved(error: &Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<DeletionOwnershipUnresolved>()
            .is_some()
    })
}

pub fn classify_deletion_failure(error: &Error) -> DeletionFailureClass {
    if is_ownership_unresolved(error) {
        DeletionFailureClass::NeedsOwnershipResolution
    } else {
        DeletionFailureClass::Retryable
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_context_layers_collapse_to_one_message() {
        let root =
            anyhow::anyhow!("message wait child lineage differs from the original scope group");
        let error = root
            .context("shared command history requires explicit ownership resolution")
            .context("shared command history requires explicit ownership resolution")
            .context("shared command history requires explicit ownership resolution");

        let message = deletion_failure_message(&error);

        assert_eq!(
            message
                .matches("shared command history requires explicit ownership resolution")
                .count(),
            1,
            "{message}"
        );
        assert!(
            message.ends_with("message wait child lineage differs from the original scope group"),
            "{message}"
        );
    }

    #[test]
    fn distinct_context_layers_are_preserved_once_each() {
        let error = anyhow::anyhow!("lock held")
            .context("clearing deletion-private table queue_entries")
            .context("deletion phase Index failed");

        assert_eq!(
            deletion_failure_message(&error),
            "deletion phase Index failed: clearing deletion-private table queue_entries: lock held"
        );
    }

    #[test]
    fn ownership_marker_drives_classification() {
        let transient = anyhow::anyhow!("database is locked");
        assert_eq!(
            classify_deletion_failure(&transient),
            DeletionFailureClass::Retryable
        );

        let unresolved = anyhow::Error::new(DeletionOwnershipUnresolved::new(
            "shared history requires explicit ownership resolution",
        ))
        .context("deletion phase Index failed");
        assert_eq!(
            classify_deletion_failure(&unresolved),
            DeletionFailureClass::NeedsOwnershipResolution
        );
    }
}
