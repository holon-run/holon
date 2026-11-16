use crate::spec::ToolOutcome;
use anyhow::{anyhow, Result};
use holon_domain::{ApplyPatchSurface, ToolCapabilityFamily, ToolSpec};

pub mod advisory_decision;
pub mod apply_patch_tool;
pub mod attach_workspace;
pub mod cancel_external_trigger;
pub mod complete_work_item;
pub mod create_agent;
pub mod create_external_trigger;
pub mod create_work_item;
pub mod create_worktree;
pub mod delete_agent;
pub mod detach_workspace;
pub mod enqueue;
pub mod exec_command;
pub mod exec_command_batch;
pub mod generate_image;
pub mod get_agent;
pub mod get_work_item;
pub mod get_workspace_state;
pub mod invoke_agent;
pub mod list_model_providers;
pub mod list_provider_models;
pub mod list_subagent_cleanup;
pub mod list_work_items;
pub mod memory_get;
pub mod memory_search;
pub mod pick_work_item;
pub mod remove_worktree;
pub mod schedule_destructive_operation;
pub mod semantic_projection;
pub mod send_agent_message;
pub mod sleep;
pub mod switch_workspace;
pub mod task_input;
pub mod task_list;
pub mod task_status;
pub mod task_stop;
pub mod timer;
pub mod update_work_item;
pub mod use_workspace;
pub mod view_image;
pub mod wait_for;
pub mod web_fetch;
pub mod web_search;
pub mod work_item_action;
pub mod work_item_query;
pub mod x_search;

pub struct BuiltinToolDefinition {
    pub family: ToolCapabilityFamily,
    pub spec: ToolSpec,
}

pub struct ToolModelRenderContext<'a> {
    pub tool_execution_id: &'a str,
    pub tool_output_budget_estimated_tokens: usize,
}

pub fn builtin_tool_definitions() -> Result<Vec<BuiltinToolDefinition>> {
    Ok(vec![
        advisory_decision::definition()?,
        sleep::definition()?,
        wait_for::definition()?,
        timer::create_definition()?,
        timer::list_definition()?,
        timer::get_definition()?,
        timer::cancel_definition()?,
        get_agent::definition()?,
        delete_agent::definition()?,
        list_subagent_cleanup::definition()?,
        enqueue::definition()?,
        create_agent::definition()?,
        send_agent_message::definition()?,
        invoke_agent::definition()?,
        task_list::definition()?,
        task_list::legacy_definition()?,
        task_status::definition()?,
        task_input::definition()?,
        task_stop::definition()?,
        list_model_providers::definition()?,
        list_provider_models::definition()?,
        create_work_item::definition()?,
        pick_work_item::definition()?,
        get_work_item::definition()?,
        generate_image::definition()?,
        list_work_items::definition()?,
        update_work_item::definition()?,
        complete_work_item::definition()?,
        memory_search::definition()?,
        memory_get::definition()?,
        get_workspace_state::definition()?,
        attach_workspace::definition()?,
        detach_workspace::definition()?,
        switch_workspace::definition()?,
        create_worktree::definition()?,
        remove_worktree::definition()?,
        schedule_destructive_operation::definition()?,
        apply_patch_tool::definition()?,
        exec_command::definition()?,
        exec_command_batch::definition()?,
        use_workspace::definition()?,
        view_image::definition()?,
        web_fetch::definition()?,
        web_search::definition()?,
        x_search::definition()?,
    ])
}

pub fn builtin_tool_definitions_for_apply_patch_surface(
    surface: ApplyPatchSurface,
) -> Result<Vec<BuiltinToolDefinition>> {
    builtin_tool_definitions()?
        .into_iter()
        .map(|definition| {
            if definition.spec.name == apply_patch_tool::NAME {
                apply_patch_tool::definition_for_surface(surface)
            } else {
                Ok(definition)
            }
        })
        .collect()
}

pub fn estimated_tokens(text: &str) -> usize {
    text.chars().count().saturating_add(3) / 4
}

pub fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut truncated = value
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    truncated.push('…');
    truncated
}

