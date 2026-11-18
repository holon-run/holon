use crate::runtime_error::{
    collect_runtime_error_source_chain, describe_runtime_error, RuntimeErrorDomain,
};
use anyhow::Error as AnyhowError;
pub use holon_domain::ToolError;
use serde_json::json;

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
