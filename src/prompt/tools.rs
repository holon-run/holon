//! Tool-specific prompt guidance
//!
//! This module provides registry-style organization for tool usage guidance.
//! Each tool section is emitted only when that tool is available.

use super::{section, PromptSection, PromptStability};
use crate::tool::ToolSpec;

fn guidance(content: &'static str) -> String {
    content
        .replace("\r\n", "\n")
        .trim_end_matches('\n')
        .to_string()
}

fn guidance_template(content: &'static str, replacements: &[(&str, &str)]) -> String {
    let mut rendered = guidance(content);
    for (placeholder, replacement) in replacements {
        rendered = rendered.replace(placeholder, replacement);
    }
    rendered
}

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

    if names.contains(&"WaitFor") {
        sections.push(section(
            "tool_wait_for",
            PromptStability::Stable,
            guidance(include_str!("tool_guidance/tool_wait_for.md")),
        ));
    }
    if names.contains(&"SpawnAgent") {
        sections.push(section(
            "tool_spawn_agent",
            PromptStability::Stable,
            guidance(include_str!("tool_guidance/tool_spawn_agent.md")),
        ));
    }
    if names.contains(&"AgentGet") {
        sections.push(section(
            "tool_agent_get",
            PromptStability::Stable,
            guidance(include_str!("tool_guidance/tool_agent_get.md")),
        ));
    }
    if names.contains(&"Enqueue") {
        sections.push(section(
            "tool_enqueue",
            PromptStability::Stable,
            guidance(include_str!("tool_guidance/tool_enqueue.md")),
        ));
    }
    if names.contains(&"CreateExternalTrigger") || names.contains(&"CancelExternalTrigger") {
        sections.push(section(
            "tool_external_trigger",
            PromptStability::Stable,
            guidance(include_str!("tool_guidance/tool_external_trigger.md")),
        ));
    }
    if names.contains(&"CreateWorkItem")
        || names.contains(&"PickWorkItem")
        || names.contains(&"UpdateWorkItem")
        || names.contains(&"CompleteWorkItem")
        || names.contains(&"GetWorkItem")
        || names.contains(&"ListWorkItems")
    {
        sections.push(section(
            "tool_work_item_scheduling",
            PromptStability::Stable,
            guidance(include_str!("tool_guidance/tool_work_item_scheduling.md")),
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
            guidance(include_str!("tool_guidance/tool_work_item_write.md")),
        ));
    }
    if names.contains(&"GetWorkItem") || names.contains(&"ListWorkItems") {
        sections.push(section(
            "tool_work_item_read",
            PromptStability::Stable,
            guidance(include_str!("tool_guidance/tool_work_item_read.md")),
        ));
    }
    if names.contains(&"ListTasks")
        || names.contains(&"TaskList")
        || names.contains(&"TaskStatus")
        || names.contains(&"TaskInput")
        || names.contains(&"TaskOutput")
        || names.contains(&"TaskStop")
    {
        sections.push(section(
            "tool_task_control",
            PromptStability::Stable,
            guidance(include_str!("tool_guidance/tool_task_control.md")),
        ));
    }
    if names.contains(&"ExecCommand") {
        sections.push(section(
            "tool_exec_command",
            PromptStability::Stable,
            guidance(include_str!("tool_guidance/tool_exec_command.md")),
        ));
    }
    if names.contains(&"ExecCommandBatch") {
        sections.push(section(
            "tool_exec_command_batch",
            PromptStability::Stable,
            guidance(include_str!("tool_guidance/tool_exec_command_batch.md")),
        ));
    }
    if let Some(apply_patch_tool) = apply_patch_tool {
        let invocation_contract = if apply_patch_tool.freeform_grammar.is_some() {
            "Current ApplyPatch surface is Codex DSL freeform: send raw `*** Begin Patch` / `*** End Patch` text directly. Do not wrap it in JSON, and do not use `patch` or `input` fields."
        } else {
            "Current ApplyPatch surface is a JSON/function tool: call it with exactly `{\"patch\":\"--- a/path\\n+++ b/path\\n@@ -1,1 +1,1 @@\\n-old\\n+new\\n\"}`. Do not use `input`, and do not send a raw freeform diff body."
        };
        let patch_language = if apply_patch_tool.freeform_grammar.is_some() {
            "express ordinary file mutation as Codex DSL with `*** Add File`, `*** Delete File`, or `*** Update File` hunks"
        } else {
            "express ordinary file mutation as unified diff text"
        };
        sections.push(section(
            "tool_apply_patch",
            PromptStability::Stable,
            guidance_template(
                include_str!("tool_guidance/tool_apply_patch.md"),
                &[
                    ("{patch_language}", patch_language),
                    ("{invocation_contract}", invocation_contract),
                ],
            ),
        ));
    }
    if names.contains(&"ApplyPatch") {
        let format_guidance = if apply_patch_tool
            .and_then(|tool| tool.freeform_grammar.as_ref())
            .is_some()
        {
            "Use Codex DSL file hunks: `*** Add File`, `*** Delete File`, `*** Update File`, optional `*** Move to`, and `@@` chunk separators when useful. Prefer focused chunks with enough surrounding context to stay unambiguous. Blank context lines within update chunks must have a space prefix."
        } else {
            "Use unified diff `---`/`+++` file headers and `@@` hunks for deletes, precise edits, and renames. For ordinary edits, the `--- a/path` and `+++ b/path` headers must refer to the same path; different paths are treated as a rename/move and require explicit `rename from` and `rename to` headers. Do not put old/new prose text in `---`/`+++` header lines; those lines must contain file paths. Prefer focused hunks with enough surrounding context to stay unambiguous. Include at least 3 context lines before and after each changed line, and expand to 5-10 lines when the file contains repeated structures or similar patterns. Blank lines within hunks must have a space prefix to be valid context lines."
        };
        sections.push(section(
            "tool_file_mutation",
            PromptStability::Stable,
            guidance_template(
                include_str!("tool_guidance/tool_file_mutation.md"),
                &[("{format_guidance}", format_guidance)],
            ),
        ));
    }
    if names.contains(&"UseWorkspace") {
        sections.push(section(
            "tool_workspace",
            PromptStability::Stable,
            guidance(include_str!("tool_guidance/tool_workspace.md")),
        ));
    }
    sections
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
            .contains("call WaitFor with wake=task_result"));
        assert!(section.content.contains("resource set to the task id"));
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
        assert!(section.content.contains("Use WaitFor, not UpdateWorkItem"));
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
        assert!(section.content.contains("specific wait with WaitFor"));
        assert!(section
            .content
            .contains("instead of silently widening exploration"));
        assert!(section
            .content
            .contains("audit whether the acceptance evidence is present"));
    }

    #[test]
    fn test_work_item_scheduling_section_emitted_when_available() {
        let tools = vec![ToolSpec {
            name: "UpdateWorkItem".into(),
            description: String::new(),
            input_schema: json!({}),
            freeform_grammar: None,
        }];
        let sections = tool_sections(&tools);
        let section = sections
            .iter()
            .find(|s| s.name == "tool_work_item_scheduling")
            .expect("work item scheduling section");
        assert!(section.content.contains("open runnable WorkItem"));
        assert!(section.content.contains("system tick"));
        assert!(section.content.contains("Ending the current response"));
        assert!(section
            .content
            .contains("does not change WorkItem readiness"));
        assert!(section.content.contains("Use WaitFor to record"));
        assert!(section.content.contains("operator_input waits"));
        assert!(section
            .content
            .contains("Keep runnable WorkItems only for work that is actually ready"));
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
            .contains("derived from scheduler readiness plus current focus"));
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
            .contains("Do not repeat the same read command"));
        assert!(section
            .content
            .contains("refresh only the smallest relevant slice"));
        assert!(section
            .content
            .contains("After a successful command creates, rewrites, formats, or generates files"));
        assert!(section
            .content
            .contains("rely on the command receipt first"));
        assert!(section
            .content
            .contains("several short, bounded shell commands"));
        assert!(section
            .content
            .contains("long-running, or uncertain-runtime"));
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
            .contains("for ordinary completion waiting, call WaitFor"));
        assert!(section.content.contains("resource set to the task id"));
        assert!(section
            .content
            .contains("multiple independent long-running commands"));
        assert!(section
            .content
            .contains("each command needs its own task handle"));
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
        assert!(section.content.contains("short, bounded shell commands"));
        assert!(section.content.contains("runs items sequentially"));
        assert!(section.content.contains("Do not pass `tty`"));
        assert!(section.content.contains("defaults to 30_000 ms"));
        assert!(section.content.contains("Top-level `workdir`"));
        assert!(section
            .content
            .contains("does not promote timed-out items into managed command tasks"));
        assert!(section.content.contains("omit it by default"));
        assert!(section.content.contains("grouped receipt"));
        assert!(section
            .content
            .contains("long-running, has uncertain runtime"));
        assert!(section
            .content
            .contains("For multiple independent long-running commands"));
        assert!(section.content.contains("shell-level `&`"));
        assert!(section
            .content
            .contains("call WaitFor with `reason`, `wake=task_result`, and `resource=<task_id>`"));
        assert!(section.content.contains("rather than polling"));
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
            .contains("Current ApplyPatch surface is Codex DSL freeform"));
        let section = sections
            .iter()
            .find(|s| s.name == "tool_file_mutation")
            .expect("file mutation section");
        assert!(section.content.contains("centered on ApplyPatch"));
        assert!(section
            .content
            .contains("Relative patch paths resolve from the active workspace"));
        assert!(section
            .content
            .contains("explicit absolute paths are filesystem targets"));
        assert!(section.content.contains("sed -i"));
        assert!(section
            .content
            .contains("do not retry the same large failed patch unchanged"));
        assert!(section.content.contains("bounded script or heredoc"));
        assert!(section
            .content
            .contains("do not re-read the same file merely to confirm"));
        assert!(section
            .content
            .contains("read only the smallest relevant slice"));
        assert!(section.content.contains("non-clean receipt"));
        assert!(section
            .content
            .contains("inspect the exact affected region"));
        assert!(section
            .content
            .contains("formatter, script, command, or user edit"));
        assert!(section.content.contains("For ordinary edits"));
        assert!(section.content.contains("must refer to the same path"));
        assert!(section
            .content
            .contains("different paths are treated as a rename/move"));
        assert!(section.content.contains("rename from"));
        assert!(section.content.contains("rename to"));
        assert!(section.content.contains("Do not put old/new prose text"));
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
            .contains("Current ApplyPatch surface is Codex DSL freeform"));
        assert!(apply_patch
            .content
            .contains("send raw `*** Begin Patch` / `*** End Patch` text directly"));
        assert!(apply_patch
            .content
            .contains("do not use `patch` or `input` fields"));
        assert!(!apply_patch
            .content
            .contains("Current ApplyPatch surface is a JSON/function tool"));
        let section = sections
            .iter()
            .find(|s| s.name == "tool_file_mutation")
            .expect("file mutation section");
        assert!(section.content.contains("Use Codex DSL file hunks"));
        assert!(!section.content.contains("For ordinary edits"));
        assert!(!section.content.contains("Do not put old/new prose text"));
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
        assert!(section
            .content
            .contains("default long-lived project context"));
        assert!(section
            .content
            .contains("the base for relative ApplyPatch paths"));
        assert!(section
            .content
            .contains("not a global prohibition against explicit filesystem targets"));
        assert!(section.content.contains("agent_home"));
        assert!(section
            .content
            .contains("Shell `cd` affects only that shell command"));
        assert!(section.content.contains("AGENTS.md loading scope"));
        assert!(section.content.contains("relative ApplyPatch base"));
        assert!(section.content.contains("UseWorkspace"));
    }

    #[test]
    fn test_multiple_tool_sections_emitted() {
        let tools = vec![
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
        assert!(sections.iter().all(|s| s.name != "tool_sleep"));
        assert!(sections.iter().any(|s| s.name == "tool_task_control"));
        assert!(sections.iter().any(|s| s.name == "tool_exec_command"));
        assert!(sections.iter().any(|s| s.name == "tool_apply_patch"));
        assert!(sections.iter().any(|s| s.name == "tool_file_mutation"));
    }

    #[test]
    fn test_migrated_tool_guidance_sections_are_non_empty() {
        let tools = vec![
            ToolSpec {
                name: "WaitFor".into(),
                description: String::new(),
                input_schema: json!({}),
                freeform_grammar: None,
            },
            ToolSpec {
                name: "SpawnAgent".into(),
                description: String::new(),
                input_schema: json!({}),
                freeform_grammar: None,
            },
            ToolSpec {
                name: "AgentGet".into(),
                description: String::new(),
                input_schema: json!({}),
                freeform_grammar: None,
            },
            ToolSpec {
                name: "Enqueue".into(),
                description: String::new(),
                input_schema: json!({}),
                freeform_grammar: None,
            },
            ToolSpec {
                name: "CreateExternalTrigger".into(),
                description: String::new(),
                input_schema: json!({}),
                freeform_grammar: None,
            },
            ToolSpec {
                name: "CreateWorkItem".into(),
                description: String::new(),
                input_schema: json!({}),
                freeform_grammar: None,
            },
            ToolSpec {
                name: "GetWorkItem".into(),
                description: String::new(),
                input_schema: json!({}),
                freeform_grammar: None,
            },
            ToolSpec {
                name: "TaskStatus".into(),
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
                name: "ExecCommandBatch".into(),
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
            ToolSpec {
                name: "UseWorkspace".into(),
                description: String::new(),
                input_schema: json!({}),
                freeform_grammar: None,
            },
        ];
        let sections = tool_sections(&tools);
        for name in [
            "tool_wait_for",
            "tool_spawn_agent",
            "tool_agent_get",
            "tool_enqueue",
            "tool_external_trigger",
            "tool_work_item_scheduling",
            "tool_work_item_write",
            "tool_work_item_read",
            "tool_task_control",
            "tool_exec_command",
            "tool_exec_command_batch",
            "tool_apply_patch",
            "tool_file_mutation",
            "tool_workspace",
        ] {
            let section = sections
                .iter()
                .find(|s| s.name == name)
                .unwrap_or_else(|| panic!("{name} section"));
            assert!(!section.content.trim().is_empty());
        }
    }

    #[test]
    fn test_guidance_normalizes_line_endings() {
        assert_eq!(guidance("one\r\ntwo\r\n"), "one\ntwo");
        assert_eq!(guidance("one\ntwo\n\n"), "one\ntwo");
    }

    #[test]
    fn test_empty_tools_returns_empty_sections() {
        let sections = tool_sections(&[]);
        assert!(sections.is_empty());
    }
}
