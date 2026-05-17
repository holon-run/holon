//! Tool-specific prompt guidance
//!
//! This module provides registry-style organization for tool usage guidance.
//! Each tool section is emitted only when that tool is available.

use super::{section, PromptSection, PromptStability};
use crate::tool::ToolSpec;

/// Build tool-specific prompt sections based on available tools.
///
/// This function implements a registry pattern: each tool has an associated
/// guidance section that is emitted only when that tool is present in the
/// available tools list.
pub fn tool_sections(available_tools: &[ToolSpec]) -> Vec<PromptSection> {
    let mut sections = Vec::new();
    let names = available_tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>();
    let apply_patch_tool = available_tools
        .iter()
        .find(|tool| tool.name == "ApplyPatch");

    if names.contains(&"Sleep") {
        sections.push(section(
            "tool_sleep",
            PromptStability::Stable,
            "Use Sleep when the current task is complete and no immediate follow-up remains. Do not idle-spin by avoiding Sleep once the agent can safely rest. Emit a delivery-ready completion summary in a text block before calling Sleep. The Sleep reason should be a concise label referencing the preceding summary. When calling Sleep, always provide `reason`. Omit `duration_ms` for ordinary indefinite rest. Optionally add a short positive `duration_ms` only when you intentionally want a session-local wake after a bounded delay; never set it to 0. Do not use `duration_ms` as a durable timer or scheduling substitute, and do not expect a task handle from Sleep. Never use Sleep with a vague reason like 'done' or 'completed'.".to_string(),
        ));
    }
    if names.contains(&"SpawnAgent") {
        sections.push(section(
            "tool_spawn_agent",
            PromptStability::Stable,
            "Use SpawnAgent when you need another agent context rather than a command task. SpawnAgent selects behavior through a small `preset` surface: omit it or use `private_child` for the default bounded delegated child, or use `public_named` when you intentionally want a self-owned public agent. Provide one caller text field: `initial_message`. For `private_child`, `initial_message` is required, becomes the child's first delegation message, and is also the source for the parent-visible task label; `private_child` returns both `agent_id` and a structured `task_handle`. The private-child task handle is a background supervision handle by default, so the parent can continue working while the child runs. You may pass `task_handle.task_id` to TaskStatus, TaskOutput, TaskInput, or TaskStop when you need active supervision, intermediate output inspection, follow-up input, or explicit stop control; you do not need to poll the handle just to wait for completion because terminal child results re-enter the parent through the runtime event loop. For `public_named`, `agent_id` is required and `initial_message` is optional bootstrap input; it returns only `agent_id` because it is not parent-supervised through a task handle. Do not pass `summary`, `task_summary`, `prompt`, or `work_item` to SpawnAgent. When the delegated agent should start from a reusable role bootstrap, set `template` to a catalog template id such as `holon-default`, an explicit catalog selector such as `builtin:holon-default`, `agent:worker`, `agent_home:worker`, `user:worker`, or `user_global:worker`, an absolute local template path, or a GitHub template URL; the template only initializes that agent's own `agent_home/AGENTS.md` and agent-local skills, and later edits to the agent's local state remain authoritative. Prefer `workspace_mode=worktree` only for `private_child` when the child truly needs isolated file changes; keep inherited workspace otherwise. SpawnAgent is for bounded delegation or explicit agent creation, not for handing off overall understanding or opening an unconstrained worker swarm.".to_string(),
        ));
    }
    if names.contains(&"AgentGet") {
        sections.push(section(
            "tool_agent_get",
            PromptStability::Stable,
            "Use AgentGet for agent-plane inspection: it returns the current agent summary, including `identity.visibility`, `identity.ownership`, and `identity.profile_preset`, plus active work focus, waiting state, execution snapshot, and visible child-agent lineage. Read those identity fields as the current agent contract: `public_named` means a public self-owned agent addressed directly by `agent_id`, while `private_child` means a private parent-supervised child that stays under a supervising task handle. Child-agent summaries expose the same ownership/profile semantics. Prefer AgentGet when you need to understand the context-owning agent itself. Prefer TaskStatus when you are inspecting a managed task handle such as a command task or a parent-supervised SpawnAgent handle. Do not use AgentGet as a transcript dump or as a substitute for TaskOutput.".to_string(),
        ));
    }
    if names.contains(&"NotifyOperator") {
        sections.push(section(
            "tool_notify_operator",
            PromptStability::Stable,
            "NotifyOperator is a constrained runtime-policy surface for operator delivery adapters, not a normal agent progress or completion channel. Prefer final responses, WorkItem plan_status/blocked_by/completion state, briefs, and runtime-derived activity for operator-facing communication; do not duplicate those facts through ad hoc notifications.".to_string(),
        ));
    }
    if names.contains(&"Enqueue") {
        sections.push(section(
            "tool_enqueue",
            PromptStability::Stable,
            "Use Enqueue only when you need to schedule a follow-up message for this same agent instead of acting immediately in the current tool loop. Prefer `priority=next` for normal continuations, `background` for low-urgency bookkeeping, and reserve `interject` for genuinely urgent self-follow-up that should preempt queued work. Enqueue returns a structured receipt with `enqueued`, `priority`, `follow_up_text`, and `summary_text`; treat that receipt as confirmation that the follow-up entered the runtime queue, not as completion of the follow-up work itself.".to_string(),
        ));
    }
    if names.contains(&"CreateExternalTrigger") || names.contains(&"CancelExternalTrigger") {
        sections.push(section(
            "tool_external_trigger",
            PromptStability::Stable,
            "Use CreateExternalTrigger only to retrieve the default agent external ingress capability for a delivery mode. The capability is long-lived agent infrastructure, not a WorkItem or waiting-intent resource, and should not be created separately for each PR, CI run, issue, source, or WorkItem. Use delivery_mode=wake_hint when an external system with its own durable queue or query API only needs to wake the agent to inspect external state, and delivery_mode=enqueue_message when every callback payload should enter the agent queue. A delivered external trigger records provenance and wakes/re-enters the agent, but it does not automatically clear blocked_by or complete work; inspect the evidence and then explicitly call UpdateWorkItem or CompleteWorkItem as appropriate. Use CancelExternalTrigger only for explicit capability revoke or rotation by external_trigger_id, not normal WorkItem cleanup.".to_string(),
        ));
    }
    if names.contains(&"CreateWorkItem")
        || names.contains(&"PickWorkItem")
        || names.contains(&"UpdateWorkItem")
        || names.contains(&"CompleteWorkItem")
    {
        sections.push(section(
            "tool_work_item_write",
            PromptStability::Stable,
            "Use WorkItem write tools only for durable work state, not as a scratchpad, not for transient current-turn steps, and not as a default planning tool. Use CreateWorkItem to create a new open objective only for genuinely separate work with an independent lifecycle and completion criteria; its optional plan seeds the AgentHome plan artifact. PickWorkItem makes an existing open item current. UpdateWorkItem refines objective, plan_status, todo_list, and/or blocked_by; edit plan_artifact.path directly for plan body changes instead of passing plan text to UpdateWorkItem. CompleteWorkItem only when the objective is actually complete. Reuse the current WorkItem whenever the underlying tracked objective is the same; continuous discussion, planning threads, candidate issue screening, option comparison, and incremental decisions should normally update one WorkItem rather than create another. Create a new WorkItem only when the operator asks for independent execution or the objective has an independent lifecycle. Do not create a new WorkItem just to refine, narrow, or switch candidates inside the same planning thread; if the current WorkItem is still the same underlying task, update its objective, plan_status, and todo_list with UpdateWorkItem and edit the plan artifact file as needed. If an old WorkItem should be replaced, complete it first or explicitly PickWorkItem for the intended item before creating genuinely independent work. Any cross-turn waiting, callback-driven continuation, or sleep-ready handoff should already be anchored in a current work item before the turn ends. For tracked work, maintain the plan as durable prose in plan_artifact.path and todo_list as a durable progress checklist, not disposable current-turn steps. Before nontrivial file mutation or other high-commitment action on tracked work, make sure the current work item has a durable plan and set plan_status=ready once the plan is stable; use plan_status=needs_input when the current open item is waiting for operator input and must not be scheduler-resumed as runnable work. If task interpretation, scope, or acceptance changes, update objective or plan_status and edit the plan artifact before continuing. Update todo_list after material progress such as a code change, verification result, blocker discovery, or completed inspection objective. Work-item updates are coordination/bookkeeping and do not replace file mutation, verification, PR/issue updates, final delivery, or other artifact progress. If the current item remains open because progress is blocked, record the specific blocker or missing fact in blocked_by instead of silently widening exploration. Use blocked_by/plan_status for durable WorkItem waiting state and agent-level external triggers for reusable ingress when an external system should wake the agent; use TaskOutput(block=true) only for a bounded current-turn output check, not as durable work-item dependency state. Before completing a WorkItem, audit whether the acceptance evidence is present and verification status is known. Complete explicitly when complete; completion is your confirmation and clears blocked_by; explicitly cancel external triggers only when revoking or rotating a capability. Write the operator-facing completion report as assistant text in the same round as the focused CompleteWorkItem call; after that tool succeeds, the runtime promotes that exact text as the canonical completion report.".to_string(),
        ));
    }
    if names.contains(&"GetWorkItem") || names.contains(&"ListWorkItems") {
        sections.push(section(
            "tool_work_item_read",
            PromptStability::Stable,
            "Use ListWorkItems with filter=current to inspect the current work-item focus before relying on memory briefs. Use GetWorkItem when you already know the id and need its open/completed state, focus flag, readiness, plan_artifact descriptor with bounded preview, and optional todo_list. Read or edit plan_artifact.path directly when the preview is incomplete or the plan body needs changes. Use ListWorkItems for queue inspection with filters such as open, completed, current, queued, blocked, waiting_for_operator, and runnable. Treat current_work_item_id as focus, not lifecycle; open/completed describes completion, current/queued/blocked describes focus, and readiness describes scheduler eligibility. Read the work-item surface before switching, completing, or expanding cross-turn work so the next action is anchored to the right objective.".to_string(),
        ));
    }
    if names.contains(&"TaskList")
        || names.contains(&"TaskStatus")
        || names.contains(&"TaskInput")
        || names.contains(&"TaskOutput")
        || names.contains(&"TaskStop")
    {
        sections.push(section(
            "tool_task_control",
            PromptStability::Stable,
            "Use TaskList to inspect background work when coordinating longer flows. TaskList is intentionally compact and only shows a digest view such as id, kind, status, summary, updated_at, and wait_policy. Use TaskStatus for structured lifecycle metadata; it returns a stable envelope with a compact `task` snapshot rather than raw output bytes or full internal task detail. Use TaskInput when a managed task explicitly needs continuation input. Command tasks accept stdin or tty input there only when they were created with interactive continuation enabled, and parent-supervised child handles accept bounded follow-up input on the same surface. Check TaskStatus before sending input so you can confirm the task kind, lifecycle state, and whether the command snapshot still advertises `accepts_input`; if the task is not currently accepting input, expect a structured rejection receipt instead of assuming transport failure. For child supervision handles, expect `input_target=child_followup` instead of stdin-style delivery. Use TaskOutput when you need a bounded output preview, artifact refs, or an explicit short synchronous check inside the current turn; its canonical result is `{ retrieval_status, task }`, but the command-family tool receipt shown back to the model is a compact text summary with task status, preview text, and artifact refs when present. Do not use TaskOutput polling as the default way to wait for a child agent or command task to finish; when you are simply waiting for completion, call Sleep and let the runtime wake you from the terminal TaskResult event. For command tasks specifically, TaskOutput keeps bounded `output_preview` plus path-only artifact refs for full output, while TaskStatus returns coordination metadata such as `output_path`, `result_summary`, `exit_status`, and `terminal_reentry`; terminal re-entry is not a scheduler blocking policy. Use TaskStop only when a task is clearly no longer useful, is blocking progress, or has become irrelevant; it returns a structured stop receipt with the updated task snapshot, and command task stop may first report `cancelling` before the final `cancelled` result arrives. In longer sessions with multiple subtasks: (1) use TaskList to see overall task status at a glance, (2) use TaskStatus to inspect lifecycle metadata before deciding what to do next, (3) use TaskInput only when a managed task truly needs follow-up input, (4) use TaskOutput only when you need bounded output inspection or an explicit current-turn check, (5) use Sleep when waiting for task completion without immediate intervention, and (6) use TaskStop when explicit stop semantics are actually needed. These tools add value in multi-step coordination but should not be forced into simple single-turn tasks.".to_string(),
        ));
    }
    if names.contains(&"ExecCommand") {
        sections.push(section(
            "tool_exec_command",
            PromptStability::Stable,
            "Use ExecCommand as the primary repo-inspection and verification primitive. For code and docs, prefer shell-first inspection patterns such as `rg --files`, `rg -n`, `sed -n start,endp`, `head`, and `tail`. Startup input is only the command-start contract: `cmd` plus optional `workdir`, `shell`, `login`, `tty`, `accepts_input`, `continue_on_result`, `yield_time_ms`, and `max_output_tokens`. `workdir` is optional and usually should be omitted because Holon defaults it to the current workspace cwd. Only set `workdir` when you truly need a different directory, and then prefer a short relative path inside the workspace instead of copying a long absolute worktree path. `yield_time_ms` is optional and defaults to 10_000 ms; omit it unless you intentionally want a shorter or longer foreground wait window. Before setting `yield_time_ms`, ask whether you are deliberately trying to change when the command returns or becomes a background task; if not, omit it. Narrow commands before repeating broad scans, and do not dump large files with `cat` unless no smaller slice can answer the current question. Keep command startup compact: prefer checked-in scripts, temp files, or path-based artifacts over huge inline heredocs when generating or transforming large content. When several bounded shell commands should run before the next decision and ExecCommandBatch is available, prefer it instead of shell separator scripts; otherwise keep one-off commands on ExecCommand.\n\nValid startup examples:\n- `{ \"cmd\": \"rg -n \\\"render_for_model\\\" src\" }`\n- `{ \"cmd\": \"sed -n '1,120p' src/runtime/turn.rs\", \"max_output_tokens\": 1200 }`\n- `{ \"cmd\": \"python -i\", \"tty\": true, \"accepts_input\": true }`\n\nInvalid startup shapes:\n- `{ \"command\": \"rg -n ...\" }` because the field is `cmd`, not `command`\n- `{ \"cmd\": \"cargo test\", \"status\": \"running\" }` because `status` is result/task metadata, not startup input\n- `{ \"cmd\": \"git status\", \"commentary\": \"checking repo\" }` because free-form commentary is not an ExecCommand field\n\nAfter a failed edit or verification command, inspect the relevant failure output once, then make one focused correction. Avoid repeated micro-commands that only move one line at a time or re-check the same nearby slice without new evidence.\n\nKeep startup, immediate result, and promoted-task semantics separate. ExecCommand keeps a structured canonical result with fields such as `disposition`, `exit_status`, bounded previews, truncation flags, artifact refs, and command cost diagnostics, but the command-family tool receipt shown back to the model is rendered as a readable text receipt instead of a raw JSON dump. Command output uses a bounded default budget and per-call `max_output_tokens` is only useful when the next decision truly needs more preview text; artifact refs remain the route for full output. If the command exceeds `yield_time_ms` (default 10_000 ms), Holon promotes it into a `command_task` and returns `disposition=promoted_to_task` plus `task_handle`, `initial_output_preview`, and `initial_output_truncated`. Those are result fields, not valid startup input. When output is truncated, refine the command instead of asking for more of the same wide dump. After promotion, pass `task_handle.task_id` to TaskOutput only when you need bounded output retrieval or an explicit current-turn check; for ordinary completion waiting, call Sleep and let the runtime wake you from the terminal TaskResult event. Use TaskStatus/TaskList for coordination metadata.".to_string(),
        ));
    }
    if names.contains(&"ExecCommandBatch") {
        sections.push(section(
            "tool_exec_command_batch",
            PromptStability::Stable,
            "Use ExecCommandBatch when several bounded shell commands should run before the next decision and do not require interactive input, background task management, or command-task continuation. Each item uses restricted ExecCommand startup fields: `cmd`, optional `workdir`, `shell`, `login`, `yield_time_ms`, and `max_output_tokens`. Per-item `yield_time_ms` is optional and defaults to 10_000 ms; omit it by default, and set it only when intentionally changing the foreground wait window for that item. Do not pass `tty`, `accepts_input`, or `continue_on_result`; call ExecCommand directly for interactive, tty, or long-running supervised commands. ExecCommandBatch runs items sequentially and returns one grouped receipt with per-item status, output previews, truncation flags, command previews, and errors. Use it instead of unstructured shell separator scripts when item boundaries matter, but keep each item command compact and artifact-oriented. Do not use it for edits, arbitrary nested tools, installs, or commands whose later items depend on earlier output unless you intentionally set `stop_on_error` for a bounded sequence.".to_string(),
        ));
    }
    if let Some(apply_patch_tool) = apply_patch_tool {
        let invocation_contract = if apply_patch_tool.freeform_grammar.is_some() {
            "Current ApplyPatch surface is a freeform grammar tool: send the unified diff body directly. Do not wrap it in JSON, and do not use `patch` or `input` fields."
        } else {
            "Current ApplyPatch surface is a JSON/function tool: call it with exactly `{\"patch\":\"--- a/path\\n+++ b/path\\n@@ -1,1 +1,1 @@\\n-old\\n+new\\n\"}`. Do not use `input`, and do not send a raw freeform diff body."
        };
        sections.push(section(
            "tool_apply_patch",
            PromptStability::Stable,
            format!("Use ApplyPatch as the primary precise file-mutation tool. Use it for focused new files, small local edits, multi-hunk single-file changes, structural edits, and bounded refactors. For very large new files, generated files, whole-file rewrites, bulk deletes, or broad mechanical refactors, choose the lower-context path: split the change into smaller ApplyPatch calls, or use a carefully bounded ExecCommand/scripted rewrite when that avoids emitting a huge diff and is easier to verify. Do not expect separate Write or Edit tools; express ordinary file mutation as unified diff text. {invocation_contract} The model-visible ApplyPatch receipt is concise text with action markers like `A`, `M`, `D`, or `R`; if it says success with diagnostics, treat the target file as potentially different from your intended mental model. The canonical result still records structured changed-file metadata, `changed_paths`, `changed_file_count`, ignored metadata, diagnostics, and `summary_text`."),
        ));
    }
    if names.contains(&"ApplyPatch") {
        sections.push(section(
            "tool_file_mutation",
            PromptStability::Stable,
            "File mutation is workspace-scoped and centered on ApplyPatch. Use unified diff `---`/`+++` file headers and `@@` hunks for deletes, precise edits, and renames. Prefer focused hunks with enough surrounding context to stay unambiguous. Include at least 3 context lines before and after each changed line, and expand to 5–10 lines when the file contains repeated structures or similar patterns. Blank lines within hunks must have a space prefix to be valid context lines. Keep tool output bounded: do not paste enormous malformed patches, do not retry the same large failed patch unchanged, and split large refactors into smaller patch/application steps when that keeps failures recoverable. Avoid using ExecCommand with shell rewrite tricks like `sed -i` as the default editing path, but use a bounded script or heredoc when generating/replacing a large file is cheaper and safer than a huge diff. After a clean file mutation, rely on the ApplyPatch receipt first, then run focused verification with ExecCommand when correctness matters. If ApplyPatch reports diagnostics, warnings, partial application, context mismatch, or any non-clean receipt, inspect the exact affected region before continuing edits to that file and do not retry the same diagnostic-producing patch unchanged. Do not use file mutation tools for broad exploration; inspect with shell-first read commands through ExecCommand instead.".to_string(),
        ));
    }
    if names.contains(&"UseWorkspace") {
        sections.push(section(
            "tool_workspace",
            PromptStability::Stable,
            "Workspace is explicit runtime state, not just a shell directory. The active workspace defines the instruction root, execution root, default cwd, workspace-scoped memory/policy boundary, and where ApplyPatch and future local tools operate. Every agent always has exactly one active workspace. `agent_home` is the built-in fallback workspace for durable agent-local state; it is not a substitute for project work.\n\nUse UseWorkspace to make the right workspace active before local file or command work. Call `UseWorkspace({\"path\":\"/repo/or/subdir\"})` when the operator gave you a project path or you need to discover/adopt a directory. Call `UseWorkspace({\"workspace_id\":\"agent_home\"})` to return to AgentHome, or `UseWorkspace({\"workspace_id\":\"ws-...\"})` to switch to a known workspace id from agent state. Provide exactly one of `path` or `workspace_id`. Use `mode=\"isolated\"` only when you need a runtime-managed isolated execution root, and provide an `isolation_label` as an intent/branch hint rather than inventing a worktree path.\n\nShell `cd` affects only that shell command process. It does not redefine the active workspace, instruction root, or ApplyPatch target root. Switching workspaces does not delete files, remove bindings, or clean up retained isolated roots; cleanup is a separate explicit lifecycle action.".to_string(),
        ));
    }
    sections
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_sleep_section_emitted_when_sleep_available() {
        let tools = vec![ToolSpec {
            name: "Sleep".into(),
            description: String::new(),
            input_schema: json!({}),
            freeform_grammar: None,
        }];
        let sections = tool_sections(&tools);
        assert!(sections.iter().any(|s| s.name == "tool_sleep"));
    }

    #[test]
    fn test_sleep_section_not_emitted_when_sleep_unavailable() {
        let tools = vec![ToolSpec {
            name: "ExecCommand".into(),
            description: String::new(),
            input_schema: json!({}),
            freeform_grammar: None,
        }];
        let sections = tool_sections(&tools);
        assert!(!sections.iter().any(|s| s.name == "tool_sleep"));
    }

    #[test]
    fn test_spawn_agent_section_emitted_when_available() {
        let tools = vec![ToolSpec {
            name: "SpawnAgent".into(),
            description: String::new(),
            input_schema: json!({}),
            freeform_grammar: None,
        }];
        let sections = tool_sections(&tools);
        let section = sections
            .iter()
            .find(|s| s.name == "tool_spawn_agent")
            .expect("spawn agent section");
        assert!(section
            .content
            .contains("You may pass `task_handle.task_id`"));
        assert!(section.content.contains("active supervision"));
        assert!(section
            .content
            .contains("you do not need to poll the handle just to wait for completion"));
        assert!(section.content.contains("runtime event loop"));
    }

    #[test]
    fn test_agent_get_section_emitted_when_available() {
        let tools = vec![ToolSpec {
            name: "AgentGet".into(),
            description: String::new(),
            input_schema: json!({}),
            freeform_grammar: None,
        }];
        let sections = tool_sections(&tools);
        assert!(sections.iter().any(|s| s.name == "tool_agent_get"));
        let section = sections
            .iter()
            .find(|s| s.name == "tool_agent_get")
            .expect("agent get section");
        assert!(section.content.contains("identity.ownership"));
        assert!(section.content.contains("identity.profile_preset"));
        assert!(section.content.contains("public_named"));
        assert!(section.content.contains("private_child"));
    }

    #[test]
    fn test_notify_operator_section_emitted_when_available() {
        let tools = vec![ToolSpec {
            name: "NotifyOperator".into(),
            description: String::new(),
            input_schema: json!({}),
            freeform_grammar: None,
        }];
        let sections = tool_sections(&tools);
        let section = sections
            .iter()
            .find(|s| s.name == "tool_notify_operator")
            .expect("notify operator section");
        assert!(section.content.contains("runtime-policy surface"));
        assert!(section.content.contains("not a normal agent progress"));
        assert!(section.content.contains("WorkItem plan_status/blocked_by"));
    }

    #[test]
    fn test_enqueue_section_emitted_when_available() {
        let tools = vec![ToolSpec {
            name: "Enqueue".into(),
            description: String::new(),
            input_schema: json!({}),
            freeform_grammar: None,
        }];
        let sections = tool_sections(&tools);
        let section = sections
            .iter()
            .find(|s| s.name == "tool_enqueue")
            .expect("enqueue section");
        assert!(section.content.contains("structured receipt"));
        assert!(section.content.contains("follow_up_text"));
    }

    #[test]
    fn test_task_output_section_emitted_when_available() {
        let tools = vec![ToolSpec {
            name: "TaskOutput".into(),
            description: String::new(),
            input_schema: json!({}),
            freeform_grammar: None,
        }];
        let sections = tool_sections(&tools);
        let section = sections
            .iter()
            .find(|s| s.name == "tool_task_control")
            .expect("task control section");
        assert!(section
            .content
            .contains("Do not use TaskOutput polling as the default way"));
        assert!(section
            .content
            .contains("call Sleep and let the runtime wake you"));
        assert!(section.content.contains("terminal TaskResult event"));
        assert!(section
            .content
            .contains("bounded output inspection or an explicit current-turn check"));
    }

    #[test]
    fn test_task_input_section_emitted_when_available() {
        let tools = vec![ToolSpec {
            name: "TaskInput".into(),
            description: String::new(),
            input_schema: json!({}),
            freeform_grammar: None,
        }];
        let sections = tool_sections(&tools);
        assert!(sections.iter().any(|s| s.name == "tool_task_control"));
    }

    #[test]
    fn test_exec_command_section_emitted_when_available() {
        let tools = vec![ToolSpec {
            name: "ExecCommand".into(),
            description: String::new(),
            input_schema: json!({}),
            freeform_grammar: None,
        }];
        let sections = tool_sections(&tools);
        assert!(sections.iter().any(|s| s.name == "tool_exec_command"));
    }

    #[test]
    fn test_work_item_write_section_emitted_when_available() {
        let tools = vec![ToolSpec {
            name: "CreateWorkItem".into(),
            description: String::new(),
            input_schema: json!({}),
            freeform_grammar: None,
        }];
        let sections = tool_sections(&tools);
        let section = sections
            .iter()
            .find(|s| s.name == "tool_work_item_write")
            .expect("work item write section");
        assert!(section.content.contains("durable work state"));
        assert!(section.content.contains("genuinely separate work"));
        assert!(section
            .content
            .contains("independent lifecycle and completion criteria"));
        assert!(section.content.contains("not as a scratchpad"));
        assert!(section
            .content
            .contains("not for transient current-turn steps"));
        assert!(section.content.contains("not as a default planning tool"));
        assert!(section.content.contains("Reuse the current WorkItem"));
        assert!(section
            .content
            .contains("continuous discussion, planning threads"));
        assert!(section.content.contains("candidate issue screening"));
        assert!(section
            .content
            .contains("Do not create a new WorkItem just to refine"));
        assert!(section
            .content
            .contains("switch candidates inside the same planning thread"));
        assert!(section
            .content
            .contains("update its objective, plan_status, and todo_list"));
        assert!(section.content.contains("edit the plan artifact file"));
        assert!(section.content.contains("plan_status=ready"));
        assert!(section.content.contains("plan_status=needs_input"));
        assert!(section
            .content
            .contains("must not be scheduler-resumed as runnable work"));
        assert!(section
            .content
            .contains("update objective or plan_status and edit the plan artifact"));
        assert!(section
            .content
            .contains("Update todo_list after material progress"));
        assert!(!section
            .content
            .contains("use ApplyPatch first and update the work item afterward"));
        assert!(section.content.contains("coordination/bookkeeping"));
        assert!(section
            .content
            .contains("same round as the focused CompleteWorkItem call"));
        assert!(section
            .content
            .contains("promotes that exact text as the canonical completion report"));
        assert!(!section.content.contains("result_summary"));
        assert!(section.content.contains("specific blocker or missing fact"));
        assert!(section
            .content
            .contains("instead of silently widening exploration"));
        assert!(section
            .content
            .contains("audit whether the acceptance evidence is present"));
    }

    #[test]
    fn test_work_item_read_section_emitted_when_available() {
        let tools = vec![
            ToolSpec {
                name: "GetWorkItem".into(),
                description: String::new(),
                input_schema: json!({}),
                freeform_grammar: None,
            },
            ToolSpec {
                name: "ListWorkItems".into(),
                description: String::new(),
                input_schema: json!({}),
                freeform_grammar: None,
            },
        ];
        let sections = tool_sections(&tools);
        let section = sections
            .iter()
            .find(|s| s.name == "tool_work_item_read")
            .expect("work item read section");
        assert!(section.content.contains("current_work_item_id as focus"));
        assert!(section.content.contains("open/completed"));
        assert!(section.content.contains("waiting_for_operator"));
        assert!(section.content.contains("runnable"));
        assert!(section
            .content
            .contains("readiness describes scheduler eligibility"));
        assert!(section.content.contains("before relying on memory briefs"));
    }

    #[test]
    fn test_external_trigger_section_prefers_new_names() {
        let tools = vec![ToolSpec {
            name: "CreateExternalTrigger".into(),
            description: String::new(),
            input_schema: json!({}),
            freeform_grammar: None,
        }];
        let sections = tool_sections(&tools);
        let section = sections
            .iter()
            .find(|s| s.name == "tool_external_trigger")
            .expect("external trigger section");
        assert!(section.content.contains("CreateExternalTrigger"));
        assert!(section.content.contains("CancelExternalTrigger"));
    }

    #[test]
    fn test_exec_command_guidance_is_shell_first() {
        let tools = vec![ToolSpec {
            name: "ExecCommand".into(),
            description: String::new(),
            input_schema: json!({}),
            freeform_grammar: None,
        }];
        let sections = tool_sections(&tools);
        let section = sections
            .iter()
            .find(|s| s.name == "tool_exec_command")
            .expect("exec command section");
        assert!(section.content.contains("rg --files"));
        assert!(section.content.contains("rg -n"));
        assert!(section.content.contains("sed -n start,endp"));
        assert!(section
            .content
            .contains("`workdir` is optional and usually should be omitted"));
        assert!(section
            .content
            .contains("prefer a short relative path inside the workspace"));
        assert!(section
            .content
            .contains("`yield_time_ms` is optional and defaults to 10_000 ms"));
        assert!(section
            .content
            .contains("Before setting `yield_time_ms`, ask whether"));
        assert!(section
            .content
            .contains("do not dump large files with `cat`"));
        assert!(section
            .content
            .contains("and ExecCommandBatch is available, prefer it"));
        assert!(section.content.contains("Avoid repeated micro-commands"));
        assert!(section
            .content
            .contains("readable text receipt instead of a raw JSON dump"));
        assert!(section.content.contains("Valid startup examples:"));
        assert!(section.content.contains("{ \"cmd\": \"rg -n"));
        assert!(section
            .content
            .contains("the field is `cmd`, not `command`"));
        assert!(section
            .content
            .contains("`status` is result/task metadata, not startup input"));
        assert!(section
            .content
            .contains("Those are result fields, not valid startup input"));
        assert!(section.content.contains("`disposition=promoted_to_task`"));
        assert!(section
            .content
            .contains("When output is truncated, refine the command"));
        assert!(section
            .content
            .contains("TaskOutput only when you need bounded output retrieval"));
        assert!(section
            .content
            .contains("for ordinary completion waiting, call Sleep"));
        assert!(section.content.contains("terminal TaskResult event"));
    }

    #[test]
    fn test_exec_command_batch_section_emitted_when_available() {
        let tools = vec![ToolSpec {
            name: "ExecCommandBatch".into(),
            description: String::new(),
            input_schema: json!({}),
            freeform_grammar: None,
        }];
        let sections = tool_sections(&tools);
        let section = sections
            .iter()
            .find(|s| s.name == "tool_exec_command_batch")
            .expect("exec command batch section");
        assert!(section.content.contains("bounded shell commands"));
        assert!(section.content.contains("runs items sequentially"));
        assert!(section.content.contains("Do not pass `tty`"));
        assert!(section.content.contains("defaults to 10_000 ms"));
        assert!(section.content.contains("omit it by default"));
        assert!(section.content.contains("grouped receipt"));
    }

    #[test]
    fn test_retired_repo_native_sections_are_not_emitted() {
        let tools = vec![ToolSpec {
            name: "ExecCommand".into(),
            description: String::new(),
            input_schema: json!({}),
            freeform_grammar: None,
        }];
        let sections = tool_sections(&tools);
        assert!(!sections.iter().any(|s| s.name == "tool_read"));
        assert!(!sections.iter().any(|s| s.name == "tool_glob"));
        assert!(!sections.iter().any(|s| s.name == "tool_grep"));
    }

    #[test]
    fn test_apply_patch_section_emitted_when_available() {
        let tools = vec![ToolSpec {
            name: "ApplyPatch".into(),
            description: String::new(),
            input_schema: json!({}),
            freeform_grammar: None,
        }];
        let sections = tool_sections(&tools);
        assert!(sections.iter().any(|s| s.name == "tool_apply_patch"));
        assert!(sections.iter().any(|s| s.name == "tool_file_mutation"));
    }

    #[test]
    fn test_apply_patch_section_mentions_retired_write_and_edit_surface() {
        let tools = vec![ToolSpec {
            name: "ApplyPatch".into(),
            description: String::new(),
            input_schema: json!({}),
            freeform_grammar: None,
        }];
        let sections = tool_sections(&tools);
        let apply_patch = sections
            .iter()
            .find(|s| s.name == "tool_apply_patch")
            .expect("apply patch section");
        assert!(apply_patch
            .content
            .contains("Do not expect separate Write or Edit tools"));
        assert!(apply_patch.content.contains("lower-context path"));
        assert!(apply_patch.content.contains("very large new files"));
        assert!(apply_patch.content.contains("success with diagnostics"));
        assert!(apply_patch
            .content
            .contains("Current ApplyPatch surface is a JSON/function tool"));
        assert!(apply_patch
            .content
            .contains("do not send a raw freeform diff body"));
        assert!(!apply_patch
            .content
            .contains("Current ApplyPatch surface is a freeform grammar tool"));
        let section = sections
            .iter()
            .find(|s| s.name == "tool_file_mutation")
            .expect("file mutation section");
        assert!(section.content.contains("centered on ApplyPatch"));
        assert!(section.content.contains("sed -i"));
        assert!(section
            .content
            .contains("do not retry the same large failed patch unchanged"));
        assert!(section.content.contains("bounded script or heredoc"));
        assert!(section.content.contains("non-clean receipt"));
        assert!(section
            .content
            .contains("inspect the exact affected region"));
    }

    #[test]
    fn test_apply_patch_section_uses_freeform_invocation_when_grammar_is_available() {
        let tools = vec![ToolSpec {
            name: "ApplyPatch".into(),
            description: String::new(),
            input_schema: json!({}),
            freeform_grammar: Some(crate::tool::spec::ToolFreeformGrammar {
                syntax: "lark".into(),
                definition: "start: /.+/".into(),
            }),
        }];
        let sections = tool_sections(&tools);
        let apply_patch = sections
            .iter()
            .find(|s| s.name == "tool_apply_patch")
            .expect("apply patch section");

        assert!(apply_patch
            .content
            .contains("Current ApplyPatch surface is a freeform grammar tool"));
        assert!(apply_patch
            .content
            .contains("send the unified diff body directly"));
        assert!(apply_patch
            .content
            .contains("do not use `patch` or `input` fields"));
        assert!(!apply_patch
            .content
            .contains("Current ApplyPatch surface is a JSON/function tool"));
    }

    #[test]
    fn test_workspace_section_emitted_when_available() {
        let tools = vec![ToolSpec {
            name: "UseWorkspace".into(),
            description: String::new(),
            input_schema: json!({}),
            freeform_grammar: None,
        }];
        let sections = tool_sections(&tools);
        let section = sections
            .iter()
            .find(|s| s.name == "tool_workspace")
            .expect("workspace section");
        assert!(section
            .content
            .contains("always has exactly one active workspace"));
        assert!(section.content.contains("agent_home"));
        assert!(section
            .content
            .contains("Shell `cd` affects only that shell command"));
        assert!(section.content.contains("UseWorkspace"));
    }

    #[test]
    fn test_multiple_tool_sections_emitted() {
        let tools = vec![
            ToolSpec {
                name: "Sleep".into(),
                description: String::new(),
                input_schema: json!({}),
                freeform_grammar: None,
            },
            ToolSpec {
                name: "TaskOutput".into(),
                description: String::new(),
                input_schema: json!({}),
                freeform_grammar: None,
            },
            ToolSpec {
                name: "ExecCommand".into(),
                description: String::new(),
                input_schema: json!({}),
                freeform_grammar: None,
            },
            ToolSpec {
                name: "ApplyPatch".into(),
                description: String::new(),
                input_schema: json!({}),
                freeform_grammar: None,
            },
        ];
        let sections = tool_sections(&tools);
        assert!(sections.iter().any(|s| s.name == "tool_sleep"));
        assert!(sections.iter().any(|s| s.name == "tool_task_control"));
        assert!(sections.iter().any(|s| s.name == "tool_exec_command"));
        assert!(sections.iter().any(|s| s.name == "tool_apply_patch"));
        assert!(sections.iter().any(|s| s.name == "tool_file_mutation"));
    }

    #[test]
    fn test_empty_tools_returns_empty_sections() {
        let sections = tool_sections(&[]);
        assert!(sections.is_empty());
    }
}