/// An executable built-in tool route, including compatibility aliases.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuiltinTool {
    AdvisoryDecision,
    Sleep,
    WaitFor,
    CreateTimer,
    ListTimer,
    GetTimer,
    CancelTimer,
    DeleteAgent,
    GetAgent,
    ListSubagentCleanup,
    Enqueue,
    CreateAgent,
    SendAgentMessage,
    InvokeAgent,
    TaskList,
    TaskListLegacy,
    TaskStatus,
    TaskInput,
    TaskStop,
    ListModelProviders,
    ListProviderModels,
    CreateWorkItem,
    PickWorkItem,
    GetWorkItem,
    GenerateImage,
    ListWorkItems,
    UpdateWorkItem,
    CompleteWorkItem,
    MemorySearch,
    MemoryGet,
    GetWorkspaceState,
    AttachWorkspace,
    DetachWorkspace,
    SwitchWorkspace,
    CreateWorktree,
    RemoveWorktree,
    ScheduleDestructiveOperation,
    CreateExternalTrigger,
    CancelExternalTrigger,
    ApplyPatchTool,
    ExecCommand,
    ExecCommandBatch,
    UseWorkspace,
    ViewImage,
    WebFetch,
    WebSearch,
    XSearch,
}

impl BuiltinTool {
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            advisory_decision::NAME => Self::AdvisoryDecision,
            sleep::NAME => Self::Sleep,
            wait_for::NAME => Self::WaitFor,
            timer::CREATE_NAME => Self::CreateTimer,
            timer::LIST_NAME => Self::ListTimer,
            timer::GET_NAME => Self::GetTimer,
            timer::CANCEL_NAME => Self::CancelTimer,
            delete_agent::NAME => Self::DeleteAgent,
            get_agent::NAME => Self::GetAgent,
            list_subagent_cleanup::NAME => Self::ListSubagentCleanup,
            enqueue::NAME => Self::Enqueue,
            create_agent::NAME => Self::CreateAgent,
            send_agent_message::NAME => Self::SendAgentMessage,
            invoke_agent::NAME => Self::InvokeAgent,
            task_list::NAME => Self::TaskList,
            task_list::LEGACY_NAME => Self::TaskListLegacy,
            task_status::NAME => Self::TaskStatus,
            task_input::NAME => Self::TaskInput,
            task_stop::NAME => Self::TaskStop,
            list_model_providers::NAME => Self::ListModelProviders,
            list_provider_models::NAME => Self::ListProviderModels,
            create_work_item::NAME => Self::CreateWorkItem,
            pick_work_item::NAME => Self::PickWorkItem,
            get_work_item::NAME => Self::GetWorkItem,
            generate_image::NAME => Self::GenerateImage,
            list_work_items::NAME => Self::ListWorkItems,
            update_work_item::NAME => Self::UpdateWorkItem,
            complete_work_item::NAME => Self::CompleteWorkItem,
            memory_search::NAME => Self::MemorySearch,
            memory_get::NAME => Self::MemoryGet,
            get_workspace_state::NAME => Self::GetWorkspaceState,
            attach_workspace::NAME => Self::AttachWorkspace,
            detach_workspace::NAME => Self::DetachWorkspace,
            switch_workspace::NAME => Self::SwitchWorkspace,
            create_worktree::NAME => Self::CreateWorktree,
            remove_worktree::NAME => Self::RemoveWorktree,
            schedule_destructive_operation::NAME => Self::ScheduleDestructiveOperation,
            create_external_trigger::NAME => Self::CreateExternalTrigger,
            cancel_external_trigger::NAME => Self::CancelExternalTrigger,
            apply_patch_tool::NAME => Self::ApplyPatchTool,
            exec_command::NAME => Self::ExecCommand,
            exec_command_batch::NAME => Self::ExecCommandBatch,
            use_workspace::NAME => Self::UseWorkspace,
            view_image::NAME => Self::ViewImage,
            web_fetch::NAME => Self::WebFetch,
            web_search::NAME => Self::WebSearch,
            x_search::NAME => Self::XSearch,
            _ => return None,
        })
    }
}

