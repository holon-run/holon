//! Prompt text for the bounded active Task window.
use super::sanitize_inline;
use holon_domain::{
    is_active_task_status, ChildSupervisionProjection, CommandTaskStatusSnapshot, TaskRecord,
};
use holon_system::utf8::truncate_text;

pub const ACTIVE_TASKS_CONTEXT_LIMIT: usize = 5;
const ACTIVE_TASK_SUMMARY_CHAR_BUDGET: usize = 240;
const ACTIVE_TASK_CMD_PREVIEW_CHAR_BUDGET: usize = 240;

pub fn render_active_tasks(
    tasks: &[TaskRecord],
    total_count: usize,
    redacted_command_preview: fn(&str) -> String,
) -> String {
    let mut lines = vec![format!(
        "Active managed tasks (bounded to {ACTIVE_TASKS_CONTEXT_LIMIT}; use ListTasks for the full list):"
    )];

    for task in tasks
        .iter()
        .filter(|task| is_active_task_status(&task.status))
    {
        let task_id = sanitize_inline(&task.id);
        lines.push(format!("- task_id: {task_id}"));
        lines.push(format!("  task_ref: task:{task_id}"));
        lines.push(format!("  kind: {}", sanitize_inline(task.kind.as_str())));
        if let Some(summary) = task
            .summary
            .as_deref()
            .filter(|summary| !summary.trim().is_empty())
        {
            lines.push(format!(
                "  summary: {}",
                sanitize_inline(&truncate_text(summary, ACTIVE_TASK_SUMMARY_CHAR_BUDGET))
            ));
        }
        if let Some(work_item_id) = task.effective_work_item_id() {
            lines.push(format!(
                "  associated_work_item: {}",
                sanitize_inline(work_item_id)
            ));
        }

        if let Some(command) = CommandTaskStatusSnapshot::from_task_record(task) {
            lines.push("  command:".to_string());
            render_active_task_command(&mut lines, &command, redacted_command_preview);
        }

        if let Some(child) = ChildSupervisionProjection::from_task_record(task) {
            lines.push("  child_agent:".to_string());
            lines.push(format!(
                "    child_agent_id: {}",
                sanitize_inline(&child.child_agent_id)
            ));
            if let Some(workspace_mode) = child.workspace_mode {
                lines.push(format!(
                    "    workspace_mode: {}",
                    sanitize_inline(workspace_mode.label())
                ));
            }
            lines.push(format!(
                "    followup_target: {}",
                sanitize_inline(&child.followup_target)
            ));
            lines.push(format!(
                "    cleanup_owner: {}",
                sanitize_inline(&child.cleanup_owner)
            ));
            if let Some(worktree) = child.worktree {
                if let Some(path) = worktree.worktree_path {
                    lines.push(format!("    worktree_path: {}", sanitize_inline(&path)));
                }
            }
        }

        lines.push("  retrieval:".to_string());
        lines.push(format!(
            "    status: use TaskStatus({}) for lifecycle details",
            task_id
        ));
        lines.push("    output: boundedly read only runtime-confirmed command files when available; observe agent progress with bounded holon tail --agent; neither is task completion".to_string());
        lines.push(format!(
            "    completion: use WaitFor(wake=task_result, resource={}) and result re-entry when only completion remains",
            task_id
        ));
    }

    if total_count > tasks.len() {
        lines.push(format!(
            "... {} more active tasks not shown; use ListTasks for the full active task list.",
            total_count - tasks.len()
        ));
    }

    lines.join("\n")
}

fn render_active_task_command(
    lines: &mut Vec<String>,
    command: &CommandTaskStatusSnapshot,
    redacted_command_preview: fn(&str) -> String,
) {
    if let Some(cmd) = command.cmd.as_deref().filter(|cmd| !cmd.trim().is_empty()) {
        lines.push(format!(
            "    cmd_preview: {}",
            sanitize_inline(&truncate_text(
                &redacted_command_preview(cmd),
                ACTIVE_TASK_CMD_PREVIEW_CHAR_BUDGET
            ))
        ));
    }
    if let Some(cmd_digest) = command.cmd_digest.as_deref() {
        lines.push(format!("    cmd_digest: {}", sanitize_inline(cmd_digest)));
    }
    if let Some(workdir) = command.workdir.as_deref() {
        lines.push(format!("    workdir: {}", sanitize_inline(workdir)));
    }
    if command.tty == Some(true) {
        lines.push("    tty: true".to_string());
    }
    if command.accepts_input == Some(true) {
        lines.push("    accepts_input: true".to_string());
        if let Some(input_target) = command.input_target.as_deref() {
            lines.push(format!(
                "    input_target: {}",
                sanitize_inline(input_target)
            ));
        }
    }
    if let Some(output_path) = command.output_path.as_deref() {
        lines.push(format!("    output_path: {}", sanitize_inline(output_path)));
    }
}

#[cfg(test)]
mod tests;
