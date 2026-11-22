//! Tool-specific prompt guidance
//!
//! This module provides registry-style organization for tool usage guidance.
//! Each tool section is emitted only when that tool is available.

// This registry emits only fixed guidance conditioned on the actual tool surface.
use super::{shared_section as section, PromptSection, PromptStability};
use holon_domain::tool_names as tn;
use holon_domain::{ApplyPatchSurface, ToolSpec};

pub const AGENT_INVOCATION_GUIDANCE: &str = include_str!("tool_guidance/tool_agent_invocation.md");

#[derive(Clone, Copy, Debug, Default)]
pub struct ToolPromptContext {
    pub apply_patch_surface: Option<ApplyPatchSurface>,
}

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
    tool_sections_with_context(available_tools, ToolPromptContext::default())
}

pub fn tool_sections_with_context(
    available_tools: &[ToolSpec],
    context: ToolPromptContext,
) -> Vec<PromptSection> {
    let mut sections = Vec::new();
    let names = available_tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>();
    let apply_patch_tool = available_tools
        .iter()
        .find(|tool| tool.name == tn::APPLY_PATCH);

    if names.contains(&tn::WAIT_FOR) {
        sections.push(section(
            "tool_wait_for",
            PromptStability::Stable,
            guidance(include_str!("tool_guidance/tool_wait_for.md")),
        ));
    }
    if names.contains(&tn::CREATE_TIMER)
        || names.contains(&tn::LIST_TIMERS)
        || names.contains(&tn::GET_TIMER)
        || names.contains(&tn::EDIT_TIMER_CONTENT)
        || names.contains(&tn::CANCEL_TIMER)
    {
        sections.push(section(
            "tool_timer",
            PromptStability::Stable,
            guidance(include_str!("tool_guidance/tool_timer.md")),
        ));
    }
    if names.contains(&tn::CREATE_AGENT)
        || names.contains(&tn::INVOKE_AGENT)
        || names.contains(&tn::SEND_AGENT_MESSAGE)
    {
        sections.push(section(
            "tool_agent_invocation",
            PromptStability::Stable,
            guidance(AGENT_INVOCATION_GUIDANCE),
        ));
    }
    if names.contains(&tn::GET_AGENT) {
        sections.push(section(
            "tool_get_agent",
            PromptStability::Stable,
            guidance(include_str!("tool_guidance/tool_get_agent.md")),
        ));
    }
    if names.contains(&tn::ENQUEUE) {
        sections.push(section(
            "tool_enqueue",
            PromptStability::Stable,
            guidance(include_str!("tool_guidance/tool_enqueue.md")),
        ));
    }
    if names.contains(&tn::CREATE_EXTERNAL_TRIGGER) || names.contains(&tn::CANCEL_EXTERNAL_TRIGGER)
    {
        sections.push(section(
            "tool_external_trigger",
            PromptStability::Stable,
            guidance(include_str!("tool_guidance/tool_external_trigger.md")),
        ));
    }
    if names.contains(&"CreateWorkItem")
        || names.contains(&tn::PICK_WORK_ITEM)
        || names.contains(&tn::UPDATE_WORK_ITEM)
        || names.contains(&tn::COMPLETE_WORK_ITEM)
        || names.contains(&tn::GET_WORK_ITEM)
        || names.contains(&tn::LIST_WORK_ITEMS)
    {
        sections.push(section(
            "tool_work_item_scheduling",
            PromptStability::Stable,
            guidance(include_str!("tool_guidance/tool_work_item_scheduling.md")),
        ));
    }
    if names.contains(&tn::CREATE_WORK_ITEM)
        || names.contains(&tn::PICK_WORK_ITEM)
        || names.contains(&tn::UPDATE_WORK_ITEM)
        || names.contains(&tn::COMPLETE_WORK_ITEM)
    {
        sections.push(section(
            "tool_work_item_write",
            PromptStability::Stable,
            guidance(include_str!("tool_guidance/tool_work_item_write.md")),
        ));
    }
    if names.contains(&tn::GET_WORK_ITEM) || names.contains(&tn::LIST_WORK_ITEMS) {
        sections.push(section(
            "tool_work_item_read",
            PromptStability::Stable,
            guidance(include_str!("tool_guidance/tool_work_item_read.md")),
        ));
    }
    if names.contains(&tn::LIST_TASKS)
        || names.contains(&tn::TASK_LIST)
        || names.contains(&tn::TASK_STATUS)
        || names.contains(&tn::TASK_INPUT)
        || names.contains(&tn::TASK_STOP)
    {
        sections.push(section(
            "tool_task_control",
            PromptStability::Stable,
            guidance(include_str!("tool_guidance/tool_task_control.md")),
        ));
    }
    if names.contains(&tn::EXEC_COMMAND) {
        sections.push(section(
            "tool_exec_command",
            PromptStability::Stable,
            guidance(include_str!("tool_guidance/tool_exec_command.md")),
        ));
        sections.push(section(
            "holon_cli_contract",
            PromptStability::Stable,
            guidance(include_str!("tool_guidance/tool_holon_cli.md")),
        ));
    }
    if names.contains(&tn::EXEC_COMMAND_BATCH) {
        sections.push(section(
            "tool_exec_command_batch",
            PromptStability::Stable,
            guidance(include_str!("tool_guidance/tool_exec_command_batch.md")),
        ));
    }
    if let Some(apply_patch_tool) = apply_patch_tool {
        let apply_patch_surface = context.apply_patch_surface.unwrap_or_else(|| {
            if apply_patch_tool.freeform_grammar.is_some() {
                ApplyPatchSurface::CodexDslFreeform
            } else {
                ApplyPatchSurface::UnifiedDiffJson
            }
        });
        let invocation_contract = if matches!(
            apply_patch_surface,
            ApplyPatchSurface::CodexDslFreeform
        ) {
            "Current ApplyPatch surface is Codex DSL freeform: send raw `*** Begin Patch` / `*** End Patch` text directly. Do not wrap it in JSON, and do not use `patch` or `input` fields."
        } else {
            "Current ApplyPatch surface is a JSON/function tool: call it with exactly `{\"patch\":\"--- a/path\\n+++ b/path\\n@@ -1,1 +1,1 @@\\n-old\\n+new\\n\"}`. Do not use `input`, and do not send a raw freeform diff body."
        };
        let patch_language = if matches!(apply_patch_surface, ApplyPatchSurface::CodexDslFreeform) {
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
    if names.contains(&tn::APPLY_PATCH) {
        let apply_patch_surface = context.apply_patch_surface.unwrap_or_else(|| {
            if apply_patch_tool
                .and_then(|tool| tool.freeform_grammar.as_ref())
                .is_some()
            {
                ApplyPatchSurface::CodexDslFreeform
            } else {
                ApplyPatchSurface::UnifiedDiffJson
            }
        });
        let format_guidance = if matches!(apply_patch_surface, ApplyPatchSurface::CodexDslFreeform)
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
    if names.contains(&tn::GET_WORKSPACE_STATE)
        || names.contains(&tn::ATTACH_WORKSPACE)
        || names.contains(&tn::DETACH_WORKSPACE)
        || names.contains(&tn::SWITCH_WORKSPACE)
        || names.contains(&tn::CREATE_WORKTREE)
        || names.contains(&tn::REMOVE_WORKTREE)
        || names.contains(&tn::USE_WORKSPACE)
    {
        sections.push(section(
            "tool_workspace",
            PromptStability::Stable,
            guidance(include_str!("tool_guidance/tool_workspace.md")),
        ));
    }
    sections
}

#[cfg(test)]
mod tests;
