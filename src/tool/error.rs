use crate::runtime_error::{
    collect_runtime_error_source_chain, describe_runtime_error, RuntimeErrorDomain,
};
use anyhow::Error as AnyhowError;
pub use holon_domain::ToolError;
use serde_json::json;

/// Expected lifecycle refusals. Project only static diagnostics, including
/// in the source chain collected by `tool_error_from_anyhow`.
pub(crate) enum LifecycleRejection {
    RuntimeExecutionOrWake,
    UnregisteredHomeArtifact,
    CheckoutIdentityMismatch,
    MergedIntoRequired,
    ReturnToRequired,
    WorktreeOccupied,
    WorktreeLocked,
    WorktreeGenerationMismatch,
    AgentDeleted,
}

impl LifecycleRejection {
    pub(crate) fn into_error(self) -> AnyhowError {
        use RuntimeErrorDomain::{Conflict, NotFound, Validation};

        let (domain, kind, reason, message) = match self {
            Self::RuntimeExecutionOrWake => (
                Conflict,
                "agent_cleanup_blocked",
                "runtime_execution_or_wake",
                "agent_cleanup_blocked: runtime_execution_or_wake",
            ),
            Self::UnregisteredHomeArtifact => (
                Conflict,
                "agent_cleanup_blocked",
                "unregistered_home_artifact",
                "agent cleanup blocked: agent home contains an unregistered artifact",
            ),
            Self::CheckoutIdentityMismatch => (
                Conflict,
                "worktree_removal_blocked",
                "checkout_identity_mismatch",
                "registered worktree checkout identity does not match live git state",
            ),
            Self::MergedIntoRequired => (
                Validation,
                "worktree_removal_precondition_required",
                "merged_into_required",
                "`merged_into` is required by branch policy",
            ),
            Self::ReturnToRequired => (
                Validation,
                "worktree_removal_precondition_required",
                "return_to_required",
                "active worktree requires `return_to`",
            ),
            Self::WorktreeOccupied => (
                Conflict,
                "worktree_removal_blocked",
                "worktree_occupied",
                "worktree has active occupancy outside the current projection",
            ),
            Self::WorktreeLocked => (
                Conflict,
                "worktree_removal_blocked",
                "worktree_locked",
                "worktree is locked",
            ),
            Self::WorktreeGenerationMismatch => (
                Conflict,
                "worktree_removal_blocked",
                "worktree_generation_mismatch",
                "registered worktree generation does not match live git state",
            ),
            Self::AgentDeleted => (
                NotFound,
                "agent_deleted",
                "agent_deleted",
                "agent was deleted",
            ),
        };
        ToolError::new(kind, message)
            .with_domain(domain)
            .with_details(json!({"reason": reason}))
            .into()
    }
}

pub fn tool_error_from_anyhow(error: &AnyhowError) -> ToolError {
    if let Some(mut tool_error) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<ToolError>())
        .cloned()
    {
        if tool_error.source_chain.is_empty() {
            tool_error.source_chain = collect_runtime_error_source_chain(error);
        }
        return tool_error;
    }

    let descriptor = describe_runtime_error(error);
    let details = (!descriptor.safe_context.is_empty()).then(|| json!(descriptor.safe_context));
    let kind =
        if descriptor.domain == RuntimeErrorDomain::Unknown && descriptor.code == "runtime_error" {
            "tool_execution_failed".to_string()
        } else {
            descriptor.code
        };
    ToolError {
        kind,
        message: descriptor.operator_message,
        domain: Some(descriptor.domain),
        details,
        recovery_hint: descriptor.recovery_hint,
        retryable: descriptor.retryable,
        source_chain: descriptor.source_chain,
    }
}

#[cfg(holon_lib_test_shard = "c")]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_error_from_anyhow_finds_wrapped_tool_error() {
        let error = anyhow::Error::from(
            ToolError::new(
                "execution_root_violation",
                "requested working directory is outside the current execution root",
            )
            .with_recovery_hint("omit `workdir`"),
        )
        .context("failed to resolve command task");
        let tool_error = crate::tool::tool_error_from_anyhow(&error);

        assert_eq!(tool_error.kind, "execution_root_violation");
        assert_eq!(tool_error.recovery_hint.as_deref(), Some("omit `workdir`"));
        assert!(tool_error
            .source_chain
            .iter()
            .any(|message| message == "failed to resolve command task"));
    }

    #[test]
    fn tool_error_from_anyhow_projects_typed_runtime_error() {
        let error = anyhow::Error::from(
            crate::runtime_error::RuntimeError::not_found(
                "task_not_found",
                "task task_123 not found",
            )
            .with_safe_context("task_id", "task_123"),
        )
        .context("failed to read task");

        let tool_error = crate::tool::tool_error_from_anyhow(&error);

        assert_eq!(tool_error.kind, "task_not_found");
        assert_eq!(tool_error.domain, Some(RuntimeErrorDomain::NotFound));
        assert_eq!(tool_error.details.as_ref().unwrap()["task_id"], "task_123");
        assert!(tool_error
            .source_chain
            .iter()
            .any(|message| message == "failed to read task"));
    }

    #[test]
    fn tool_error_from_anyhow_preserves_unknown_fallback_code() {
        let tool_error =
            crate::tool::tool_error_from_anyhow(&anyhow::anyhow!("unclassified failure"));

        assert_eq!(tool_error.kind, "tool_execution_failed");
        assert_eq!(tool_error.domain, Some(RuntimeErrorDomain::Unknown));
        assert!(!tool_error.retryable);
    }
}
