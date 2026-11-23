use super::*;
use holon_domain::{TaskKind, TaskStatus};
use serde_json::json;

#[test]
fn active_task_text_uses_redacted_command_and_bounds_inline_fields() {
    let now = chrono::Utc::now();
    let task = TaskRecord {
        id: "task-active".into(),
        agent_id: "agent".into(),
        kind: TaskKind::CommandTask,
        status: TaskStatus::Running,
        created_at: now,
        updated_at: now,
        parent_message_id: None,
        work_item_id: Some("work-current\nspoofed: true".into()),
        summary: Some("Run verification\ninjected: true".into()),
        detail: Some(json!({
            "cmd": "PRIVATE_COMMAND",
            "cmd_digest": "digest\nspoofed: true",
            "workdir": "/workspace\nspoofed: true",
            "output_path": "/tmp/output\nspoofed: true",
        })),
        recovery: None,
    };
    let mut terminal = task.clone();
    terminal.id = "task-terminal".into();
    terminal.status = TaskStatus::Completed;
    let text = render_active_tasks(&[task, terminal], 3, |cmd| {
        assert_eq!(cmd, "PRIVATE_COMMAND");
        "redacted\npreview".into()
    });
    assert!(text.contains("summary: Run verification injected: true"));
    assert!(text.contains("associated_work_item: work-current spoofed: true"));
    assert!(text.contains("cmd_preview: redacted preview"));
    assert!(text.contains("cmd_digest: digest spoofed: true"));
    assert!(text.contains("workdir: /workspace spoofed: true"));
    assert!(text.contains("output_path: /tmp/output spoofed: true"));
    assert!(text.contains("... 1 more active tasks not shown"));
    assert!(!text.contains("task-terminal"));
    assert!(!text.contains("PRIVATE_COMMAND"));
    assert!(!text.contains("\ninjected: true"));
    assert!(!text.contains("\nspoofed: true"));
}
