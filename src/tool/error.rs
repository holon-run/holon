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
    use serde_json::Value;
    const MODEL_VISIBLE_DETAILS_MAX_CHARS: usize = 1_000;

    #[test]
    fn tool_error_renders_as_pretty_json() {
        let rendered = ToolError::new("invalid_tool_input", "missing required string field")
            .with_details(json!({ "field": "cmd" }))
            .with_recovery_hint("provide a string value for `cmd`")
            .render();

        assert!(rendered.contains("\"kind\": \"invalid_tool_input\""));
        assert!(rendered.contains("\"field\": \"cmd\""));
        assert!(rendered.contains("provide a string value"));
    }

    #[test]
    fn tool_error_model_rendering_uses_shared_corrective_receipt() {
        let rendered = ToolError::new("invalid_tool_input", "missing required string field")
            .with_details(json!({ "field": "cmd", "parse_error": "missing field `cmd`" }))
            .with_recovery_hint("provide a string value for `cmd`")
            .render_for_model(Some("ExecCommand"));
        let receipt: Value = serde_json::from_str(&rendered).unwrap();

        assert_eq!(receipt["ok"], false);
        assert_eq!(receipt["tool_name"], "ExecCommand");
        assert_eq!(receipt["kind"], "invalid_tool_input");
        assert_eq!(receipt["message"], "missing required string field");
        assert_eq!(receipt["hint"], "provide a string value for `cmd`");
        assert_eq!(receipt["field"], "cmd");
        assert_eq!(receipt["retryable"], false);
        assert_eq!(receipt["details"]["parse_error"], "missing field `cmd`");
    }

    #[test]
    fn tool_error_model_rendering_supports_runtime_failures() {
        let rendered = ToolError::new("command_spawn_failed", "failed to start command process")
            .with_recovery_hint("check the command path and arguments")
            .with_retryable(true)
            .render_for_model(Some("ExecCommand"));
        let receipt: Value = serde_json::from_str(&rendered).unwrap();

        assert_eq!(receipt["ok"], false);
        assert_eq!(receipt["kind"], "command_spawn_failed");
        assert_eq!(receipt["hint"], "check the command path and arguments");
        assert_eq!(receipt["retryable"], true);
    }

    #[test]
    fn tool_error_model_rendering_bounds_long_details_without_changing_canonical_error() {
        let raw_payload = "x".repeat(MODEL_VISIBLE_DETAILS_MAX_CHARS + 500);
        let error = ToolError::new("invalid_tool_input", "malformed tool input")
            .with_details(json!({ "raw_input": raw_payload.clone() }));
        let canonical = error.render();
        let rendered = error.render_for_model(Some("ApplyPatch"));
        let receipt: Value = serde_json::from_str(&rendered).unwrap();

        assert!(canonical.contains(&raw_payload));
        assert_eq!(receipt["details"]["omitted"], true);
        assert_eq!(
            receipt["details"]["reason"],
            "details exceeded model-visible budget"
        );
        assert!(receipt["details"]["sha256"].as_str().unwrap().len() >= 64);
        assert!(
            receipt["details"]["preview"]
                .as_str()
                .unwrap()
                .chars()
                .count()
                <= MODEL_VISIBLE_DETAILS_MAX_CHARS
        );
        assert!(!rendered.contains(&raw_payload));
    }

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

    #[test]
    fn tool_error_deserializes_legacy_shape_with_taxonomy_defaults() {
        let tool_error: ToolError = serde_json::from_value(serde_json::json!({
            "kind": "tool_execution_failed",
            "message": "legacy failure",
            "retryable": false
        }))
        .unwrap();

        assert_eq!(tool_error.domain, None);
        assert!(tool_error.source_chain.is_empty());
    }
}
