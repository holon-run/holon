use super::*;

#[test]
fn min_display_levels() {
    assert_eq!(
        PresentationItem::UserMessage { text: "hi".into() }.min_display_level(),
        3
    );
    assert_eq!(
        PresentationItem::AssistantProgress {
            text: "...".into(),
            state: ItemState::Stable
        }
        .min_display_level(),
        4
    );
    assert_eq!(
        PresentationItem::ProviderRound {
            model: "gpt-4".into(),
            stop_reason: "end_turn".into(),
            tokens: TokenCount::default()
        }
        .min_display_level(),
        5
    );
}

#[test]
fn is_visible_at() {
    let item = PresentationItem::CommandExecuted {
        status: CommandStatus::Completed,
        cmd_preview: "cargo test".into(),
        task_id: None,
        duration_ms: Some(1000),
        exit_code: Some(0),
        stdout_summary: "".into(),
        full_stdout: None,
        full_stderr: None,
        error_message: None,
    };
    assert!(!item.is_visible_at(3));
    assert!(item.is_visible_at(4));
    assert!(item.is_visible_at(5));
}

#[test]
fn command_render_level_4() {
    let item = PresentationItem::CommandExecuted {
        status: CommandStatus::Completed,
        cmd_preview: "cargo test --lib".into(),
        task_id: None,
        duration_ms: Some(2300),
        exit_code: Some(0),
        stdout_summary: "5 passed".into(),
        full_stdout: Some("running 5 tests\ntest result: ok".into()),
        full_stderr: None,
        error_message: None,
    };
    let cells = item.render(4);
    assert_eq!(cells.len(), 1);
    assert!(cells[0].body.contains("cargo test --lib"));
    assert!(cells[0].body.contains("2.3s"));
    assert!(!cells[0].body.contains("5 passed"));
    assert!(!cells[0].body.contains("running 5 tests"));
}

#[test]
fn command_render_level_5() {
    let item = PresentationItem::CommandExecuted {
        status: CommandStatus::Completed,
        cmd_preview: "cargo test --lib".into(),
        task_id: None,
        duration_ms: Some(2300),
        exit_code: Some(0),
        stdout_summary: "5 passed".into(),
        full_stdout: Some("running 5 tests\ntest result: ok".into()),
        full_stderr: None,
        error_message: None,
    };
    let cells = item.render(5);
    assert_eq!(cells.len(), 1);
    assert!(cells[0].body.contains("running 5 tests"));
    assert!(cells[0].body.contains("test result: ok"));
}

#[test]
fn command_render_level_5_includes_full_error_message() {
    let item = PresentationItem::CommandExecuted {
        status: CommandStatus::Failed,
        cmd_preview: "cargo test --lib".into(),
        task_id: None,
        duration_ms: Some(2300),
        exit_code: None,
        stdout_summary: "".into(),
        full_stdout: None,
        full_stderr: Some("stderr line 1\nstderr line 2".into()),
        error_message: Some("error line 1\nerror line 2\nerror line 3".into()),
    };
    let body = &item.render(5)[0].body;
    assert!(body.contains("error line 1"));
    assert!(body.contains("error line 2"));
    assert!(body.contains("error line 3"));
    assert!(body.contains("stderr line 1"));
    assert!(body.contains("stderr line 2"));
}

#[test]
fn command_render_promoted_level_4() {
    let item = PresentationItem::CommandExecuted {
        status: CommandStatus::PromotedToTask,
        cmd_preview: "cargo test --all".into(),
        task_id: Some("task-123".into()),
        duration_ms: None,
        exit_code: None,
        stdout_summary: "running tests".into(),
        full_stdout: None,
        full_stderr: None,
        error_message: None,
    };
    let cells = item.render(4);
    assert_eq!(cells.len(), 1);
    assert!(cells[0].body.contains("running in background"));
    assert!(cells[0].body.contains("task-123"));
    assert!(!cells[0].body.contains("running tests"));
}

#[test]
fn file_change_render_level_4() {
    let item = PresentationItem::FileChange {
        path: "src/foo.rs".into(),
        action: FileAction::Modified,
        hunks: vec![HunkSummary {
            old_start: 12,
            old_count: 5,
            new_start: 12,
            new_count: 7,
            added_count: 2,
            removed_count: 1,
        }],
        full_diff: Some("@@ -12,5 +12,7 @@\n-old\n+new".into()),
    };
    let cells = item.render(4);
    assert_eq!(cells.len(), 1);
    assert!(cells[0].body.contains("M src/foo.rs"));
    assert!(cells[0].body.contains("(+2, -1)"));
    assert!(!cells[0].body.contains("@@"));
}

#[test]
fn file_change_render_level_5() {
    let item = PresentationItem::FileChange {
        path: "src/foo.rs".into(),
        action: FileAction::Modified,
        hunks: vec![HunkSummary {
            old_start: 12,
            old_count: 5,
            new_start: 12,
            new_count: 7,
            added_count: 2,
            removed_count: 1,
        }],
        full_diff: Some("@@ -12,5 +12,7 @@\n-old\n+new".into()),
    };
    let cells = item.render(5);
    assert_eq!(cells.len(), 1);
    assert!(cells[0].body.contains("M src/foo.rs"));
    assert!(cells[0].body.contains("@@"));
    assert!(cells[0].body.contains("-old"));
    assert!(cells[0].body.contains("+new"));
}

#[test]
fn render_suppresses_below_min_level() {
    let item = PresentationItem::ProviderRound {
        model: "gpt-4".into(),
        stop_reason: "end_turn".into(),
        tokens: TokenCount::default(),
    };
    assert!(item.render(4).is_empty());
}

// ── Reducer snapshot tests ──────────────────────────────────────────

