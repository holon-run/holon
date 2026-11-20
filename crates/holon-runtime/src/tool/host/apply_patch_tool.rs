use super::serialize_success;
use crate::runtime::RuntimeHandle;
use crate::system::ExecutionScopeKind;
use crate::tool::apply_patch;
#[cfg(test)]
use crate::tool::apply_patch::ApplyPatchSurface;
#[cfg(test)]
use crate::tool::ToolResult;
use crate::types::ApplyPatchResult;
use crate::types::AuthorityClass;
use anyhow::Result;
use serde_json::Value;

pub(crate) use holon_tools::builtins::apply_patch_tool::*;

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let surface = runtime.current_apply_patch_surface().await;
    let input_material = runtime.context_resource_material()?;
    let patch_input = {
        let _io = input_material.enter();
        crate::diagnostics::resource_io::charge_owned_metadata(
            crate::runtime::input_evidence_budget::serialized_bytes(input)?
                .checked_mul(4)
                .ok_or_else(|| anyhow::anyhow!("patch input material overflow"))?
                + 8192,
            128,
        )?;
        extract_patch_input(input, surface)?
    };
    let contains_replacement_character = patch_input.contains('\u{FFFD}');
    let execution = runtime
        .effective_execution(ExecutionScopeKind::AgentTurn)
        .await?;
    let admission = runtime.execution_admission_lease()?;
    let resource_runtime = runtime.clone();
    let (send, receive) = tokio::sync::oneshot::channel();
    runtime.spawn_owned_mutation(async move {
        let result = tokio::task::spawn_blocking(move || {
            let _admission = admission;
            let outcome = apply_patch::apply_patch_with_material_and_writer(
                execution.workspace.execution_root(),
                &patch_input,
                surface,
                &input_material,
                |paths| {
                    let writer = resource_runtime.begin_context_resource_write_for_paths(paths)?;
                    writer.enter_physical()?;
                    Ok(writer)
                },
            );
            (outcome, input_material)
        })
        .await;
        let _ = send.send(result);
    })?;
    let (outcome, _input_material) = receive
        .await
        .map_err(|error| anyhow::anyhow!("patch physical worker ended before returning: {error}"))?
        .map_err(|error| anyhow::anyhow!("patch physical worker failed: {error}"))?;
    let mut outcome = outcome.map_err(|error| {
        if contains_replacement_character {
            anyhow::Error::new(add_replacement_character_recovery_hint(
                crate::tool::tool_error_from_anyhow(&error),
            ))
        } else {
            error
        }
    })?;
    if contains_replacement_character {
        outcome.diagnostics.push(replacement_character_diagnostic());
    }
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

#[cfg(holon_lib_test_shard = "c")]
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn apply_patch_sentinel_failure_receipt_does_not_echo_raw_tail() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("sample.txt");
        tokio::fs::write(&file, "old\n").await.unwrap();
        let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@ -1 +1 @@
-old
+new
*** End Patch trailing garbage
"#;

        let error = apply_patch::apply_patch(dir.path(), patch, ApplyPatchSurface::UnifiedDiffJson)
            .await
            .unwrap_err();
        let result = ToolResult::error(NAME, crate::tool::tool_error_from_anyhow(&error));
        let rendered = render_for_model(&result).unwrap();

        assert!(rendered.contains("- kind: invalid_patch_syntax"));
        assert!(rendered.contains("standalone patch sentinel"));
        assert!(rendered.contains("- details: omitted from model-visible receipt"));
        assert!(!rendered.contains("*** End Patch"));
        assert!(!rendered.contains("trailing garbage"));
        assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), "old\n");
    }
}
