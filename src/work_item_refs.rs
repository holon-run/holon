use std::collections::BTreeMap;

use chrono::Utc;
use serde_json::Value;
use url::Url;

use crate::{
    tool::helpers::{command_output_source_ref, command_preview, command_receipt_source_ref},
    types::{
        MessageBody, MessageEnvelope, ToolExecutionRecord, WorkItemRef, WorkItemRefKind,
        WorkItemRefStatus,
    },
};

pub const MAX_ACTIVE_WORK_REFS: usize = 12;

pub fn current_turn_tool_refs(
    tools: &[ToolExecutionRecord],
    current_turn_id: Option<&str>,
    current_turn_index: u64,
    current_work_item_id: &str,
) -> Vec<WorkItemRef> {
    let mut refs = Vec::new();
    for tool in tools {
        if !tool_belongs_to_current_turn(tool, current_turn_id, current_turn_index) {
            continue;
        }
        if tool
            .work_item_id
            .as_deref()
            .is_some_and(|id| id != current_work_item_id)
        {
            continue;
        }
        refs.extend(extract_tool_refs(tool));
    }
    refs
}

pub fn message_work_refs(message: &MessageEnvelope) -> Vec<WorkItemRef> {
    let mut refs = Vec::new();
    for source_ref in message.source_refs.values() {
        if let Some(work_ref) = work_ref_from_source_ref(source_ref, "message source ref") {
            refs.push(work_ref);
        }
        refs.extend(extract_github_refs(source_ref, "message source ref", None));
    }
    refs.extend(extract_github_refs(
        &message_body_text(&message.body),
        "current input",
        None,
    ));
    refs
}

pub fn merge_work_refs(existing: &[WorkItemRef], additions: Vec<WorkItemRef>) -> Vec<WorkItemRef> {
    let mut merged = BTreeMap::<(WorkItemRefKind, String), WorkItemRef>::new();
    for work_ref in existing.iter().chain(additions.iter()) {
        if work_ref.ref_id.trim().is_empty() {
            continue;
        }
        let key = (work_ref.kind, work_ref.ref_id.clone());
        merged
            .entry(key)
            .and_modify(|existing| {
                if work_ref.last_seen_at >= existing.last_seen_at {
                    let title = work_ref.title.clone().or_else(|| existing.title.clone());
                    let source_ref = work_ref
                        .source_ref
                        .clone()
                        .or_else(|| existing.source_ref.clone());
                    let mut metadata = existing.metadata.clone();
                    metadata.extend(work_ref.metadata.clone());
                    let material_changed = existing.title != title
                        || existing.reason != work_ref.reason
                        || existing.status != work_ref.status
                        || existing.source_ref != source_ref
                        || existing.metadata != metadata;
                    if material_changed {
                        existing.title = title;
                        existing.reason = work_ref.reason.clone();
                        existing.status = work_ref.status;
                        existing.last_seen_at = work_ref.last_seen_at;
                        existing.source_ref = source_ref;
                        existing.metadata = metadata;
                    }
                }
            })
            .or_insert_with(|| work_ref.clone());
    }

    let mut refs = merged.into_values().collect::<Vec<_>>();
    refs.sort_by(|left, right| {
        ref_status_rank(left.status)
            .cmp(&ref_status_rank(right.status))
            .then_with(|| right.last_seen_at.cmp(&left.last_seen_at))
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.ref_id.cmp(&right.ref_id))
    });

    let mut active_seen = 0usize;
    refs.into_iter()
        .filter(|work_ref| {
            if work_ref.status != WorkItemRefStatus::Active {
                return true;
            }
            active_seen += 1;
            active_seen <= MAX_ACTIVE_WORK_REFS
        })
        .collect()
}