use crate::operator_event::{present_operator_event, OperatorPresentationContext};
use crate::tui::projection::ProjectionEventRecord;
use chrono::Utc;
use serde_json::json;

fn make_event(kind: &str, summary: &str, payload: serde_json::Value) -> ProjectionEventRecord {
    let presentation = present_operator_event(
        kind,
        &payload,
        summary,
        &OperatorPresentationContext::default(),
    );
    ProjectionEventRecord {
        id: "evt-1".into(),
        event_seq: 1,
        ts: Utc::now(),
        lane: crate::tui::projection::ProjectionEventLane::Debug,
        kind: kind.into(),
        summary: presentation.summary.clone(),
        presentation,
        payload,
    }
}

#[test]
fn reducer_merges_command_start_and_finish() {
    let start = make_event(
        "process_execution_requested",
        "command started: cargo test",
        json!({"exec_command_cmd": "cargo test --lib", "cmd": "cargo test --lib"}),
    );
    let finish = make_event(
        "tool_executed",
        "tool executed: cargo test --lib",
        json!({
            "tool_name": "ExecCommand",
            "exec_command_cmd": "cargo test --lib",
            "exit_status": 0,
            "stdout_preview": "5 passed"
        }),
    );

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[start.clone(), finish.clone()],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    // Should produce exactly 1 CommandExecuted (merged)
    assert_eq!(items.len(), 1);
    match &items[0].item {
        PresentationItem::CommandExecuted {
            status,
            cmd_preview,
            exit_code,
            stdout_summary,
            ..
        } => {
            assert_eq!(*status, CommandStatus::Completed);
            assert!(cmd_preview.contains("cargo test"));
            assert_eq!(*exit_code, Some(0));
            assert_eq!(stdout_summary, "5 passed");
        }
        other => panic!("expected CommandExecuted, got {:?}", other),
    }
}

#[test]
fn reducer_merges_legacy_summary_command_start_and_finish() {
    let start = make_event(
        "process_execution_requested",
        "Command started: agentinbox inbox ack --agent-id holon-pm",
        json!({}),
    );
    let finish = make_event(
        "tool_executed",
        "Command finished: agentinbox inbox ack --agent-id holon-pm",
        json!({
            "tool_name": "ExecCommand",
            "exec_command_cmd": "agentinbox inbox ack --agent-id holon-pm",
            "exit_status": 0
        }),
    );

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[start, finish],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert_eq!(items.len(), 1);
    match &items[0].item {
        PresentationItem::CommandExecuted { cmd_preview, .. } => {
            assert_eq!(cmd_preview, "agentinbox inbox ack --agent-id holon-pm");
        }
        other => panic!("expected CommandExecuted, got {:?}", other),
    }
}

#[test]
fn reducer_merges_exec_command_batch_start_and_finish() {
    let batch_items = json!([
        {"index": 1, "cmd": "cargo fmt --all -- --check", "cmd_display": "cargo fmt --all -- --check"},
        {"index": 2, "cmd": "cargo test presentation --lib", "cmd_display": "cargo test presentation --lib"}
    ]);
    let start = make_event(
        "process_execution_requested",
        "Command batch started",
        json!({
            "surface": "ExecCommandBatch",
            "exec_command_batch_items": batch_items.clone()
        }),
    );
    let finish = make_event(
        "tool_executed",
        "Command batch finished",
        json!({
            "tool_name": "ExecCommandBatch",
            "exec_command_batch_items": batch_items,
            "duration_ms": 42,
            "exec_command_result": {
                "completed_count": 2,
                "item_count": 2,
                "summary_text": "ExecCommandBatch completed 2/2 items"
            }
        }),
    );

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[start, finish],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert_eq!(items.len(), 1);
    match &items[0].item {
        PresentationItem::CommandExecuted {
            status,
            cmd_preview,
            duration_ms,
            ..
        } => {
            assert_eq!(*status, CommandStatus::Completed);
            assert_eq!(
                cmd_preview,
                "cargo fmt --all -- --check\ncargo test presentation --lib"
            );
            assert_eq!(*duration_ms, Some(42));
        }
        other => panic!("expected CommandExecuted, got {:?}", other),
    }
}

#[test]
fn reducer_updates_command_item_when_events_are_not_adjacent() {
    let start = make_event(
        "process_execution_requested",
        "command started: cargo test",
        json!({
            "surface": "ExecCommand",
            "exec_command_cmd": "cargo test --lib",
            "cmd": "cargo test --lib"
        }),
    );
    let progress = make_event(
        "assistant_round_recorded",
        "checking command result",
        json!({"text_preview": "checking command result"}),
    );
    let finish = make_event(
        "tool_executed",
        "tool executed: cargo test --lib",
        json!({
            "tool_name": "ExecCommand",
            "exec_command_cmd": "cargo test --lib",
            "duration_ms": 42,
            "exit_status": 0,
            "stdout_preview": "5 passed"
        }),
    );

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[start.clone(), progress, finish.clone()],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    let command_items = items
        .iter()
        .filter(|timed| matches!(timed.item, PresentationItem::CommandExecuted { .. }))
        .collect::<Vec<_>>();
    assert_eq!(command_items.len(), 1);
    assert_eq!(
        command_items[0].dedupe_key,
        "command::cargo test --lib".to_string()
    );
    match &command_items[0].item {
        PresentationItem::CommandExecuted {
            status,
            duration_ms,
            exit_code,
            stdout_summary,
            ..
        } => {
            assert_eq!(*status, CommandStatus::Completed);
            assert_eq!(*duration_ms, Some(42));
            assert_eq!(*exit_code, Some(0));
            assert_eq!(stdout_summary, "5 passed");
        }
        other => panic!("expected CommandExecuted, got {:?}", other),
    }
}