pub struct ToolRenderHooks<State> {
    pub image: fn(&ToolOutcome<State>) -> Result<String>,
    pub workspace: fn(&ToolOutcome<State>, &ToolModelRenderContext<'_>) -> Result<String>,
}

pub fn render_tool_result_for_model(result: &ToolOutcome<()>) -> Result<String> {
    render_tool_result_for_model_with_hooks(result, &BUILTIN_RENDER_HOOKS)
}

pub fn render_tool_result_for_model_with_context(
    result: &ToolOutcome<()>,
    context: &ToolModelRenderContext<'_>,
) -> Result<String> {
    render_tool_result_with_context_and_hooks(result, context, &BUILTIN_RENDER_HOOKS)
}

const BUILTIN_RENDER_HOOKS: ToolRenderHooks<()> = ToolRenderHooks {
    image: generate_image::render_for_model,
    workspace: get_workspace_state::render_for_model,
};

pub fn render_tool_result_for_model_with_hooks<State: Default>(
    result: &ToolOutcome<State>,
    hooks: &ToolRenderHooks<State>,
) -> Result<String> {
    let rendered = render_tool_result_for_model_base(result, hooks)?;
    Ok(if uses_custom_model_renderer(result) {
        append_input_coercion_notice(rendered, result)
    } else {
        rendered
    })
}

fn render_tool_result_for_model_base<State: Default>(
    result: &ToolOutcome<State>,
    hooks: &ToolRenderHooks<State>,
) -> Result<String> {
    if result.is_error() {
        let error = result
            .tool_error()
            .ok_or_else(|| anyhow!("tool error result missing error payload"))?;
        return Ok(error.render_for_model(Some(&result.envelope.tool_name)));
    }

    match result.envelope.tool_name.as_str() {
        apply_patch_tool::NAME => apply_patch_tool::render_for_model(result),
        exec_command::NAME => exec_command::render_for_model(result),
        exec_command_batch::NAME => exec_command_batch::render_for_model(result),
        generate_image::NAME => (hooks.image)(result),
        view_image::NAME => view_image::render_for_model(result),
        _ => result.content_text(),
    }
}

pub fn render_tool_result_with_context_and_hooks<State: Default>(
    result: &ToolOutcome<State>,
    context: &ToolModelRenderContext<'_>,
    hooks: &ToolRenderHooks<State>,
) -> Result<String> {
    let custom_workspace_render =
        result.envelope.tool_name == get_workspace_state::NAME && !result.is_error();
    let rendered = if custom_workspace_render {
        (hooks.workspace)(result, context)?
    } else {
        render_tool_result_for_model_base(result, hooks)?
    };
    let rendered = if custom_workspace_render || uses_custom_model_renderer(result) {
        append_input_coercion_notice(rendered, result)
    } else {
        rendered
    };
    if result.envelope.tool_name != list_work_items::NAME
        && estimated_tokens(&rendered) <= context.tool_output_budget_estimated_tokens
    {
        return Ok(rendered);
    }

    let output_ref = format!("tool_execution:{}:output", context.tool_execution_id);
    semantic_projection::project(
        &result.envelope,
        Some(&output_ref),
        context.tool_output_budget_estimated_tokens,
    )
    .ok_or_else(|| {
        anyhow!(
            "tool output budget cannot accommodate a useful {} receipt",
            result.envelope.tool_name
        )
    })
}

fn uses_custom_model_renderer<State: Default>(result: &ToolOutcome<State>) -> bool {
    result.is_error()
        || matches!(
            result.envelope.tool_name.as_str(),
            apply_patch_tool::NAME
                | exec_command::NAME
                | exec_command_batch::NAME
                | generate_image::NAME
                | view_image::NAME
        )
}

fn append_input_coercion_notice<State: Default>(
    mut rendered: String,
    result: &ToolOutcome<State>,
) -> String {
    let Some(crate::tool::spec::ToolInputCoercion::UnwrapToolInputEnvelope {
        envelope_key,
        outer_keys,
        inner_keys,
    }) = &result.envelope.input_coercion
    else {
        return rendered;
    };
    rendered.push_str(&format!(
        "\n\nInput recovery: unwrapped top-level `{envelope_key}` tool-input envelope; outer keys: {}; inner keys: {}.",
        outer_keys.join(", "),
        inner_keys.join(", "),
    ));
    rendered
}