pub fn extract_tool_refs(tool: &ToolExecutionRecord) -> Vec<WorkItemRef> {
    let mut refs = Vec::new();
    match tool.tool_name.as_str() {
        "ApplyPatch" => {
            if let Some(input) = tool
                .input
                .as_str()
                .or_else(|| tool.input.get("patch").and_then(Value::as_str))
            {
                for path in extract_patch_files(input) {
                    refs.push(work_ref(
                        WorkItemRefKind::File,
                        path.clone(),
                        Some(path),
                        "file changed by ApplyPatch",
                        None,
                    ));
                }
            }
        }
        "ExecCommand" => {
            if let Some(cmd) = tool.input.get("cmd").and_then(Value::as_str) {
                let cmd_ref = command_receipt_source_ref(&tool.id, None);
                refs.push(work_ref(
                    WorkItemRefKind::ToolExecution,
                    cmd_ref.clone(),
                    Some(command_preview(cmd)),
                    "command executed for this WorkItem",
                    Some(cmd_ref.clone()),
                ));
                refs.push(work_ref(
                    WorkItemRefKind::ToolExecution,
                    command_output_source_ref(&tool.id, None, "output"),
                    Some(format!("{} output", command_preview(cmd))),
                    "command output may need reopening",
                    Some(command_output_source_ref(&tool.id, None, "output")),
                ));
                refs.extend(extract_github_refs(
                    cmd,
                    "GitHub command inspected",
                    Some(cmd_ref),
                ));
            }
        }
        "ExecCommandBatch" => {
            if let Some(items) = tool.input.get("items").and_then(Value::as_array) {
                for (offset, item) in items.iter().enumerate() {
                    if let Some(cmd) = item.get("cmd").and_then(Value::as_str) {
                        let index = offset + 1;
                        let cmd_ref = command_receipt_source_ref(&tool.id, Some(index));
                        refs.push(work_ref(
                            WorkItemRefKind::ToolExecution,
                            cmd_ref.clone(),
                            Some(command_preview(cmd)),
                            "batch command executed for this WorkItem",
                            Some(cmd_ref.clone()),
                        ));
                        refs.push(work_ref(
                            WorkItemRefKind::ToolExecution,
                            command_output_source_ref(&tool.id, Some(index), "output"),
                            Some(format!("{} output", command_preview(cmd))),
                            "batch command output may need reopening",
                            Some(command_output_source_ref(&tool.id, Some(index), "output")),
                        ));
                        refs.extend(extract_github_refs(
                            cmd,
                            "GitHub command inspected",
                            Some(cmd_ref),
                        ));
                    }
                }
            }
        }
        "MemoryGet" | "MemorySearch" => {
            if let Some(source_ref) = tool.input.get("source_ref").and_then(Value::as_str) {
                if let Some(work_ref) = work_ref_from_source_ref(source_ref, "runtime memory used")
                {
                    refs.push(work_ref);
                }
            }
        }
        _ => {}
    }
    refs
}

fn tool_belongs_to_current_turn(
    tool: &ToolExecutionRecord,
    current_turn_id: Option<&str>,
    current_turn_index: u64,
) -> bool {
    if let Some(turn_id) = current_turn_id {
        return tool.turn_id.as_deref() == Some(turn_id);
    }
    tool.turn_index == current_turn_index.max(1)
}

fn work_ref_from_source_ref(source_ref: &str, reason: &str) -> Option<WorkItemRef> {
    let (kind, title) = if source_ref.starts_with("tool_execution:") {
        (WorkItemRefKind::ToolExecution, "tool execution")
    } else if source_ref.starts_with("task:") {
        (WorkItemRefKind::Task, "task")
    } else if source_ref.starts_with("work_item:") {
        (WorkItemRefKind::Other, "work item")
    } else if source_ref.starts_with("agent_memory:")
        || source_ref.starts_with("brief:")
        || source_ref.starts_with("turn:")
        || source_ref.starts_with("episode:")
    {
        (WorkItemRefKind::Memory, "memory")
    } else if source_ref.starts_with("workspace_profile:") {
        (WorkItemRefKind::Workspace, "workspace")
    } else {
        return None;
    };
    Some(work_ref(
        kind,
        source_ref.to_string(),
        Some(title.to_string()),
        reason,
        Some(source_ref.to_string()),
    ))
}

fn extract_github_refs(text: &str, reason: &str, source_ref: Option<String>) -> Vec<WorkItemRef> {
    let mut refs = Vec::new();
    refs.extend(extract_github_url_refs(text, reason, source_ref.clone()));
    refs.extend(extract_gh_command_refs(text, reason, source_ref));
    refs
}

fn extract_github_url_refs(
    text: &str,
    reason: &str,
    source_ref: Option<String>,
) -> Vec<WorkItemRef> {
    text.split_whitespace()
        .filter_map(|token| {
            let token = token
                .trim_matches(|ch: char| matches!(ch, '"' | '\'' | '`' | ',' | ')' | ']' | '}'));
            let parsed = Url::parse(token).ok()?;
            if parsed.host_str() != Some("github.com") {
                return None;
            }
            let segments = parsed.path_segments()?.collect::<Vec<_>>();
            if segments.len() < 4 {
                return None;
            }
            let owner = segments[0];
            let repo = segments[1];
            let number = segments[3].parse::<u64>().ok()?;
            match segments[2] {
                "issues" => Some(github_ref(
                    WorkItemRefKind::Issue,
                    owner,
                    repo,
                    number,
                    reason,
                    source_ref.clone(),
                )),
                "pull" => Some(github_ref(
                    WorkItemRefKind::Pr,
                    owner,
                    repo,
                    number,
                    reason,
                    source_ref.clone(),
                )),
                _ => None,
            }
        })
        .collect()
}