#[test]
fn reducer_updates_command_item_across_incremental_calls() {
    let start = make_event(
        "process_execution_requested",
        "command started: cargo test",
        json!({
            "surface": "ExecCommand",
            "cmd_display": "cargo test --lib",
            "cmd_preview": "cargo test --lib"
        }),
    );
    let finish = make_event(
        "tool_executed",
        "tool executed: cargo test --lib",
        json!({
            "tool_name": "ExecCommand",
            "exec_command_result": {
                "cmd_display": "cargo test --lib",
                "duration_ms": 42,
                "exit_status": 0,
                "stdout_preview": "5 passed"
            }
        }),
    );

    let mut reducer = PresentationReducer::new();
    let started = reducer.reduce(
        &[start],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );
    assert!(started.is_empty());

    let finished = reducer.reduce(
        &[finish],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );
    assert_eq!(finished.len(), 1);
    assert_eq!(
        finished[0].dedupe_key,
        "command::cargo test --lib".to_string()
    );
    match &finished[0].item {
        PresentationItem::CommandExecuted {
            status,
            duration_ms,
            exit_code,
            stdout_summary,
            ..
        } => {
            assert_eq!(*status, CommandStatus::Completed);
            assert_eq!(*duration_ms, Some(42));
            assert_eq!(*exit_code, Some(0));
            assert_eq!(stdout_summary, "5 passed");
        }
        other => panic!("expected CommandExecuted, got {:?}", other),
    }
}

#[test]
fn reducer_standalone_tool_executed_becomes_command() {
    let event = make_event(
        "tool_executed",
        "tool executed: rg pattern",
        json!({
            "tool_name": "ExecCommand",
            "exec_command_cmd": "rg pattern src/",
            "exit_status": 0
        }),
    );

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[event],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert_eq!(items.len(), 1);
    match &items[0].item {
        PresentationItem::CommandExecuted { cmd_preview, .. } => {
            assert!(cmd_preview.contains("rg pattern"));
        }
        other => panic!("expected CommandExecuted, got {:?}", other),
    }
}

#[test]
fn reducer_suppresses_successful_work_item_tool_bookkeeping() {
    let event = make_event(
        "tool_executed",
        "Updated work item: UpdateWorkItem",
        json!({
            "tool_name": "UpdateWorkItem",
            "status": "success",
            "summary": "updated work item"
        }),
    );

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[event],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert!(items.is_empty());
}

#[test]
fn reducer_suppresses_successful_pick_work_item_tool_bookkeeping() {
    let event = make_event(
        "tool_executed",
        "Picked work item: PickWorkItem",
        json!({
            "tool_name": "PickWorkItem",
            "status": "success",
            "summary": "picked work item"
        }),
    );

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[event],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert!(items.is_empty());
}

#[test]
fn reducer_keeps_error_status_work_item_tool_bookkeeping() {
    let event = make_event(
        "tool_executed",
        "UpdateWorkItem error validation_failed: missing work item",
        json!({
            "tool_name": "UpdateWorkItem",
            "status": "error",
            "error": "missing work item"
        }),
    );

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[event],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert_eq!(items.len(), 1);
    match &items[0].item {
        PresentationItem::ToolAction { .. } => {}
        other => panic!("expected ToolAction, got {:?}", other),
    }
}

#[test]
fn reducer_keeps_failed_work_item_tool_bookkeeping() {
    let event = make_event(
        "tool_execution_failed",
        "Update work item failed: missing work item",
        json!({
            "tool_name": "UpdateWorkItem",
            "error": "missing work item"
        }),
    );

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[event],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert_eq!(items.len(), 1);
    match &items[0].item {
        PresentationItem::ToolAction { summary } => {
            assert!(summary.contains("Update work item failed"));
        }
        other => panic!("expected ToolAction, got {:?}", other),
    }
}

#[test]
fn reducer_list_work_items_result_becomes_snapshot() {
    let event = make_event(
        "tool_executed",
        "Listed work items: ListWorkItems",
        json!({
            "tool_name": "ListWorkItems",
            "status": "success",
            "summary": "ListWorkItems",
            "tool_result": {
                "context": {"current_work_item_id": "work_e8a1686c34d1653"},
                "filter": "blocked",
                "returned": 1,
                "total_matching": 1,
                "limit": 10,
                "work_items": [{
                    "id": "work_e8a1686c34d1653",
                    "objective": "Track PR #1552 for issue #1537 TUI composer Up/Down cursor movement through CI/review/merge",
                    "focus": "blocked",
                    "readiness": "blocked",
                    "blocked_by": "PR #1552 CI is green and Copilot review had no comments; wait for human review, review feedback, merge, or close event before the next action.",
                    "todo_list": [{
                        "state": "in_progress",
                        "text": "Track PR #1552 through human review/merge"
                    }]
                }]
            }
        }),
    );

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[event],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert_eq!(items.len(), 1);
    match &items[0].item {
        PresentationItem::ToolAction { summary } => {
            assert!(summary.contains("Work items: filter=blocked returned=1 total=1 limit=10"));
            assert!(summary.contains("[blocked][blocked] work_e8a1686c34"));
            assert!(summary.contains("Track PR #1552"));
            assert!(summary.contains("blocked_by: PR #1552 CI is green"));
            assert!(!summary.contains("Listed work items: ListWorkItems"));
        }
        other => panic!("expected ToolAction snapshot, got {:?}", other),
    }
}

