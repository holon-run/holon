use anyhow::{anyhow, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    system::ExecutionScopeKind,
    tool::{
        apply_patch,
        helpers::{invalid_tool_input, truncate_text, validate_non_empty},
        spec::{ToolFreeformGrammar, ToolResultStatus},
        ToolError, ToolResult,
    },
    types::{
        ApplyPatchAction, ApplyPatchDiagnostic, ApplyPatchResult, ToolCapabilityFamily, TrustLevel,
    },
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::{helpers::parse_tool_args, schema::tool_input_schema};

pub(crate) const NAME: &str = "ApplyPatch";
const APPLY_PATCH_LARK_GRAMMAR: &str = include_str!("apply_patch_tool.lark");
const MODEL_ERROR_TEXT_LIMIT: usize = 700;
const MODEL_ERROR_TOKEN_LIMIT: usize = 96;
const MODEL_DIAGNOSTIC_LIMIT: usize = 8;
const SUMMARY_DIAGNOSTIC_LIMIT: usize = 3;

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
            description: "Apply a unified diff patch across one or more files. Follow the invocation shape exposed by the current tool surface, and do not use the old *** Begin Patch format.".to_string(),
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
    let mut summary_text = if outcome.changed_files.is_empty() {
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
    if !outcome.diagnostics.is_empty() {
        summary_text.push_str(&render_diagnostics_summary(&outcome.diagnostics));
    }
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
        return Ok(render_apply_patch_error_for_model(error));
    }

    let value = result
        .envelope
        .result
        .clone()
        .ok_or_else(|| anyhow!("ApplyPatch result missing payload"))?;
    let result: ApplyPatchResult = serde_json::from_value(value)?;

    let mut lines = if result.diagnostics.is_empty() {
        vec!["Success. Updated the following files:".to_string()]
    } else {
        vec!["Success with diagnostics. Updated the following files:".to_string()]
    };
    if result.changed_files.is_empty() {
        lines.push("(no file changes recorded)".to_string());
    } else {
        lines.extend(result.changed_files.iter().map(render_changed_file_receipt));
    }
    if !result.diagnostics.is_empty() {
        lines.push(String::new());
        lines.push("Diagnostics:".to_string());
        lines.extend(
            result
                .diagnostics
                .iter()
                .take(MODEL_DIAGNOSTIC_LIMIT)
                .map(render_diagnostic_for_model),
        );
        if result.diagnostics.len() > MODEL_DIAGNOSTIC_LIMIT {
            lines.push(format!(
                "- omitted {} additional diagnostics",
                result.diagnostics.len() - MODEL_DIAGNOSTIC_LIMIT
            ));
        }
        lines.push(
            "Inspect the affected target region before applying another patch to the same file."
                .to_string(),
        );
    }
    Ok(lines.join("\n"))
}

fn render_apply_patch_error_for_model(error: &ToolError) -> String {
    let mut lines = vec![
        "ApplyPatch failed".to_string(),
        format!("- kind: {}", error.kind),
        format!(
            "- message: {}",
            sanitize_model_visible_error_text(&error.message)
        ),
    ];
    if let Some(recovery_hint) = error.recovery_hint.as_deref() {
        lines.push(format!(
            "- recovery_hint: {}",
            sanitize_model_visible_error_text(recovery_hint)
        ));
    }
    if error.details.is_some() {
        lines.push(
            "- details: omitted from model-visible receipt; inspect audit/tool records if exact parser details are needed"
                .to_string(),
        );
    }
    if error.retryable {
        lines.push("- retryable: true".to_string());
    }
    lines.push(
        "Use a smaller, valid unified diff or inspect the target file before retrying.".to_string(),
    );
    format!("{}\n", lines.join("\n"))
}