fn extract_gh_command_refs(
    text: &str,
    reason: &str,
    source_ref: Option<String>,
) -> Vec<WorkItemRef> {
    let words = shell_words(text);
    if words.len() < 5 || words.first().map(String::as_str) != Some("gh") {
        return Vec::new();
    }
    let kind = match words.get(1).map(String::as_str) {
        Some("issue") => WorkItemRefKind::Issue,
        Some("pr") => WorkItemRefKind::Pr,
        _ => return Vec::new(),
    };
    let number = words
        .iter()
        .skip(2)
        .find_map(|word| word.parse::<u64>().ok());
    let Some(number) = number else {
        return Vec::new();
    };
    let repo = words.windows(2).find_map(|window| {
        if window[0] == "--repo" || window[0] == "-R" {
            Some(window[1].as_str())
        } else {
            None
        }
    });
    let Some((owner, repo)) = repo.and_then(|repo| repo.split_once('/')) else {
        return Vec::new();
    };
    vec![github_ref(kind, owner, repo, number, reason, source_ref)]
}

fn github_ref(
    kind: WorkItemRefKind,
    owner: &str,
    repo: &str,
    number: u64,
    reason: &str,
    source_ref: Option<String>,
) -> WorkItemRef {
    let prefix = match kind {
        WorkItemRefKind::Issue => "issue",
        WorkItemRefKind::Pr => "pr",
        _ => "github",
    };
    work_ref(
        kind,
        format!("github:{owner}/{repo}#{number}"),
        Some(format!("{owner}/{repo}#{number}")),
        reason,
        source_ref,
    )
    .with_metadata("github_kind", prefix)
}

fn work_ref(
    kind: WorkItemRefKind,
    ref_id: String,
    title: Option<String>,
    reason: &str,
    source_ref: Option<String>,
) -> WorkItemRef {
    WorkItemRef {
        kind,
        ref_id,
        title,
        reason: reason.to_string(),
        status: WorkItemRefStatus::Active,
        last_seen_at: Utc::now(),
        source_ref,
        metadata: serde_json::Map::new(),
    }
}

trait WorkItemRefExt {
    fn with_metadata(self, key: &str, value: &str) -> Self;
}

impl WorkItemRefExt for WorkItemRef {
    fn with_metadata(mut self, key: &str, value: &str) -> Self {
        self.metadata
            .insert(key.to_string(), Value::String(value.to_string()));
        self
    }
}

fn message_body_text(body: &MessageBody) -> String {
    match body {
        MessageBody::Text { text } => text.clone(),
        MessageBody::Brief { title, text, .. } => title
            .as_deref()
            .map(|title| format!("{title}\n{text}"))
            .unwrap_or_else(|| text.clone()),
        MessageBody::Json { value } => value.to_string(),
    }
}

fn shell_words(input: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for ch in input.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if let Some(open) = quote {
            if ch == open {
                quote = None;
            } else {
                current.push(ch);
            }
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            continue;
        }
        if ch.is_whitespace() {
            if !current.is_empty() {
                words.push(current);
                current = String::new();
            }
            continue;
        }
        current.push(ch);
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn extract_patch_files(input: &str) -> Vec<String> {
    let lines = input.lines().collect::<Vec<_>>();
    let mut files = Vec::new();
    let mut pending_rename_from: Option<String> = None;
    let mut index = 0usize;
    while index < lines.len() {
        if let Some(path) = lines[index].strip_prefix("rename from ") {
            pending_rename_from = Some(strip_diff_prefix(path).to_string());
            index += 1;
            continue;
        }
        if let Some(path) = lines[index].strip_prefix("rename to ") {
            if let Some(from) = pending_rename_from.take() {
                push_unique_patch_file(&mut files, from);
                push_unique_patch_file(&mut files, strip_diff_prefix(path).to_string());
            }
            index += 1;
            continue;
        }
        if let Some(old_path) = lines[index].strip_prefix("--- ") {
            if index + 1 < lines.len() {
                if let Some(new_path) = lines[index + 1].strip_prefix("+++ ") {
                    for path in [old_path, new_path] {
                        let path = strip_diff_prefix(path);
                        if path != "/dev/null" {
                            push_unique_patch_file(&mut files, path.to_string());
                        }
                    }
                    index += 2;
                    continue;
                }
            }
        }
        index += 1;
    }
    files
}

fn strip_diff_prefix(path: &str) -> &str {
    path.strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path)
        .trim()
}