#[test]
fn reducer_list_work_items_empty_result_is_informative() {
    let event = make_event(
        "tool_executed",
        "Listed work items: ListWorkItems",
        json!({
            "tool_name": "ListWorkItems",
            "status": "success",
            "tool_result": {
                "context": {"current_work_item_id": "work_f83896b472f10e1"},
                "filter": "runnable",
                "returned": 0,
                "total_matching": 0,
                "limit": 10,
                "work_items": []
            }
        }),
    );

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[event],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert_eq!(items.len(), 1);
    match &items[0].item {
        PresentationItem::ToolAction { summary } => {
            assert!(summary.contains("Work items: filter=runnable returned=0 total=0 limit=10"));
            assert!(summary.contains("No work items matched filter=runnable."));
            assert!(summary.contains("current: work_f83896b472"));
        }
        other => panic!("expected ToolAction snapshot, got {:?}", other),
    }
}

#[test]
fn reducer_apply_patch_result_becomes_file_changes() {
    let event = make_event(
        "tool_executed",
        "Applied patch: patched M:src/lib.rs, A:README.md",
        json!({
            "tool_name": "ApplyPatch",
            "apply_patch_result": {
                "changed_files": [
                    {"action": "modify", "path": "src/lib.rs"},
                    {"action": "add", "path": "README.md"}
                ],
                "changed_paths": ["src/lib.rs", "README.md"],
                "changed_file_count": 2
            }
        }),
    );

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[event],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert_eq!(items.len(), 2);
    match &items[0].item {
        PresentationItem::FileChange { action, path, .. } => {
            assert_eq!(*action, FileAction::Modified);
            assert_eq!(path, "src/lib.rs");
            assert!(items[0].item.render(4)[0].body.contains("M src/lib.rs"));
        }
        other => panic!("expected FileChange, got {:?}", other),
    }
    match &items[1].item {
        PresentationItem::FileChange { action, path, .. } => {
            assert_eq!(*action, FileAction::Added);
            assert_eq!(path, "README.md");
            assert!(items[1].item.render(4)[0].body.contains("A README.md"));
        }
        other => panic!("expected FileChange, got {:?}", other),
    }
}

#[test]
fn reducer_apply_patch_summary_fallback_becomes_file_change() {
    let event = make_event(
        "tool_executed",
        "Applied patch: patched M:src/presentation.rs",
        json!({
            "tool_name": "ApplyPatch",
            "summary": "patched M:src/presentation.rs"
        }),
    );

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[event],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert_eq!(items.len(), 1);
    match &items[0].item {
        PresentationItem::FileChange { action, path, .. } => {
            assert_eq!(*action, FileAction::Modified);
            assert_eq!(path, "src/presentation.rs");
        }
        other => panic!("expected FileChange, got {:?}", other),
    }
}

#[test]
fn reducer_apply_patch_failure_is_not_command() {
    let event = make_event(
        "tool_execution_failed",
        "Patch failed: context not found",
        json!({
            "tool_name": "ApplyPatch",
            "error": "context not found\nexpected lines: old text"
        }),
    );

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[event],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert_eq!(items.len(), 1);
    match &items[0].item {
        PresentationItem::PatchFailure { error_message, .. } => {
            assert_eq!(
                error_message.as_deref(),
                Some("context not found\nexpected lines: old text")
            );
            assert!(items[0].item.render(4)[0].body.contains("Patch failed"));
            assert!(!items[0].item.render(4)[0].body.contains("expected lines"));
            assert!(items[0].item.render(5)[0].body.contains("expected lines"));
        }
        other => panic!("expected PatchFailure, got {:?}", other),
    }
}

#[test]
fn reducer_apply_patch_failure_prefers_structured_message_over_raw_json() {
    let event = make_event(
        "tool_execution_failed",
        "Patch failed: {",
        json!({
            "tool_name": "ApplyPatch",
            "error": "{\n  \"kind\": \"invalid_tool_input\",\n  \"message\": \"input for ApplyPatch does not match the advertised tool schema\",\n  \"retryable\": false\n}",
            "tool_error": {
                "kind": "invalid_tool_input",
                "message": "input for ApplyPatch does not match the advertised tool schema",
                "retryable": false
            }
        }),
    );

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[event],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert_eq!(items.len(), 1);
    match &items[0].item {
        PresentationItem::PatchFailure { summary, .. } => {
            assert_eq!(
                summary,
                "Patch failed: input for ApplyPatch does not match the advertised tool schema"
            );
            let rendered = items[0].item.render(4)[0].body.clone();
            assert!(rendered.contains("ApplyPatch does not match"));
            assert!(!rendered.contains("Patch failed: {"));
        }
        other => panic!("expected PatchFailure, got {:?}", other),
    }
}

#[test]
fn reducer_renders_work_item_lifecycle_cards_with_objectives() {
    let created = make_event(
        "work_item_written",
        "work item created",
        json!({
            "action": "created",
            "record": {
                "id": "wi-1",
                "agent_id": "default",
                "workspace_id": "agent_home",
                "objective": "ship lifecycle display",
                "state": "open",
                "plan_status": "draft",
                "created_at": "2026-01-01T00:00:00Z",
                "updated_at": "2026-01-01T00:00:00Z"
            }
        }),
    );
    let completed = make_event(
        "work_item_written",
        "work item completed",
        json!({
            "action": "completed",
            "record": {
                "id": "wi-1",
                "agent_id": "default",
                "workspace_id": "agent_home",
                "objective": "ship lifecycle display",
                "state": "completed",
                "plan_status": "ready",
                "result_summary": "merged PR",
                "created_at": "2026-01-01T00:00:00Z",
                "updated_at": "2026-01-01T00:00:01Z"
            }
        }),
    );

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[created, completed],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert_eq!(items.len(), 2);
    match &items[0].item {
        PresentationItem::WorkItemCard { action, summary } => {
            assert_eq!(action, "tracking started");
            assert_eq!(summary, "ship lifecycle display");
            assert_eq!(items[0].item.min_display_level(), 3);
            assert!(!items[0].item.render(3)[0]
                .body
                .contains("work_item_written"));
        }
        other => panic!("expected WorkItemCard, got {:?}", other),
    }
    match &items[1].item {
        PresentationItem::WorkItemCard { action, summary } => {
            assert_eq!(action, "completed");
            assert_eq!(summary, "merged PR");
            assert_eq!(items[1].item.min_display_level(), 3);
        }
        other => panic!("expected WorkItemCard, got {:?}", other),
    }
}

