//! Tool-specific prompt guidance
//!
//! This module provides registry-style organization for tool usage guidance.
//! Each tool section is emitted only when that tool is available.

// This registry emits only fixed guidance conditioned on the actual tool surface.
use super::{shared_section as section, PromptSection, PromptStability};
use holon_domain::tool_names as tn;
use holon_domain::{ApplyPatchSurface, ToolSpec};

const AGENT_INVOCATION_CORE_GUIDANCE: &str = include_str!("tool_guidance/tool_agent_invocation.md");

const AGENT_DELEGATION_GUIDANCE: &str = "\
Default to completing the task yourself: delegate only for real parallelism, a specialist role/template (load missing skill guides yourself; templates grant no authority; agent review is not human approval), or significant context isolation. Record the concrete benefit, bounded deliverable/acceptance criteria, known evidence, parallel parent work, and shared-file or Cargo conflict ownership; advance independent work rather than duplicating child exploration while retaining responsibility for integration, verification, and child cleanup.";

const AGENT_SUPERVISION_CLI_GUIDANCE: &str = "\
For read-only child progress without waking or polling the target, use bounded `holon tail` (`holon docs agents` for snapshot/follow flags and safe deletion rules); use `TaskStatus` for exact task state, `TaskInput` only after `TaskStatus` confirms input acceptance, and `TaskStop` only for active control. Terminal tasks are historical evidence, not reusable channels.";

const AGENT_SUPERVISION_NO_CLI_GUIDANCE: &str = "\
Use `TaskStatus` (when available) for exact task state, `TaskInput` only after `TaskStatus` confirms input acceptance, and `TaskStop` only for active control; no in-turn blocking output wait is provided, and terminal tasks are historical evidence rather than reusable channels.";

const CREATE_AGENT_GUIDANCE: &str = "\
`CreateAgent` creates an independently managed persistent agent identity without creating a result-bearing task; duplicate creates never deliver the optional bootstrap message or continue prior work.";

const INVOKE_AGENT_GUIDANCE: &str = "\
`InvokeAgent` sends a message and returns `(agent_id, task_handle)` with exactly one target variant: `existing_agent` waits for the first later durable message from that authorized agent (not guaranteed business completion, concurrent waits may observe the same message, and explicit reply expectations require a durable `SendAgentMessage` response), while `new_subagent` spawns a private supervised child with a result-bearing task handle.";

const SEND_AGENT_MESSAGE_GUIDANCE: &str = "\
`SendAgentMessage` sends durable asynchronous communication and returns a delivery receipt, not a task handle or business result. Optional `reply_context_id` selects a runtime-provided original agent-message ID admitted in the current live turn (implicit when unique; required when multiple eligible routes exist; unknown, foreign-turn, or terminal selectors are rejected and supply no sender, correlation, or authority; pure task results create no agent-message route). Set `forward=true` only for an intentional relay of the unique or selected source; unrelated third-party sends stay direct.";

fn build_agent_invocation_guidance(names: &[&str]) -> String {
    let has_create_agent = names.contains(&tn::CREATE_AGENT);
    let has_invoke_agent = names.contains(&tn::INVOKE_AGENT);
    let has_send_agent_message = names.contains(&tn::SEND_AGENT_MESSAGE);
    let has_exec_command = names.contains(&tn::EXEC_COMMAND);

    let mut blocks = Vec::new();
    if has_create_agent || has_invoke_agent {
        blocks.push(AGENT_DELEGATION_GUIDANCE.to_string());
    }
    blocks.push(guidance(AGENT_INVOCATION_CORE_GUIDANCE));
    if has_create_agent {
        blocks.push(CREATE_AGENT_GUIDANCE.to_string());
    }
    if has_invoke_agent {
        blocks.push(INVOKE_AGENT_GUIDANCE.to_string());
    }
    if has_send_agent_message {
        blocks.push(SEND_AGENT_MESSAGE_GUIDANCE.to_string());
    }
    if has_create_agent || has_invoke_agent {
        if has_exec_command {
            blocks.push(AGENT_SUPERVISION_CLI_GUIDANCE.to_string());
        } else {
            blocks.push(AGENT_SUPERVISION_NO_CLI_GUIDANCE.to_string());
        }
    }
    blocks.join("\n\n")
}

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
            build_agent_invocation_guidance(&names),
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
    if names.contains(&tn::CREATE_WORK_ITEM)
        || names.contains(&tn::PICK_WORK_ITEM)
        || names.contains(&tn::UPDATE_WORK_ITEM)
        || names.contains(&tn::COMPLETE_WORK_ITEM)
    {
        let mut transition_tools = vec!["WaitFor"];
        if names.contains(&tn::PICK_WORK_ITEM) {
            transition_tools.push("PickWorkItem");
        }
        transition_tools.push("UpdateWorkItem");
        transition_tools.push("CompleteWorkItem");
        let transition_tools_text = format!(
            "{}, or {}",
            transition_tools[..transition_tools.len() - 1].join(", "),
            transition_tools[transition_tools.len() - 1]
        );
        let pick_work_item_clause = if names.contains(&tn::PICK_WORK_ITEM) {
            "`PickWorkItem` activates an open item and records any runnable caller continuation; "
        } else {
            ""
        };
        sections.push(section(
            "tool_work_item_scheduling",
            PromptStability::Stable,
            guidance_template(
                include_str!("tool_guidance/tool_work_item_scheduling.md"),
                &[("{transition_tools}", &transition_tools_text)],
            ),
        ));
        sections.push(section(
            "tool_work_item_write",
            PromptStability::Stable,
            guidance_template(
                include_str!("tool_guidance/tool_work_item_write.md"),
                &[("{pick_work_item_clause}", pick_work_item_clause)],
            ),
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
        let (patch_language, invocation_contract, format_guidance) = if matches!(
            apply_patch_surface,
            ApplyPatchSurface::CodexDslFreeform
        ) {
            (
                "express ordinary file mutation as Codex DSL with `*** Add File: `, `*** Delete File: `, or `*** Update File: ` hunks",
                "Current ApplyPatch surface is Codex DSL freeform: send raw `*** Begin Patch` / `*** End Patch` text directly. Do not wrap it in JSON, and do not use `patch` or `input` fields.",
                "Use Codex DSL file hunks: `*** Add File: `, `*** Delete File: `, `*** Update File: `, optional `*** Move to: `, and `@@` chunk separators when useful. Prefer focused chunks with unambiguous context, and keep a single space prefix on blank context lines.",
            )
        } else {
            (
                "express ordinary file mutation as unified diff text",
                "Current ApplyPatch surface is a JSON/function tool: call it with exactly `{\"patch\":\"--- a/path\\n+++ b/path\\n@@ -1,1 +1,1 @@\\n-old\\n+new\\n\"}`. Do not use `input`, and do not send a raw freeform diff body.",
                "Use unified diff `---`/`+++` file headers and `@@` hunks: write `--- a/relative/path` for workspace-relative paths and `--- /abs/path` (without `a/`) for explicit absolute paths outside the workspace. Keep `--- a/path` and `+++ b/path` identical unless performing a move with `rename from`/`rename to`, include at least 3 context lines before and after each changed line (5-10 around repeated patterns), and keep a space prefix on blank context lines.",
            )
        };
        sections.push(section(
            "tool_apply_patch",
            PromptStability::Stable,
            guidance_template(
                include_str!("tool_guidance/tool_apply_patch.md"),
                &[
                    ("{patch_language}", patch_language),
                    ("{invocation_contract}", invocation_contract),
                    ("{format_guidance}", format_guidance),
                ],
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
