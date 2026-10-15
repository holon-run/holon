//! Finite RunOnce fields borrowed from an original canonical Tool source.
use crate::types::ToolExecutionRecord;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ToolRunReportProjection {
    pub patch_paths: Vec<String>,
    pub output_paths: Vec<String>,
    pub sleep_reason: Option<String>,
    #[serde(default)]
    pub batched_exec_command_items: usize,
}

/// The original Input actor admits the new projection into its remaining phase
/// before this function owns a path, reason or container. This grants no alias.
pub(crate) fn project_tool_run_report(
    record: &ToolExecutionRecord,
    admit: impl FnOnce(u64, usize) -> Result<()>,
) -> Result<ToolRunReportProjection> {
    project_tool_run_report_fields(&record.tool_name, &record.input, &record.output, admit)
}
/// Shared by the exact canonical writer and the finite original SQL projection.
/// Callers own source qualification and admit into their existing material phase.
pub(crate) fn project_tool_run_report_fields(
    tool_name: &str,
    input: &serde_json::Value,
    output: &serde_json::Value,
    admit: impl FnOnce(u64, usize) -> Result<()>,
) -> Result<ToolRunReportProjection> {
    let mut bytes = 512u64;
    let mut records = 8usize;
    let mut input_paths = 0usize;
    let mut charge = |path: &str| -> Result<()> {
        bytes = bytes
            .checked_add(super::input_evidence_budget::serialized_bytes(&path)?)
            .and_then(|n| n.checked_add(256))
            .context("Run report path bytes overflow")?;
        records = records
            .checked_add(1)
            .context("Run report path count overflow")?;
        Ok(())
    };
    if tool_name == "ApplyPatch" {
        if let Some(patch) = patch_input(input) {
            visit_patch_paths(patch, |path| {
                input_paths = input_paths
                    .checked_add(1)
                    .context("Run report input count overflow")?;
                charge(path)
            })?;
        }
        if input_paths == 0 {
            for path in output_paths(output) {
                charge(path)?;
            }
        }
    }
    if let Some(reason) = sleep_reason(tool_name, output) {
        charge(reason)?;
    }
    let batched_exec_command_items = if tool_name == "ExecCommandBatch" {
        let result = output.get("envelope").and_then(|value| value.get("result"));
        let completed = result
            .and_then(|value| value.get("completed_count"))
            .and_then(serde_json::Value::as_u64)
            .map_or(0, |value| value as usize);
        let failed = result
            .and_then(|value| value.get("failed_count"))
            .and_then(serde_json::Value::as_u64)
            .map_or(0, |value| value as usize);
        completed
            .checked_add(failed)
            .context("Run batch result counter overflow")?
    } else {
        0
    };
    // Duplicate source paths are conservatively charged before deduplication.
    admit(bytes, records)?;
    let mut result = ToolRunReportProjection {
        batched_exec_command_items,
        ..Default::default()
    };
    if tool_name == "ApplyPatch" {
        if input_paths > 0 {
            visit_patch_paths(patch_input(input).expect("borrowed path source"), |path| {
                if !result.patch_paths.iter().any(|seen| seen == path) {
                    result.patch_paths.push(path.to_owned());
                }
                Ok(())
            })?;
        } else {
            result.output_paths = output_paths(output).map(str::to_owned).collect();
        }
    }
    result.sleep_reason = sleep_reason(tool_name, output).map(str::to_owned);
    Ok(result)
}
fn patch_input(input: &serde_json::Value) -> Option<&str> {
    input
        .as_str()
        .or_else(|| input.get("patch").and_then(serde_json::Value::as_str))
}
fn output_paths(output: &serde_json::Value) -> impl Iterator<Item = &str> {
    output
        .get("envelope")
        .and_then(|v| v.get("result"))
        .and_then(|v| v.get("changed_paths"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
}
fn sleep_reason<'a>(tool_name: &str, output: &'a serde_json::Value) -> Option<&'a str> {
    if tool_name != crate::tool::names::SLEEP {
        return None;
    }
    output
        .get("envelope")
        .and_then(|v| v.get("result"))
        .and_then(|v| v.get("reason"))
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            output
                .get("envelope")
                .and_then(|v| v.get("summary_text"))
                .and_then(serde_json::Value::as_str)
        })
        .map(str::trim)
        .filter(|text| !text.is_empty())
}
fn strip_diff_prefix(path: &str) -> &str {
    path.strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path)
}
/// Same rename/unified-diff rules as RunOnce; no Vec of every patch line.
fn visit_patch_paths<'a>(
    input: &'a str,
    mut visit: impl FnMut(&'a str) -> Result<()>,
) -> Result<()> {
    let mut pending_rename_from = None;
    let mut lines = input.lines().peekable();
    while let Some(line) = lines.next() {
        if let Some(path) = line.strip_prefix("rename from ") {
            pending_rename_from = Some(strip_diff_prefix(path));
            continue;
        }
        if let Some(path) = line.strip_prefix("rename to ") {
            if let Some(from) = pending_rename_from.take() {
                visit(from)?;
                visit(strip_diff_prefix(path))?;
            }
            continue;
        }
        if let Some(old) = line.strip_prefix("--- ") {
            if let Some(new) = lines.peek().and_then(|next| next.strip_prefix("+++ ")) {
                let old = strip_diff_prefix(old);
                let new = strip_diff_prefix(new);
                if old != "/dev/null" {
                    visit(old)?;
                }
                if new != "/dev/null" && new != old {
                    visit(new)?;
                }
                lines.next();
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AuthorityClass, ToolExecutionStatus};
    fn tool(name: &str) -> ToolExecutionRecord {
        ToolExecutionRecord {
            id: "run-report".into(),
            agent_id: "report-owner".into(),
            work_item_id: None,
            turn_index: 1,
            turn_id: Some("original-turn".into()),
            tool_name: name.into(),
            created_at: chrono::Utc::now(),
            completed_at: None,
            duration_ms: 0,
            authority_class: AuthorityClass::OperatorInstruction,
            status: ToolExecutionStatus::Success,
            input: serde_json::Value::Null,
            output: serde_json::Value::Null,
            summary: "actual source".into(),
            invocation_surface: None,
        }
    }
    #[test]
    fn run_report_preserves_rename_dedup_and_input_preference_before_output_fallback() -> Result<()>
    {
        let mut original = tool("ApplyPatch");
        original.input = serde_json::json!({"patch":"rename from a/old.rs\nrename to b/new.rs\n--- a/old.rs\n+++ b/new.rs\n--- /dev/null\n+++ b/added.rs\n"});
        original.output =
            serde_json::json!({"envelope":{"result":{"changed_paths":["wrong-fallback"]}}});
        let report = project_tool_run_report(&original, |bytes, nodes| {
            assert!(bytes > 512);
            assert!(nodes > 8);
            Ok(())
        })?;
        assert_eq!(report.patch_paths, vec!["old.rs", "new.rs", "added.rs"]);
        assert!(report.output_paths.is_empty());
        original.input = serde_json::json!({"patch":"@@ no file headers @@"});
        let report = project_tool_run_report(&original, |_, _| Ok(()))?;
        assert!(report.patch_paths.is_empty());
        assert_eq!(report.output_paths, vec!["wrong-fallback"]);
        Ok(())
    }
    #[test]
    fn run_report_refused_admission_keeps_source_and_large_stdout_is_never_projected() -> Result<()>
    {
        let mut original = tool("ApplyPatch");
        original.input = serde_json::json!({"patch":format!("--- a/{}\n+++ b/{}\n","long-path".repeat(4096),"long-path".repeat(4096))});
        let error = project_tool_run_report(&original, |bytes, _| {
            anyhow::ensure!(bytes <= 4096, "original remaining phase exhausted");
            Ok(())
        })
        .err()
        .context("oversized paths must reject at original admission")?;
        assert!(error.to_string().contains("remaining phase exhausted"));
        let mut stdout = tool("ExecCommand");
        stdout.output = serde_json::json!({"stdout":"x".repeat(10*1024*1024)});
        let report = project_tool_run_report(&stdout, |bytes, nodes| {
            assert_eq!((bytes, nodes), (512, 8));
            Ok(())
        })?;
        drop(stdout);
        assert_eq!(report, ToolRunReportProjection::default());
        Ok(())
    }
    #[test]
    fn run_report_sleep_uses_original_reason_then_summary_without_other_tool_adoption() -> Result<()>
    {
        let mut original = tool(crate::tool::names::SLEEP);
        original.output = serde_json::json!({"envelope":{"result":{"reason":" original wait "},"summary_text":"fallback"}});
        assert_eq!(
            project_tool_run_report(&original, |_, _| Ok(()))?
                .sleep_reason
                .as_deref(),
            Some("original wait")
        );
        original.output = serde_json::json!({"envelope":{"summary_text":" fallback "}});
        assert_eq!(
            project_tool_run_report(&original, |_, _| Ok(()))?
                .sleep_reason
                .as_deref(),
            Some("fallback")
        );
        original.tool_name = "Other".into();
        assert!(project_tool_run_report(&original, |_, _| Ok(()))?
            .sleep_reason
            .is_none());
        Ok(())
    }
}