#[test]
fn reducer_renders_lightweight_work_item_lifecycle_cards() {
    let created = make_event(
        "work_item_written",
        "Work item Open: ship lightweight lifecycle",
        json!({
            "agent_id": "default",
            "work_item_id": "wi-1",
            "workspace_id": "agent_home",
            "revision": 1,
            "action": "created",
            "state": "open",
            "plan_status": "draft",
            "readiness": "runnable",
            "updated_at": "2026-01-01T00:00:00Z",
            "objective_preview": "ship lightweight lifecycle",
            "objective_len": 26
        }),
    );
    let completed = make_event(
        "work_item_written",
        "Work completed: merged lightweight PR",
        json!({
            "agent_id": "default",
            "work_item_id": "wi-1",
            "workspace_id": "agent_home",
            "revision": 2,
            "action": "completed",
            "state": "completed",
            "plan_status": "ready",
            "readiness": "completed",
            "updated_at": "2026-01-01T00:00:01Z",
            "objective_preview": "ship lightweight lifecycle",
            "objective_len": 26,
            "result_summary_preview": "merged lightweight PR"
        }),
    );

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[created, completed],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert_eq!(items.len(), 2);
    match &items[0].item {
        PresentationItem::WorkItemCard { action, summary } => {
            assert_eq!(action, "tracking started");
            assert_eq!(summary, "ship lightweight lifecycle");
        }
        other => panic!("expected WorkItemCard, got {:?}", other),
    }
    match &items[1].item {
        PresentationItem::WorkItemCard { action, summary } => {
            assert_eq!(action, "completed");
            assert_eq!(summary, "merged lightweight PR");
        }
        other => panic!("expected WorkItemCard, got {:?}", other),
    }
}

#[test]
fn reducer_renders_work_item_bookkeeping_at_verbose_level() {
    let updated = make_event(
        "work_item_written",
        "work item updated",
        json!({
            "action": "updated",
            "record": {
                "id": "wi-1",
                "agent_id": "default",
                "workspace_id": "agent_home",
                "objective": "ship lifecycle display",
                "state": "open",
                "plan_status": "draft",
                "created_at": "2026-01-01T00:00:00Z",
                "updated_at": "2026-01-01T00:00:01Z"
            }
        }),
    );
    let picked = make_event(
        "work_item_picked",
        "work item picked",
        json!({
            "current_work_item_id": "wi-1",
            "current_readiness": "runnable"
        }),
    );

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[updated, picked],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert_eq!(items.len(), 2);
    for item in items {
        match &item.item {
            PresentationItem::WorkItemBookkeeping { summary, .. } => {
                assert_eq!(item.item.min_display_level(), 4);
                assert!(summary.contains("ship lifecycle display"));
                assert!(item.item.render(3).is_empty());
                assert!(!item.item.render(4)[0].body.contains("work_item_"));
            }
            other => panic!("expected WorkItemBookkeeping, got {:?}", other),
        }
    }
}

#[test]
fn reducer_suppresses_duplicate_update_work_item_tool_event() {
    let updated = make_event(
        "work_item_written",
        "work item updated",
        json!({
            "action": "updated",
            "record": {
                "id": "wi-1",
                "agent_id": "default",
                "workspace_id": "agent_home",
                "objective": "deduplicate work item update display",
                "state": "open",
                "plan_status": "ready",
                "created_at": "2026-01-01T00:00:00Z",
                "updated_at": "2026-01-01T00:00:01Z"
            }
        }),
    );
    let tool_event = make_event(
        "tool_executed",
        "Updated work item: UpdateWorkItem",
        json!({
            "tool_name": "UpdateWorkItem",
            "status": "success",
            "summary": "updated work item"
        }),
    );

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[updated, tool_event],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert_eq!(items.len(), 1);
    match &items[0].item {
        PresentationItem::WorkItemBookkeeping {
            transition,
            summary,
            ..
        } => {
            assert_eq!(*transition, WorkItemTransition::Updated);
            assert_eq!(summary, "deduplicate work item update display");
        }
        other => panic!("expected WorkItemBookkeeping, got {:?}", other),
    }
}

#[test]
fn reducer_standalone_process_execution_requested_is_not_a_command_item() {
    let event = make_event(
        "process_execution_requested",
        "command started: cargo test",
        json!({"cmd_preview": "cargo test --all"}),
    );

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[event],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert!(items.is_empty());
}

#[test]
fn reducer_prefers_command_display_for_script_commands() {
    let event = make_event(
        "tool_executed",
        "command finished",
        json!({
            "tool_name": "ExecCommand",
            "cmd_preview": "[omitted: command contains heredoc or inline script]",
            "cmd_display": "python - <<'PY'\nprint('hello')",
            "exit_status": 0
        }),
    );

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[event],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert_eq!(items.len(), 1);
    match &items[0].item {
        PresentationItem::CommandExecuted { cmd_preview, .. } => {
            assert_eq!(cmd_preview, "python - <<'PY'\nprint('hello')");
            assert!(items[0].item.render(4)[0].body.contains("print('hello')"));
        }
        other => panic!("expected CommandExecuted, got {:?}", other),
    }
}

