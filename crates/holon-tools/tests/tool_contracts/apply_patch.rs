use super::serialize_success;
use holon_domain::{
    ApplyPatchAction, ApplyPatchChangedFile, ApplyPatchDiagnostic, ApplyPatchResult,
    ApplyPatchSurface,
};
use holon_tools::builtins::apply_patch_tool::*;
use holon_tools::{ToolError, ToolResult};
use serde_json::json;

#[test]
fn apply_patch_renders_text_receipt() {
    let result = serialize_success(
        NAME,
        &ApplyPatchResult {
            changed_files: vec![
                ApplyPatchChangedFile {
                    action: ApplyPatchAction::Modify,
                    path: "src/lib.rs".into(),
                    from_path: None,
                    hunks: Vec::new(),
                    diff_preview: None,
                    diff_truncated: false,
                },
                ApplyPatchChangedFile {
                    action: ApplyPatchAction::Add,
                    path: "README.md".into(),
                    from_path: None,
                    hunks: Vec::new(),
                    diff_preview: None,
                    diff_truncated: false,
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
            changed_files: vec![ApplyPatchChangedFile {
                action: ApplyPatchAction::Modify,
                path: "src/lib.rs".into(),
                from_path: None,
                hunks: Vec::new(),
                diff_preview: None,
                diff_truncated: false,
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
fn apply_patch_replacement_character_warning_is_model_visible() {
    let diagnostic = replacement_character_diagnostic();
    assert_eq!(diagnostic.kind, "replacement_character_in_input");
    assert!(render_diagnostic_for_model(&diagnostic).contains("U+FFFD"));

    let error = add_replacement_character_recovery_hint(ToolError::new(
        "context_not_found",
        "patch context was not found",
    ));
    assert_eq!(error.kind, "context_not_found");
    assert!(error
        .recovery_hint
        .as_deref()
        .is_some_and(|hint| hint.contains("do not retry")));
}

#[test]
fn apply_patch_both_surfaces_preserve_replacement_character_for_warning() {
    let codex = extract_patch_input(
        &json!("*** Begin Patch\n*** Add File: x\n+�\n*** End Patch\n"),
        ApplyPatchSurface::CodexDslFreeform,
    )
    .unwrap();
    let unified = extract_patch_input(
        &json!({
            "patch": "--- /dev/null\n+++ b/x\n@@ -0,0 +1 @@\n+�\n"
        }),
        ApplyPatchSurface::UnifiedDiffJson,
    )
    .unwrap();

    assert!(codex.contains('\u{FFFD}'));
    assert!(unified.contains('\u{FFFD}'));
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
            changed_files: vec![ApplyPatchChangedFile {
                action: ApplyPatchAction::Modify,
                path: "src/lib.rs".into(),
                from_path: None,
                hunks: Vec::new(),
                diff_preview: None,
                diff_truncated: false,
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
fn apply_patch_truncated_mutation_error_does_not_force_file_inspection() {
    let result = ToolResult::error(
        NAME,
        ToolError::new(
            "truncated_mutation_tool_call",
            "ApplyPatch was not executed because the provider stopped with max_tokens",
        )
        .with_recovery_hint("Inspect only the necessary context")
        .with_retryable(true),
    );
    let rendered = render_for_model(&result).unwrap();

    assert!(rendered.contains("ApplyPatch failed"));
    assert!(rendered.contains("truncated_mutation_tool_call"));
    assert!(rendered.contains("Inspect only the necessary context"));
    assert!(rendered.contains("retryable: true"));
    assert!(!rendered.contains("inspect the target file before retrying"));
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
    let error = extract_patch_input(
        &serde_json::json!({
            "input": "--- a/old.txt\n+++ b/old.txt\n@@ -1,1 +1,1 @@\n-old\n+new\n"
        }),
        ApplyPatchSurface::UnifiedDiffJson,
    )
    .unwrap_err();
    let tool_error = error.downcast_ref::<ToolError>().unwrap();
    assert_eq!(tool_error.kind, "invalid_tool_input");
    assert!(tool_error
        .recovery_hint
        .as_deref()
        .unwrap_or_default()
        .contains("Do not use \"input\""));
}

#[test]
fn apply_patch_codex_freeform_empty_string_uses_input_label() {
    let error = extract_patch_input(
        &serde_json::json!("   "),
        ApplyPatchSurface::CodexDslFreeform,
    )
    .unwrap_err();
    let tool_error = error.downcast_ref::<ToolError>().unwrap();
    assert_eq!(tool_error.kind, "invalid_tool_input");
    assert_eq!(tool_error.details.as_ref().unwrap()["field"], "input");
    assert!(!tool_error.message.contains("`patch`"));
}

#[test]
fn apply_patch_function_fallback_uses_patch_field() {
    let spec = definition().unwrap().spec;
    assert!(spec.input_schema["properties"]["patch"].is_object());
    assert!(spec.input_schema["properties"]["input"].is_null());
    assert!(spec.freeform_grammar.is_none());
    assert!(!spec.description.contains("*** Begin Patch"));
    assert!(!spec.description.contains("*** End Patch"));
    assert!(!spec.input_schema.to_string().contains("*** Begin Patch"));
    assert!(!spec.input_schema.to_string().contains("*** End Patch"));
}

#[test]
fn apply_patch_codex_surface_uses_freeform_dsl_grammar() {
    let spec = definition_for_surface(ApplyPatchSurface::CodexDslFreeform)
        .unwrap()
        .spec;
    assert!(spec.input_schema["type"] == "string");
    let grammar = spec.freeform_grammar.expect("codex grammar");
    assert!(grammar.definition.contains("*** Begin Patch"));
    assert!(!spec.description.contains("unified diff"));
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
fn apply_patch_codex_freeform_grammar_requires_codex_hunks() {
    let grammar = definition_for_surface(ApplyPatchSurface::CodexDslFreeform)
        .unwrap()
        .spec
        .freeform_grammar
        .expect("apply patch should expose freeform grammar")
        .definition;
    assert!(grammar.contains("begin_patch: \"*** Begin Patch\" LF"));
    assert!(grammar.contains("update_hunk: \"*** Update File: \""));
    assert!(grammar.contains("change_move: \"*** Move to: \""));
    assert!(!grammar.contains("old_file: \"--- \" file_path LF"));
}
