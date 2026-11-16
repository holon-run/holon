use super::BuiltinToolDefinition;
use crate::tool::helpers::invalid_tool_input;
use crate::tool::helpers::parse_tool_args;
use crate::tool::helpers::truncate_text;
use crate::tool::helpers::validate_non_empty;
use crate::tool::schema::tool_input_schema;
use crate::tool::spec::ToolFreeformGrammar;
use crate::tool::spec::ToolOutcome;
use crate::tool::spec::ToolResultStatus;
use crate::tool::ToolError;
use crate::types::ApplyPatchAction;
use crate::types::ApplyPatchDiagnostic;
use crate::types::ApplyPatchResult;
use crate::types::ToolCapabilityFamily;
use anyhow::anyhow;
use anyhow::Result;
use holon_domain::ApplyPatchSurface;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

pub const NAME: &str = crate::tool::names::APPLY_PATCH;

// Grammar matches OpenAI Codex's ApplyPatch DSL surface; Holon still applies
// parsed edits through its own workspace guards and atomic apply path.
const CODEX_DSL_LARK_GRAMMAR: &str = include_str!("apply_patch_tool_codex.lark");

const MODEL_ERROR_TEXT_LIMIT: usize = 700;

const MODEL_ERROR_TOKEN_LIMIT: usize = 96;

pub const MODEL_DIAGNOSTIC_LIMIT: usize = 8;

pub const SUMMARY_DIAGNOSTIC_LIMIT: usize = 3;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApplyPatchArgs {
    pub patch: String,
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    definition_for_surface(ApplyPatchSurface::UnifiedDiffJson)
}

pub fn definition_for_surface(surface: ApplyPatchSurface) -> Result<BuiltinToolDefinition> {
    let (description, input_schema, freeform_grammar) = match surface {
        ApplyPatchSurface::CodexDslFreeform => (
            include_str!("../tool_descriptions/apply_patch_codex_dsl_freeform.md").to_string(),
            serde_json::json!({
                "type": "string",
                "description": "Raw Codex ApplyPatch DSL beginning with *** Begin Patch and ending with *** End Patch"
            }),
            Some(ToolFreeformGrammar {
                syntax: "lark".to_string(),
                definition: CODEX_DSL_LARK_GRAMMAR.to_string(),
            }),
        ),
        ApplyPatchSurface::UnifiedDiffJson => (
            include_str!("../tool_descriptions/apply_patch_unified_diff_json.md").to_string(),
            tool_input_schema::<ApplyPatchArgs>()?,
            None,
        ),
    };
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::LocalEnvironment,
        spec: crate::tool::ToolSpec {
            name: NAME.to_string(),
            description,
            input_schema,
            freeform_grammar,
        },
    })
}

pub fn replacement_character_diagnostic() -> ApplyPatchDiagnostic {
    ApplyPatchDiagnostic {
        path: String::new(),
        kind: "replacement_character_in_input".into(),
        message: "patch input contains U+FFFD (�), which may be original content or upstream decoding damage; reread the exact target region before constructing another patch".into(),
    }
}

pub fn add_replacement_character_recovery_hint(mut error: ToolError) -> ToolError {
    let warning = "The patch input contains U+FFFD (�), which may come from upstream streaming decode damage. Reread the exact target region and rebuild the patch from the fresh content; do not retry the old U+FFFD-containing context unchanged.";
    error.recovery_hint = Some(match error.recovery_hint.take() {
        Some(existing) => format!("{existing} {warning}"),
        None => warning.to_string(),
    });
    error
}

pub fn render_for_model(result: &ToolOutcome<impl Default>) -> Result<String> {
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
    if error.kind != "truncated_mutation_tool_call" {
        lines.push("Use the ApplyPatch format advertised for this turn, keep the patch smaller when possible, and inspect the target file before retrying.".to_string());
    }
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

pub fn extract_patch_input(input: &Value, surface: ApplyPatchSurface) -> Result<String> {
    match input {
        Value::String(patch) => validate_non_empty(
            patch.clone(),
            NAME,
            match surface {
                ApplyPatchSurface::CodexDslFreeform => "input",
                ApplyPatchSurface::UnifiedDiffJson => "patch",
            },
        ),
        Value::Object(map) if map.contains_key("input") && !map.contains_key("patch") => {
            if matches!(surface, ApplyPatchSurface::CodexDslFreeform) {
                return map
                    .get("input")
                    .and_then(Value::as_str)
                    .map(|value| validate_non_empty(value.to_string(), NAME, "input"))
                    .transpose()?
                    .ok_or_else(|| {
                        invalid_tool_input(
                            NAME,
                            "ApplyPatch input must be a string",
                            serde_json::json!({"field": "input"}),
                            "Submit raw *** Begin Patch / *** End Patch text directly.",
                        )
                    });
            }
            Err(invalid_tool_input(
                NAME,
                "ApplyPatch expects `patch`, not `input`",
                serde_json::json!({
                    "field": "input",
                    "expected_field": "patch",
                }),
                "ApplyPatch expects exactly {\"patch\": \"--- a/path\\n+++ b/path\\n@@ -1,1 +1,1 @@\\n-old\\n+new\\n\"}. Do not use \"input\".",
            ))
        }
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

pub fn render_diagnostic_for_model(diagnostic: &ApplyPatchDiagnostic) -> String {
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

pub fn render_diagnostics_summary(diagnostics: &[ApplyPatchDiagnostic]) -> String {
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

pub fn render_changed_file_summary(file: &crate::types::ApplyPatchChangedFile) -> String {
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
