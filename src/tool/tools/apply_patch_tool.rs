use anyhow::{anyhow, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    system::ExecutionScopeKind,
    tool::{
        apply_patch,
        helpers::{invalid_tool_input, validate_non_empty},
        spec::{ToolFreeformGrammar, ToolResultStatus},
        ToolResult,
    },
    types::{ApplyPatchAction, ApplyPatchResult, ToolCapabilityFamily, TrustLevel},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::{helpers::parse_tool_args, schema::tool_input_schema};

pub(crate) const NAME: &str = "ApplyPatch";
const APPLY_PATCH_LARK_GRAMMAR: &str = include_str!("apply_patch_tool.lark");

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApplyPatchArgs {
    pub(crate) patch: String,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::LocalEnvironment,
        spec: crate::tool::ToolSpec {
            name: NAME.to_string(),
            description: "Apply a unified diff patch across one or more files. On providers that expose custom/freeform tools, send the unified diff body directly rather than wrapping it in JSON. On JSON fallback providers, ApplyPatch expects exactly {\"patch\":\"--- a/path\\n+++ b/path\\n@@ -1,1 +1,1 @@\\n-old\\n+new\\n\"}. Do not use \"input\" or the old *** Begin Patch format.".to_string(),
            input_schema: tool_input_schema::<ApplyPatchArgs>()?,
            freeform_grammar: Some(ToolFreeformGrammar {
                syntax: "lark".to_string(),
                definition: APPLY_PATCH_LARK_GRAMMAR.to_string(),
            }),
        },
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _trust: &TrustLevel,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let patch_input = extract_patch_input(input)?;
    let execution = runtime
        .effective_execution(ExecutionScopeKind::AgentTurn)
        .await?;
    let outcome =
        apply_patch::apply_patch(execution.workspace.execution_root(), &patch_input).await?;
    let summary_text = if outcome.changed_files.is_empty() {
        "patched no files".to_string()
    } else {
        format!(
            "patched {}",
            outcome
                .changed_files
                .iter()
                .map(render_changed_file_summary)
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    serialize_success(
        NAME,
        &ApplyPatchResult {
            changed_files: outcome.changed_files,
            changed_file_count: outcome.changed_paths.len(),
            changed_paths: outcome.changed_paths,
            ignored_metadata: outcome.ignored_metadata,
            diagnostics: outcome.diagnostics,
            summary_text: Some(summary_text),
        },
    )
}

pub(crate) fn render_for_model(result: &ToolResult) -> Result<String> {
    if matches!(result.envelope.status, ToolResultStatus::Error) {
        let error = result
            .tool_error()
            .ok_or_else(|| anyhow!("ApplyPatch error result missing tool error"))?;
        return Ok(format!("ApplyPatch failed\n{}\n", error.render()));
    }

    let value = result
        .envelope
        .result
        .clone()
        .ok_or_else(|| anyhow!("ApplyPatch result missing payload"))?;
    let result: ApplyPatchResult = serde_json::from_value(value)?;

    let mut lines = vec!["Success. Updated the following files:".to_string()];
    if result.changed_files.is_empty() {
        lines.push("(no file changes recorded)".to_string());
    } else {
        lines.extend(result.changed_files.iter().map(render_changed_file_receipt));
    }
    Ok(lines.join("\n"))
}

fn extract_patch_input(input: &Value) -> Result<String> {
    match input {
        Value::String(patch) => validate_non_empty(patch.clone(), NAME, "patch"),
        Value::Object(map) if map.contains_key("input") && !map.contains_key("patch") => Err(
            invalid_tool_input(
                NAME,
                "ApplyPatch expects `patch`, not `input`",
                serde_json::json!({
                    "field": "input",
                    "expected_field": "patch",
                }),
                "ApplyPatch expects exactly {\"patch\": \"--- a/path\\n+++ b/path\\n@@ -1,1 +1,1 @@\\n-old\\n+new\\n\"}. Do not use \"input\".",
            ),
        ),
        _ => {
            let args: ApplyPatchArgs = parse_tool_args(NAME, input)?;
            validate_non_empty(args.patch, NAME, "patch")
        }
    }
}

fn render_changed_file_receipt(file: &crate::types::ApplyPatchChangedFile) -> String {
    match file.action {
        ApplyPatchAction::Move => format!(
            "{} {} -> {}",
            file.action.marker(),
            file.from_path.as_deref().unwrap_or("?"),
            file.path
        ),
        _ => format!("{} {}", file.action.marker(), file.path),
    }
}

fn render_changed_file_summary(file: &crate::types::ApplyPatchChangedFile) -> String {
    match file.action {
        ApplyPatchAction::Move => format!(
            "{}:{}->{}",
            file.action.marker(),
            file.from_path.as_deref().unwrap_or("?"),
            file.path
        ),
        _ => format!("{}:{}", file.action.marker(), file.path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolError;
    use serde_json::json;

    #[test]
    fn apply_patch_renders_text_receipt() {
        let result = serialize_success(
            NAME,
            &ApplyPatchResult {
                changed_files: vec![
                    crate::types::ApplyPatchChangedFile {
                        action: ApplyPatchAction::Modify,
                        path: "src/lib.rs".into(),
                        from_path: None,
                    },
                    crate::types::ApplyPatchChangedFile {
                        action: ApplyPatchAction::Add,
                        path: "README.md".into(),
                        from_path: None,
                    },
                ],
                changed_file_count: 2,
                changed_paths: vec!["src/lib.rs".into(), "README.md".into()],
                ignored_metadata: Vec::new(),
                diagnostics: Vec::new(),
                summary_text: Some("patched src/lib.rs, README.md".into()),
            },
        )
        .unwrap();

        let rendered = render_for_model(&result).unwrap();
        assert!(rendered.contains("Success. Updated the following files:"));
        assert!(rendered.contains("M src/lib.rs"));
        assert!(rendered.contains("A README.md"));
    }

    #[test]
    fn apply_patch_error_renders_failure_receipt() {
        let result = ToolResult::error(NAME, ToolError::new("patch_failed", "patch exploded"));
        let rendered = render_for_model(&result).unwrap();
        assert!(rendered.contains("ApplyPatch failed"));
        assert!(rendered.contains("patch exploded"));
    }

    #[test]
    fn apply_patch_json_input_rejects_legacy_input_field() {
        let error = extract_patch_input(&serde_json::json!({
            "input": "--- a/old.txt\n+++ b/old.txt\n@@ -1,1 +1,1 @@\n-old\n+new\n"
        }))
        .unwrap_err();
        let tool_error = crate::tool::ToolError::from_anyhow(&error);
        assert_eq!(tool_error.kind, "invalid_tool_input");
        assert!(tool_error
            .recovery_hint
            .as_deref()
            .unwrap_or_default()
            .contains("Do not use \"input\""));
    }

    #[test]
    fn apply_patch_function_fallback_uses_patch_field() {
        let spec = definition().unwrap().spec;
        assert!(spec.input_schema["properties"]["patch"].is_object());
        assert!(spec.input_schema["properties"]["input"].is_null());
        assert!(spec.freeform_grammar.is_some());
    }

    #[test]
    fn apply_patch_result_deserializes_without_changed_files() {
        let parsed: ApplyPatchResult = serde_json::from_value(json!({
            "changed_paths": ["src/lib.rs"],
            "changed_file_count": 1
        }))
        .unwrap();
        assert!(parsed.changed_files.is_empty());
        assert_eq!(parsed.changed_paths, vec!["src/lib.rs"]);
    }

    #[test]
    fn apply_patch_freeform_grammar_requires_update_hunks() {
        let grammar = definition()
            .unwrap()
            .spec
            .freeform_grammar
            .expect("apply patch should expose freeform grammar")
            .definition;
        assert!(grammar.contains("old_file: \"--- \" file_path LF"));
        assert!(grammar.contains("new_file: \"+++ \" file_path LF"));
        assert!(grammar.contains("hunk_header: \"@@ -\" range \" +\" range \" @@\""));
        assert!(!grammar.contains("*** Begin Patch"));
    }
}
