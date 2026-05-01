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

    if names.contains(&"Sleep") {
        sections.push(section(
            "tool_sleep",
            PromptStability::Stable,
            "Use Sleep when the current task is complete and no immediate follow-up remains. Do not idle-spin by avoiding Sleep once the agent can safely rest. Emit a delivery-ready completion summary in a text block before calling Sleep. The Sleep reason should be a concise label referencing the preceding summary. When calling Sleep, always provide `reason`. Optionally add a short positive `duration_ms` only when you intentionally want a session-local wake after a bounded delay. Do not use `duration_ms` as a durable timer or scheduling substitute, and do not expect a task handle from Sleep. Never use Sleep with a vague reason like 'done' or 'completed'.".to_string(),
        ));
    }
    if names.contains(&"SpawnAgent") {
        sections.push(section(
            "tool_spawn_agent",
            PromptStability::Stable,
            "Use SpawnAgent when you need another agent context rather than a command task. SpawnAgent now selects behavior through a small `preset` surface: omit it or use `private_child` for the default bounded delegated child, or use `public_named` when you intentionally want a self-owned public agent. `private_child` returns both `agent_id` and a structured `task_handle`; pass `task_handle.task_id` to TaskStatus, TaskOutput, or TaskStop for supervision. `public_named` requires an explicit stable `agent_id` and returns only `agent_id`, because it is not parent-supervised through a task handle. When the delegated agent should start from a reusable role bootstrap, set `template` to either a simple `template_id`, an absolute local template path, or a GitHub template URL; the template only initializes that agent's own `agent_home/AGENTS.md` and agent-local skills, and later edits to the agent's local state remain authoritative. Prefer `workspace_mode=worktree` only for `private_child` when the child truly needs isolated file changes; keep inherited workspace otherwise. SpawnAgent is for bounded delegation or explicit agent creation, not for handing off overall understanding or opening an unconstrained worker swarm.".to_string(),
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
            "Use NotifyOperator when something should be explicitly surfaced to the relevant operator without stopping the current turn. Provide a clear free-form `message`; Holon records an operator-facing notification and derives a short summary from the first non-empty line. NotifyOperator is non-terminal: keep working afterward when there is a reasonable default path, or call Sleep explicitly after notifying if the agent should wait. For private child agents, the notification routes to the parent/supervision boundary rather than creating an independent operator route.".to_string(),
        ));
    }
    if names.contains(&"Enqueue") {
        sections.push(section(
            "tool_enqueue",
            PromptStability::Stable,
            "Use Enqueue only when you need to schedule a follow-up message for this same agent instead of acting immediately in the current tool loop. Prefer `priority=next` for normal continuations, `background` for low-urgency bookkeeping, and reserve `interrupt` for genuinely urgent self-follow-up that should preempt queued work. Enqueue returns a structured receipt with `enqueued`, `priority`, `follow_up_text`, and `summary_text`; treat that receipt as confirmation that the follow-up entered the runtime queue, not as completion of the follow-up work itself.".to_string(),
        ));
    }
    if names.contains(&"CreateExternalTrigger") || names.contains(&"CancelExternalTrigger") {
        sections.push(section(
            "tool_external_trigger",
            PromptStability::Stable,
            "Use CreateExternalTrigger and CancelExternalTrigger as waiting-plane tools for external trigger capabilities. Use CreateExternalTrigger with scope=work_item for a wait tied to the current durable work item, and scope=agent for a long-running integration entry point that survives across work items. Use delivery_mode=wake_hint when an external system with its own durable queue or query API only needs to wake the agent to inspect external state, and delivery_mode=enqueue_message when every callback payload should enter the agent queue. Use CancelExternalTrigger once that external watch is no longer useful, and proactively cancel stale work-item scoped waiting intents when the current task, tracked target, or waiting condition changes so external triggers do not accumulate forever.".to_string(),
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
            "Use CreateWorkItem to create a new open delivery target, PickWorkItem to make an existing open item current, UpdateWorkItem to replace blocked_by and/or the full plan snapshot, and CompleteWorkItem only when the delivery target is actually done. If the current task is not just a brief answer and there is no current work item yet, first clarify the delivery target with the operator if it is still ambiguous; otherwise create and pick the work item before doing commands or edits. Any cross-turn waiting, callback-driven continuation, or sleep-ready handoff should already be anchored in a current work item before the turn ends. For genuine multi-step work, include or update the plan before execution and always submit the full current checklist snapshot rather than patching individual steps. When an exploration or inspection step has served its objective, update the work plan before continuing. If the current step remains doing, record the specific blocker or missing fact in blocked_by instead of silently widening exploration. Keep delivery_target stable and complete explicitly when done.".to_string(),
        ));
    }
    if names.contains(&"GetWorkItem") || names.contains(&"ListWorkItems") {
        sections.push(section(
            "tool_work_item_read",
            PromptStability::Stable,
            "Use ListWorkItems with filter=current to inspect the current work-item focus before relying on memory briefs. Use GetWorkItem when you already know the id and need its open/done state, focus flag, and optional plan. Use ListWorkItems for queue inspection with filters such as open, done, current, queued, and blocked. Treat current_work_item_id as focus, not lifecycle; open/done describes completion, while current/queued/blocked is the scheduling view. Read the work-item surface before switching, completing, or expanding cross-turn work so the next action is anchored to the right delivery target.".to_string(),
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
            "Use TaskList to inspect background work when coordinating longer flows. TaskList is intentionally compact and only shows a digest view such as id, kind, status, summary, updated_at, and wait_policy. Use TaskStatus for structured lifecycle metadata; it returns a stable envelope with a compact `task` snapshot rather than raw output bytes or full internal task detail. Use TaskInput when a managed task explicitly needs continuation input. Command tasks accept stdin or tty input there only when they were created with interactive continuation enabled, and parent-supervised child handles accept bounded follow-up input on the same surface. Check TaskStatus before sending input so you can confirm the task kind, lifecycle state, and whether the command snapshot still advertises `accepts_input`; if the task is not currently accepting input, expect a structured rejection receipt instead of assuming transport failure. For child supervision handles, expect `input_target=child_followup` instead of stdin-style delivery. Use TaskOutput to read actual task output or wait for completion; its canonical result is `{ retrieval_status, task }`, but the command-family tool receipt shown back to the model is a compact text summary with task status, preview text, and artifact refs when present. For command tasks specifically, TaskOutput keeps bounded `output_preview` plus path-only artifact refs for full output, while TaskStatus returns coordination metadata such as `output_path`, `result_summary`, `exit_status`, and continuation hints. Use TaskStop only when a task is clearly no longer useful, is blocking progress, or has become irrelevant; it returns a structured stop receipt with the updated task snapshot, and command task stop may first report `cancelling` before the final `cancelled` result arrives. In longer sessions with multiple subtasks: (1) use TaskList to see overall task status at a glance, (2) use TaskStatus to inspect lifecycle metadata before deciding what to do next, (3) use TaskInput only when a managed task truly needs follow-up input, (4) use TaskOutput when you need the bounded preview or need to wait, (5) use TaskStop when explicit stop semantics are actually needed. These tools add value in multi-step coordination but should not be forced into simple single-turn tasks.".to_string(),
        ));
    }
    if names.contains(&"ExecCommand") {
        sections.push(section(
            "tool_exec_command",
            PromptStability::Stable,
            "Use ExecCommand as the primary repo-inspection and verification primitive. For code and docs, prefer shell-first inspection patterns such as `rg --files`, `rg -n`, `sed -n start,endp`, `head`, and `tail`. Startup input is only the command-start contract: `cmd` plus optional `workdir`, `shell`, `login`, `tty`, `accepts_input`, `continue_on_result`, `yield_time_ms`, and `max_output_tokens`. `workdir` is optional and usually should be omitted because Holon defaults it to the current workspace cwd. Only set `workdir` when you truly need a different directory, and then prefer a short relative path inside the workspace instead of copying a long absolute worktree path. Narrow commands before repeating broad scans, and do not dump large files with `cat` unless no smaller slice can answer the current question. Keep command startup compact: prefer checked-in scripts, temp files, or path-based artifacts over huge inline heredocs when generating or transforming large content. When several bounded shell commands should run before the next decision and ExecCommandBatch is available, prefer it instead of shell separator scripts; otherwise keep one-off commands on ExecCommand.\n\nValid startup examples:\n- `{ \"cmd\": \"rg -n \\\"render_for_model\\\" src\" }`\n- `{ \"cmd\": \"sed -n '1,120p' src/runtime/turn.rs\", \"max_output_tokens\": 1200 }`\n- `{ \"cmd\": \"python -i\", \"tty\": true, \"accepts_input\": true, \"yield_time_ms\": 100 }`\n\nInvalid startup shapes:\n- `{ \"command\": \"rg -n ...\" }` because the field is `cmd`, not `command`\n- `{ \"cmd\": \"cargo test\", \"status\": \"running\" }` because `status` is result/task metadata, not startup input\n- `{ \"cmd\": \"git status\", \"commentary\": \"checking repo\" }` because free-form commentary is not an ExecCommand field\n\nAfter a failed edit or verification command, inspect the relevant failure output once, then make one focused correction. Avoid repeated micro-commands that only move one line at a time or re-check the same nearby slice without new evidence.\n\nKeep startup, immediate result, and promoted-task semantics separate. ExecCommand keeps a structured canonical result with fields such as `disposition`, `exit_status`, bounded previews, truncation flags, artifact refs, and command cost diagnostics, but the command-family tool receipt shown back to the model is rendered as a readable text receipt instead of a raw JSON dump. Command output uses a bounded default budget and per-call `max_output_tokens` is only useful when the next decision truly needs more preview text; artifact refs remain the route for full output. If the command exceeds `yield_time_ms`, Holon promotes it into a `command_task` and returns `disposition=promoted_to_task` plus `task_handle`, `initial_output_preview`, and `initial_output_truncated`. Those are result fields, not valid startup input. When output is truncated, refine the command instead of asking for more of the same wide dump. After promotion, pass `task_handle.task_id` to TaskOutput for waiting or bounded output retrieval and TaskStatus/TaskList for coordination metadata.".to_string(),
        ));
    }
    if names.contains(&"ExecCommandBatch") {
        sections.push(section(
            "tool_exec_command_batch",
            PromptStability::Stable,
            "Use ExecCommandBatch when several bounded shell commands should run before the next decision and do not require interactive input, background task management, or command-task continuation. Each item uses restricted ExecCommand startup fields: `cmd`, optional `workdir`, `shell`, `login`, `yield_time_ms`, and `max_output_tokens`. Do not pass `tty`, `accepts_input`, or `continue_on_result`; call ExecCommand directly for interactive, tty, or long-running supervised commands. ExecCommandBatch runs items sequentially and returns one grouped receipt with per-item status, output previews, truncation flags, command previews, and errors. Use it instead of unstructured shell separator scripts when item boundaries matter, but keep each item command compact and artifact-oriented. Do not use it for edits, arbitrary nested tools, installs, or commands whose later items depend on earlier output unless you intentionally set `stop_on_error` for a bounded sequence.".to_string(),
        ));
    }
    if names.contains(&"ApplyPatch") {
        sections.push(section(
            "tool_apply_patch",
            PromptStability::Stable,
            "Use ApplyPatch as the primary precise file-mutation tool. Use it for focused new files, small local edits, multi-hunk single-file changes, structural edits, and bounded refactors. For very large new files, generated files, whole-file rewrites, bulk deletes, or broad mechanical refactors, choose the lower-context path: split the change into smaller ApplyPatch calls, or use a carefully bounded ExecCommand/scripted rewrite when that avoids emitting a huge diff and is easier to verify. Do not expect separate Write or Edit tools; express ordinary file mutation as unified diff text. On providers that expose ApplyPatch as a freeform grammar tool, send the unified diff body directly and do not wrap it in JSON. On JSON/function fallback providers, ApplyPatch expects exactly `{\"patch\":\"--- a/path\\n+++ b/path\\n@@ -1,1 +1,1 @@\\n-old\\n+new\\n\"}`; do not use `input`. The model-visible ApplyPatch receipt is concise text with action markers like `A`, `M`, `D`, or `R`, while the canonical result still records structured changed-file metadata, `changed_paths`, `changed_file_count`, ignored metadata, diagnostics, and `summary_text`.".to_string(),
        ));
    }
    if names.contains(&"ApplyPatch") {
        sections.push(section(
            "tool_file_mutation",
            PromptStability::Stable,
            "File mutation is workspace-scoped and centered on ApplyPatch. Use unified diff `---`/`+++` file headers and `@@` hunks for deletes, precise edits, and renames. Prefer focused hunks with enough surrounding context to stay unambiguous. Include at least 3 context lines before and after each changed line, and expand to 5–10 lines when the file contains repeated structures or similar patterns. Blank lines within hunks must have a space prefix to be valid context lines. Keep tool output bounded: do not paste enormous malformed patches, do not retry the same large failed patch unchanged, and split large refactors into smaller patch/application steps when that keeps failures recoverable. Avoid using ExecCommand with shell rewrite tricks like `sed -i` as the default editing path, but use a bounded script or heredoc when generating/replacing a large file is cheaper and safer than a huge diff. After any file mutation, rely on the ApplyPatch receipt first, then run focused verification with ExecCommand when correctness matters. Do not use file mutation tools for broad exploration; inspect with shell-first read commands through ExecCommand instead.".to_string(),
        ));
    }
    if names.contains(&"UseWorkspace") {
        sections.push(section(
            "tool_workspace",
            PromptStability::Stable,
            "Workspace is explicit runtime state, not just a shell directory. The active workspace defines the instruction root, execution root, default cwd, workspace-scoped memory/policy boundary, and where ApplyPatch and future local tools operate. Every agent always has exactly one active workspace. `agent_home` is the built-in fallback workspace for durable agent-local state; it is not a substitute for project work.\n\nUse UseWorkspace to make the right workspace active before local file or command work. Call `UseWorkspace({\"path\":\"/repo/or/subdir\"})` when the operator gave you a project path or you need to discover/adopt a directory. Call `UseWorkspace({\"workspace_id\":\"agent_home\"})` to return to AgentHome, or `UseWorkspace({\"workspace_id\":\"ws-...\"})` to switch to a known workspace id from agent state. Provide exactly one of `path` or `workspace_id`. Use `mode=\"isolated\"` only when you need a runtime-managed isolated execution root, and provide an `isolation_label` as an intent/branch hint rather than inventing a worktree path. Use `access_mode=\"exclusive_write\"` when you intend to mutate files; prefer `shared_read` for inspection.\n\nShell `cd` affects only that shell command process. It does not redefine the active workspace, instruction root, or ApplyPatch target root. Switching workspaces does not delete files, remove bindings, or clean up retained isolated roots; cleanup is a separate explicit lifecycle action.".to_string(),
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
        assert!(sections.iter().any(|s| s.name == "tool_spawn_agent"));
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
        assert!(section.content.contains("non-terminal"));
        assert!(section.content.contains("Sleep"));
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
        assert!(sections.iter().any(|s| s.name == "tool_task_control"));
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
        assert!(section
            .content
            .contains("there is no current work item yet"));
        assert!(section
            .content
            .contains("clarify the delivery target with the operator"));
        assert!(section.content.contains("update the plan before execution"));
        assert!(section
            .content
            .contains("exploration or inspection step has served its objective"));
        assert!(section.content.contains("specific blocker or missing fact"));
        assert!(section
            .content
            .contains("instead of silently widening exploration"));
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
        assert!(section.content.contains("open/done"));
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