#[test]
fn reducer_promoted_exec_command_uses_bounded_outcome() {
    let event = make_event(
        "tool_executed",
        "tool executed: ExecCommand",
        json!({
            "tool_name": "ExecCommand",
            "exec_command_cmd": "cargo test --all",
            "exec_command_result": {
                "disposition": "promoted_to_task",
                "task_handle": {"task_id": "task-123"},
                "initial_output_preview": "Compiling holon",
                "initial_output_truncated": false
            }
        }),
    );

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[event],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert_eq!(items.len(), 1);
    match &items[0].item {
        PresentationItem::CommandExecuted {
            status,
            task_id,
            stdout_summary,
            ..
        } => {
            assert_eq!(*status, CommandStatus::PromotedToTask);
            assert_eq!(task_id.as_deref(), Some("task-123"));
            assert_eq!(stdout_summary, "Compiling holon");
            assert!(items[0].item.render(4)[0]
                .body
                .contains("running in background"));
        }
        other => panic!("expected CommandExecuted, got {:?}", other),
    }
}

#[test]
fn reducer_produces_assistant_progress() {
    let event = make_event(
        "assistant_round_recorded",
        "assistant round",
        json!({"text_preview": "Let me analyze the code..."}),
    );

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[event],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert_eq!(items.len(), 1);
    match &items[0].item {
        PresentationItem::AssistantProgress { text, .. } => {
            assert_eq!(text, "Let me analyze the code...");
        }
        other => panic!("expected AssistantProgress, got {:?}", other),
    }
}

#[test]
fn reducer_provider_round_at_level_5() {
    let event = make_event(
        "provider_round_completed",
        "provider round",
        json!({
            "active_model": "deepseek-v4",
            "stop_reason": "end_turn",
            "input_tokens": 5000,
            "output_tokens": 200
        }),
    );

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[event],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert_eq!(items.len(), 1);
    let item = &items[0].item;
    assert_eq!(item.min_display_level(), 5);
    match item {
        PresentationItem::ProviderRound { model, tokens, .. } => {
            assert_eq!(model, "deepseek-v4");
            assert_eq!(tokens.input, 5000);
            assert_eq!(tokens.output, 200);
        }
        other => panic!("expected ProviderRound, got {:?}", other),
    }
}

#[test]
fn reducer_renders_external_resume_reason_at_verbose_level() {
    let event = make_event(
        "callback_delivered",
        "callback_delivered",
        json!({
            "source": "github",
            "resource": "pull/42",
            "disposition": "Triggered"
        }),
    );

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[event],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert_eq!(items.len(), 1);
    match &items[0].item {
        PresentationItem::ResumeNotice { reason, details } => {
            assert_eq!(
                reason,
                "External event from github for pull/42; resuming agent"
            );
            assert_eq!(items[0].item.min_display_level(), 4);
            assert!(items[0].item.render(3).is_empty());
            assert!(items[0].item.render(4)[0].body.contains("resuming agent"));
            assert!(items[0].item.render(5)[0]
                .body
                .contains("External event received"));
            assert!(details
                .as_deref()
                .is_some_and(|detail| detail.contains("github")));
        }
        other => panic!("expected ResumeNotice, got {:?}", other),
    }
}

#[test]
fn reducer_keeps_non_resuming_callback_as_debug_detail() {
    let event = make_event(
        "callback_delivered",
        "callback_delivered",
        json!({
            "source": "github",
            "resource": "pull/42",
            "disposition": "Coalesced"
        }),
    );

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[event],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert_eq!(items.len(), 1);
    match &items[0].item {
        PresentationItem::GenericEvent { kind, summary } => {
            assert_eq!(kind, "callback_delivered");
            assert!(summary.contains("External event received"));
            assert_eq!(items[0].item.min_display_level(), 5);
        }
        other => panic!("expected GenericEvent fallback, got {:?}", other),
    }
}

#[test]
fn reducer_brief_created_uses_brief_text_without_event_label() {
    let brief = BriefRecord::new(
        "default",
        BriefKind::Result,
        "completed the task",
        None,
        None,
    );
    let event = make_event("brief_created", "Brief: completed the task", json!(brief));

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[event],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert_eq!(items.len(), 1);
    match &items[0].item {
        PresentationItem::AssistantResult { body, outcome, .. } => {
            assert_eq!(body, "completed the task");
            assert_eq!(*outcome, Outcome::Success);
            assert!(!body.contains("Brief:"));
        }
        other => panic!("expected AssistantResult, got {:?}", other),
    }
}

#[test]
fn reducer_brief_created_resolves_transcript_backed_text() {
    let mut brief = BriefRecord::new("default", BriefKind::Result, "preview only", None, None);
    brief.content_source = crate::types::BriefContentSource::TranscriptEntry {
        entry_id: "transcript-entry-1".into(),
        relation: crate::types::BriefContentSourceRelation::DerivedFrom,
    };
    let event = make_event("brief_created", "Brief: preview only", json!(brief));
    let brief_texts = std::collections::BTreeMap::from([(
        "transcript-entry-1".to_string(),
        "resolved presentation body sentinel_2107".to_string(),
    )]);

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(&[event], &BriefTextLookup(&brief_texts));

    assert_eq!(items.len(), 1);
    match &items[0].item {
        PresentationItem::AssistantResult { body, outcome, .. } => {
            assert_eq!(body, "resolved presentation body sentinel_2107");
            assert_eq!(*outcome, Outcome::Success);
        }
        other => panic!("expected AssistantResult, got {:?}", other),
    }
}

#[test]
fn reducer_brief_created_uses_lightweight_summary() {
    let event = make_event(
        "brief_created",
        "Brief created",
        json!({
            "brief_id": "brief-1",
            "agent_id": "default",
            "workspace_id": "agent_home",
            "kind": "result",
            "created_at": "2026-01-01T00:00:00Z",
            "content_char_count": 22
        }),
    );

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[event],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert_eq!(items.len(), 1);
    match &items[0].item {
        PresentationItem::AssistantResult {
            brief_id,
            body,
            outcome,
        } => {
            assert_eq!(brief_id.as_deref(), Some("brief-1"));
            assert_eq!(body, "Result brief (22 chars)");
            assert_eq!(*outcome, Outcome::Success);
        }
        other => panic!("expected AssistantResult, got {:?}", other),
    }
}