fn push_unique_patch_file(files: &mut Vec<String>, path: String) {
    if !path.is_empty() && !files.iter().any(|existing| existing == &path) {
        files.push(path);
    }
}

fn ref_status_rank(status: WorkItemRefStatus) -> u8 {
    match status {
        WorkItemRefStatus::Active => 0,
        WorkItemRefStatus::Stale => 1,
        WorkItemRefStatus::Resolved => 2,
        WorkItemRefStatus::Archived => 3,
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;

    use super::*;
    use crate::types::{AuthorityClass, ToolExecutionRecord, ToolExecutionStatus};

    fn tool(id: &str, name: &str, input: Value) -> ToolExecutionRecord {
        ToolExecutionRecord {
            id: id.to_string(),
            agent_id: "default".into(),
            tool_name: name.into(),
            input,
            status: ToolExecutionStatus::Success,
            output: Value::Null,
            summary: String::new(),
            created_at: Utc::now(),
            completed_at: None,
            duration_ms: 0,
            authority_class: AuthorityClass::RuntimeInstruction,
            turn_index: 1,
            turn_id: Some("turn-1".into()),
            work_item_id: Some("work-1".into()),
            invocation_surface: None,
        }
    }

    #[test]
    fn apply_patch_extracts_file_refs() {
        let refs = extract_tool_refs(&tool(
            "tool-patch",
            "ApplyPatch",
            json!({
                "patch": "--- a/src/old.rs\n+++ b/src/new.rs\n@@\n-old\n+new"
            }),
        ));

        assert!(refs
            .iter()
            .any(|work_ref| work_ref.kind == WorkItemRefKind::File
                && work_ref.ref_id == "src/old.rs"));
        assert!(refs
            .iter()
            .any(|work_ref| work_ref.kind == WorkItemRefKind::File
                && work_ref.ref_id == "src/new.rs"));
    }

    #[test]
    fn github_refs_are_conservative() {
        let refs = extract_github_refs(
            "gh issue view 1662 --repo holon-run/holon",
            "test",
            Some("tool_execution:tool-1:cmd".into()),
        );
        assert_eq!(refs[0].kind, WorkItemRefKind::Issue);
        assert_eq!(refs[0].ref_id, "github:holon-run/holon#1662");

        let ambiguous = extract_github_refs("see #1662", "test", None);
        assert!(ambiguous.is_empty());
    }

    #[test]
    fn merge_work_refs_deduplicates_and_limits_active_refs() {
        let now = Utc::now();
        let existing = vec![WorkItemRef {
            kind: WorkItemRefKind::File,
            ref_id: "src/lib.rs".into(),
            title: Some("old".into()),
            reason: "old reason".into(),
            status: WorkItemRefStatus::Active,
            last_seen_at: now,
            source_ref: None,
            metadata: serde_json::Map::new(),
        }];
        let additions = vec![WorkItemRef {
            kind: WorkItemRefKind::File,
            ref_id: "src/lib.rs".into(),
            title: Some("new".into()),
            reason: "new reason".into(),
            status: WorkItemRefStatus::Active,
            last_seen_at: now + chrono::Duration::seconds(1),
            source_ref: None,
            metadata: serde_json::Map::new(),
        }];

        let merged = merge_work_refs(&existing, additions);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].title.as_deref(), Some("new"));
        assert_eq!(merged[0].reason, "new reason");
    }

    #[test]
    fn merge_work_refs_ignores_last_seen_only_changes() {
        let now = Utc::now();
        let existing = vec![WorkItemRef {
            kind: WorkItemRefKind::File,
            ref_id: "src/lib.rs".into(),
            title: Some("same".into()),
            reason: "same reason".into(),
            status: WorkItemRefStatus::Active,
            last_seen_at: now,
            source_ref: Some("turn:old".into()),
            metadata: serde_json::Map::new(),
        }];
        let additions = vec![WorkItemRef {
            kind: WorkItemRefKind::File,
            ref_id: "src/lib.rs".into(),
            title: Some("same".into()),
            reason: "same reason".into(),
            status: WorkItemRefStatus::Active,
            last_seen_at: now + chrono::Duration::seconds(1),
            source_ref: Some("turn:old".into()),
            metadata: serde_json::Map::new(),
        }];

        let merged = merge_work_refs(&existing, additions);
        assert_eq!(merged, existing);
    }
}