fn sanitize_model_visible_error_text(text: &str) -> String {
    let mut sanitized = String::new();
    let mut token_len = 0usize;
    let mut omitting_token = false;
    for ch in text.trim().chars() {
        if ch.is_whitespace() {
            token_len = 0;
            omitting_token = false;
            sanitized.push(ch);
            continue;
        }

        token_len += 1;
        if token_len <= MODEL_ERROR_TOKEN_LIMIT {
            sanitized.push(ch);
        } else if !omitting_token {
            sanitized.push_str("[long token omitted]");
            omitting_token = true;
        }
    }
    truncate_text(&sanitized, MODEL_ERROR_TEXT_LIMIT)
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

fn render_diagnostic_for_model(diagnostic: &ApplyPatchDiagnostic) -> String {
    let path = if diagnostic.path.trim().is_empty() {
        "unknown path".to_string()
    } else {
        sanitize_model_visible_error_text(&diagnostic.path)
    };
    format!(
        "- {} on {}: {}",
        diagnostic.kind,
        path,
        sanitize_model_visible_error_text(&diagnostic.message)
    )
}

fn render_diagnostics_summary(diagnostics: &[ApplyPatchDiagnostic]) -> String {
    let mut parts = diagnostics
        .iter()
        .take(SUMMARY_DIAGNOSTIC_LIMIT)
        .map(render_diagnostic_summary)
        .collect::<Vec<_>>();
    if diagnostics.len() > SUMMARY_DIAGNOSTIC_LIMIT {
        parts.push(format!(
            "+{} more",
            diagnostics.len() - SUMMARY_DIAGNOSTIC_LIMIT
        ));
    }
    format!(" (diagnostics: {})", parts.join(", "))
}

fn render_diagnostic_summary(diagnostic: &ApplyPatchDiagnostic) -> String {
    let path = if diagnostic.path.trim().is_empty() {
        "unknown path".to_string()
    } else {
        diagnostic.path.clone()
    };
    sanitize_model_visible_error_text(&format!("{} on {}", diagnostic.kind, path))
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
    fn apply_patch_renders_diagnostics_in_model_receipt() {
        let result = serialize_success(
            NAME,
            &ApplyPatchResult {
                changed_files: vec![crate::types::ApplyPatchChangedFile {
                    action: ApplyPatchAction::Modify,
                    path: "src/lib.rs".into(),
                    from_path: None,
                }],
                changed_file_count: 1,
                changed_paths: vec!["src/lib.rs".into()],
                ignored_metadata: Vec::new(),
                diagnostics: vec![ApplyPatchDiagnostic {
                    path: "src/lib.rs".into(),
                    kind: "hunk_count_mismatch".into(),
                    message: "hunk header declared -1,1 +1,1 but body counted -1,20 +1,30".into(),
                }],
                summary_text: Some("patched M:src/lib.rs".into()),
            },
        )
        .unwrap();

        let rendered = render_for_model(&result).unwrap();
        assert!(rendered.contains("Success with diagnostics"));
        assert!(rendered.contains("M src/lib.rs"));
        assert!(rendered.contains("Diagnostics:"));
        assert!(rendered.contains("hunk_count_mismatch"));
        assert!(rendered.contains("Inspect the affected target region"));
    }

    #[test]
    fn apply_patch_renders_bounded_diagnostics_in_model_receipt() {
        let diagnostics = (0..(MODEL_DIAGNOSTIC_LIMIT + 2))
            .map(|idx| ApplyPatchDiagnostic {
                path: format!("src/lib{idx}.rs"),
                kind: "hunk_count_mismatch".into(),
                message: format!("diagnostic {idx}"),
            })
            .collect::<Vec<_>>();
        let result = serialize_success(
            NAME,
            &ApplyPatchResult {
                changed_files: vec![crate::types::ApplyPatchChangedFile {
                    action: ApplyPatchAction::Modify,
                    path: "src/lib.rs".into(),
                    from_path: None,
                }],
                changed_file_count: 1,
                changed_paths: vec!["src/lib.rs".into()],
                ignored_metadata: Vec::new(),
                diagnostics,
                summary_text: Some("patched M:src/lib.rs".into()),
            },
        )
        .unwrap();

        let rendered = render_for_model(&result).unwrap();
        assert!(rendered.contains("diagnostic 7"));
        assert!(!rendered.contains("diagnostic 8"));
        assert!(rendered.contains("- omitted 2 additional diagnostics"));
    }

    #[test]
    fn apply_patch_diagnostics_summary_is_bounded() {
        let diagnostics = (0..(SUMMARY_DIAGNOSTIC_LIMIT + 2))
            .map(|idx| ApplyPatchDiagnostic {
                path: format!("src/lib{idx}.rs"),
                kind: "hunk_count_mismatch".into(),
                message: format!("diagnostic {idx}"),
            })
            .collect::<Vec<_>>();

        let summary = render_diagnostics_summary(&diagnostics);
        assert!(summary.contains("diagnostics: hunk_count_mismatch on src/lib0.rs"));
        assert!(summary.contains("+2 more"));
        assert!(!summary.contains("src/lib3.rs"));
    }

    #[test]
    fn apply_patch_error_renders_failure_receipt() {
        let result = ToolResult::error(NAME, ToolError::new("patch_failed", "patch exploded"));
        let rendered = render_for_model(&result).unwrap();
        assert!(rendered.contains("ApplyPatch failed"));
        assert!(rendered.contains("patch exploded"));
    }

    #[test]
    fn apply_patch_error_omits_large_details_from_model_receipt() {
        let long_path = format!("src/{}.rs", "nested".repeat(600));
        let invalid_fragment = format!("{} code to=functions.ApplyPatch", "***".repeat(2000));
        let result = ToolResult::error(
            NAME,
            ToolError::new(
                "invalid_patch_syntax",
                format!("invalid patch near {}", "x".repeat(400)),
            )
            .with_details(json!({
                "path": long_path,
                "fragment": invalid_fragment,
            }))
            .with_recovery_hint(format!("inspect {}", "target".repeat(200))),
        );

        let rendered = render_for_model(&result).unwrap();
        assert!(rendered.contains("ApplyPatch failed"));
        assert!(rendered.contains("details: omitted"));
        assert!(rendered.contains("[long token omitted]"));
        assert!(!rendered.contains("code to=functions.ApplyPatch"));
        assert!(!rendered.contains(&"nested".repeat(60)));
        assert!(rendered.len() < 1_200);
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