#[test]
fn reducer_brief_created_resolves_inline_audit_text() {
    let event = make_event(
        "brief_created",
        "Brief created",
        json!({
            "brief_id": "brief-inline-1",
            "agent_id": "default",
            "workspace_id": "agent_home",
            "kind": "result",
            "created_at": "2026-01-01T00:00:00Z",
            "content_source": { "kind": "inline" },
            "content_char_count": 31
        }),
    );
    let brief_texts = std::collections::BTreeMap::from([(
        "brief-inline-1".to_string(),
        "resolved inline body sentinel_8429".to_string(),
    )]);

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(&[event], &BriefTextLookup(&brief_texts));

    assert_eq!(items.len(), 1);
    match &items[0].item {
        PresentationItem::AssistantResult { body, outcome, .. } => {
            assert_eq!(body, "resolved inline body sentinel_8429");
            assert_eq!(*outcome, Outcome::Success);
        }
        other => panic!("expected AssistantResult, got {:?}", other),
    }
}

#[test]
fn reducer_deduplicates_repeated_brief_created_by_brief_id() {
    let brief = BriefRecord::new(
        "default",
        BriefKind::Result,
        "completed the task",
        None,
        None,
    );
    let first = make_event("brief_created", "Brief: completed the task", json!(brief));
    let mut second = first.clone();
    second.id = "evt-2".into();
    second.event_seq = 2;

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[first, second],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert_eq!(items.len(), 1);
    match &items[0].item {
        PresentationItem::AssistantResult { brief_id, body, .. } => {
            assert!(brief_id.is_some());
            assert_eq!(body, "completed the task");
        }
        other => panic!("expected AssistantResult, got {:?}", other),
    }
}

#[test]
fn reducer_deduplicates_repeated_operator_message_by_message_id() {
    let mut message = MessageEnvelope::new(
        "default",
        crate::types::MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        crate::types::AuthorityClass::OperatorInstruction,
        crate::types::Priority::Normal,
        MessageBody::Text {
            text: "same operator input".into(),
        },
    );
    message.id = "message-1".into();
    let first = make_event("message_enqueued", "message enqueued", json!(message));
    let mut second = first.clone();
    second.id = "evt-2".into();
    second.event_seq = 2;

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[first, second],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert_eq!(items.len(), 1);
    match &items[0].item {
        PresentationItem::UserMessage { text } => {
            assert_eq!(text, "same operator input");
        }
        other => panic!("expected UserMessage, got {:?}", other),
    }
}

#[test]
fn reducer_filters_legacy_ack_briefs_from_assistant_results() {
    let message = crate::types::MessageEnvelope::new(
        "default",
        crate::types::MessageKind::OperatorPrompt,
        crate::types::MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        crate::types::AuthorityClass::OperatorInstruction,
        crate::types::Priority::Normal,
        crate::types::MessageBody::Text {
            text: "duplicate operator input".into(),
        },
    );
    let brief = holon_domain::make_ack("default", &message);
    assert!(brief.text.starts_with(holon_domain::QUEUED_WORK_ACK_PREFIX));
    let event = make_event("brief_created", "Queued work: duplicate", json!(brief));

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[event],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert!(items.is_empty());
}

#[test]
fn reducer_deduplicates_assistant_observations_against_final_brief() {
    let assistant = make_event(
        "assistant_round_recorded",
        "assistant round",
        json!({
            "agent_id": "default",
            "text_preview": "Issue recorded: #1128..."
        }),
    );
    let text_only = make_event(
        "text_only_round_observed",
        "text only round",
        json!({
            "agent_id": "default",
            "text_preview": "Issue recorded: #1128..."
        }),
    );
    let brief = BriefRecord::new(
        "default",
        BriefKind::Result,
        "Issue recorded: #1128 with complete details",
        None,
        None,
    );
    let brief_event = make_event(
        "brief_created",
        "Issue recorded: #1128 with complete details",
        json!(brief),
    );

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[assistant, text_only, brief_event],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert_eq!(items.len(), 1);
    match &items[0].item {
        PresentationItem::AssistantResult { body, .. } => {
            assert_eq!(body, "Issue recorded: #1128 with complete details");
        }
        other => panic!("expected AssistantResult, got {:?}", other),
    }
}

#[test]
fn reducer_deduplicates_repeated_assistant_text_observations() {
    let assistant = make_event(
        "assistant_round_recorded",
        "assistant round",
        json!({
            "agent_id": "default",
            "text_preview": "Analyzing the issue"
        }),
    );
    let text_only = make_event(
        "text_only_round_observed",
        "text only round",
        json!({
            "agent_id": "default",
            "text_preview": "Analyzing   the issue"
        }),
    );

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[assistant, text_only],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert_eq!(items.len(), 1);
    match &items[0].item {
        PresentationItem::AssistantProgress { text, .. } => {
            assert_eq!(text, "Analyzing the issue");
        }
        other => panic!("expected AssistantProgress, got {:?}", other),
    }
}

#[test]
fn reducer_hides_runtime_checkpoint_assistant_rounds() {
    let assistant = make_event(
        "assistant_round_recorded",
        "assistant round",
        json!({
            "agent_id": "default",
            "round_purpose": "runtime_checkpoint",
            "text_preview": "internal checkpoint text"
        }),
    );

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[assistant],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert!(items.is_empty());
}

#[test]
fn reducer_deduplicates_same_round_text_despite_agent_id_mismatch() {
    let assistant = make_event(
        "assistant_round_recorded",
        "assistant round",
        json!({
            "agent_id": "default",
            "turn_index": 12,
            "round": 3,
            "text_preview": "Rendering once"
        }),
    );
    let text_only = make_event(
        "text_only_round_observed",
        "text only round",
        json!({
            "turn_index": 12,
            "round": 3,
            "text_preview": "Rendering once"
        }),
    );

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[assistant, text_only],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert_eq!(items.len(), 1);
    match &items[0].item {
        PresentationItem::AssistantProgress { text, .. } => {
            assert_eq!(text, "Rendering once");
        }
        other => panic!("expected AssistantProgress, got {:?}", other),
    }
}

#[test]
fn reducer_deduplicates_same_round_text_across_incremental_reductions() {
    let assistant = make_event(
        "assistant_round_recorded",
        "assistant round",
        json!({
            "agent_id": "default",
            "turn_index": 12,
            "round": 3,
            "text_preview": "Rendering once"
        }),
    );
    let text_only = make_event(
        "text_only_round_observed",
        "text only round",
        json!({
            "agent_id": "default",
            "turn_index": 12,
            "round": 3,
            "text_preview": "Rendering once"
        }),
    );

    let mut reducer = PresentationReducer::new();
    let first = reducer.reduce(
        &[assistant],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );
    let second = reducer.reduce(
        &[text_only],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert_eq!(first.len(), 1);
    assert!(second.is_empty());
}

#[test]
fn reducer_deduplicates_later_brief_against_observed_assistant_text() {
    let assistant = make_event(
        "assistant_round_recorded",
        "assistant round",
        json!({
            "agent_id": "default",
            "text_preview": "Issue recorded: #1128"
        }),
    );
    let brief = BriefRecord::new(
        "default",
        BriefKind::Result,
        "Issue recorded: #1128",
        None,
        None,
    );
    let brief_event = make_event("brief_created", "Issue recorded: #1128", json!(brief));

    let mut reducer = PresentationReducer::new();
    let first = reducer.reduce(
        &[assistant],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );
    let second = reducer.reduce(
        &[brief_event],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert_eq!(first.len(), 1);
    assert!(matches!(
        first[0].item,
        PresentationItem::AssistantProgress { .. }
    ));
    assert!(second.is_empty());
}

#[test]
fn reducer_keeps_later_brief_when_it_extends_observed_assistant_text() {
    let assistant = make_event(
        "assistant_round_recorded",
        "assistant round",
        json!({
            "agent_id": "default",
            "text_preview": "Issue recorded: #1128..."
        }),
    );
    let brief = BriefRecord::new(
        "default",
        BriefKind::Result,
        "Issue recorded: #1128 with complete details",
        None,
        None,
    );
    let brief_event = make_event(
        "brief_created",
        "Issue recorded: #1128 with complete details",
        json!(brief),
    );

    let mut reducer = PresentationReducer::new();
    let first = reducer.reduce(
        &[assistant],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );
    let second = reducer.reduce(
        &[brief_event],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert_eq!(first.len(), 1);
    assert_eq!(second.len(), 1);
    match &second[0].item {
        PresentationItem::AssistantResult { body, .. } => {
            assert_eq!(body, "Issue recorded: #1128 with complete details");
        }
        other => panic!("expected AssistantResult, got {:?}", other),
    }
}

#[test]
fn reducer_downgrades_sleep_tool_to_debug_internal_transition() {
    let event = make_event(
        "tool_executed",
        "Slept: sleep requested",
        json!({
            "tool_name": "Sleep"
        }),
    );

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[event],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert_eq!(items.len(), 1);
    match &items[0].item {
        PresentationItem::InternalTransition { what, .. } => {
            assert_eq!(what, "Sleep");
            assert_eq!(items[0].item.min_display_level(), 5);
            assert!(items[0].item.render(4).is_empty());
            assert!(!items[0].item.render(5).is_empty());
        }
        other => panic!("expected InternalTransition, got {:?}", other),
    }
}

#[test]
fn reducer_failure_brief_preserves_failure_outcome() {
    let brief = BriefRecord::new(
        "default",
        BriefKind::Failure,
        "provider transport failed",
        None,
        None,
    );
    let event = make_event(
        "brief_created",
        "Brief: provider transport failed",
        json!(brief),
    );

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[event],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert_eq!(items.len(), 1);
    match &items[0].item {
        PresentationItem::AssistantResult { body, outcome, .. } => {
            assert_eq!(body, "provider transport failed");
            assert_eq!(*outcome, Outcome::Failure);
        }
        other => panic!("expected AssistantResult, got {:?}", other),
    }
}

#[test]
fn reducer_suppresses_known_runtime_noise_but_keeps_unknown_debug_fallback() {
    let scheduler = make_event(
        "scheduler_decision",
        "Scheduler decision: sleep",
        json!({"decision": "sleep"}),
    );
    let unknown = make_event("unknown_runtime_event", "unknown runtime detail", json!({}));

    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(
        &[scheduler, unknown],
        &BriefTextLookup(&std::collections::BTreeMap::new()),
    );

    assert_eq!(items.len(), 1);
    match &items[0].item {
        PresentationItem::GenericEvent { kind, summary } => {
            assert_eq!(kind, "unknown_runtime_event");
            assert_eq!(summary, "unknown runtime detail");
            assert_eq!(items[0].item.min_display_level(), 5);
        }
        other => panic!("expected GenericEvent fallback, got {:?}", other),
    }
}

#[test]
fn reducer_empty_events_returns_empty() {
    let mut reducer = PresentationReducer::new();
    let items = reducer.reduce(&[], &BriefTextLookup(&std::collections::BTreeMap::new()));
    assert!(items.is_empty());

    let flushed = reducer.flush();
    assert!(flushed.is_empty());
}
