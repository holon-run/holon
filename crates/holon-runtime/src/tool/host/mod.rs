use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::Value;
use std::{future::Future, pin::Pin};

use crate::{
    runtime::RuntimeHandle,
    tool::{spec::ToolExecutionContext, ToolCall, ToolResult},
    types::AuthorityClass,
};

pub(crate) mod advisory_decision;
pub(crate) mod apply_patch_tool;
pub(crate) mod attach_workspace;
pub(crate) mod cancel_external_trigger;
pub(crate) mod complete_work_item;
pub(crate) mod create_agent;
pub(crate) mod create_external_trigger;
pub(crate) mod create_work_item;
pub(crate) mod create_worktree;
pub(crate) mod delete_agent;
pub(crate) mod detach_workspace;
pub(crate) mod enqueue;
pub(crate) mod exec_command;
pub(crate) mod exec_command_batch;
pub(crate) mod generate_image;
pub(crate) mod get_agent;
pub(crate) mod get_work_item;
pub(crate) mod get_workspace_state;
pub(crate) mod invoke_agent;
pub(crate) mod list_model_providers;
pub(crate) mod list_provider_models;
pub(crate) mod list_subagents;
pub(crate) mod list_work_items;
pub(crate) mod memory_get;
pub(crate) mod memory_search;
pub(crate) mod pick_work_item;
pub(crate) mod remove_worktree;
pub(crate) mod schedule_destructive_operation;
pub(crate) mod semantic_projection;
pub(crate) mod send_agent_message;
pub(crate) mod sleep;
pub(crate) mod switch_workspace;
pub(crate) mod task_input;
pub(crate) mod task_list;
pub(crate) mod task_status;
pub(crate) mod task_stop;
pub(crate) mod timer;
pub(crate) mod update_work_item;
pub(crate) mod use_workspace;
pub(crate) mod view_image;
pub(crate) mod wait_for;
pub(crate) mod web_fetch;
pub(crate) mod web_search;
pub(crate) mod work_item_action;
pub(crate) mod work_item_query;
pub(crate) mod x_search;

pub(crate) use holon_tools::builtins::ToolModelRenderContext;

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn execute_builtin_tool<'a>(
    runtime: &'a RuntimeHandle,
    agent_id: &'a str,
    authority_class: &'a AuthorityClass,
    call: &'a ToolCall,
) -> Pin<Box<dyn Future<Output = Result<ToolResult>> + Send + 'a>> {
    Box::pin(async move {
        let context = ToolExecutionContext::default();
        execute_builtin_tool_with_context(runtime, agent_id, authority_class, call, &context).await
    })
}

pub(crate) fn execute_builtin_tool_with_context<'a>(
    runtime: &'a RuntimeHandle,
    agent_id: &'a str,
    authority_class: &'a AuthorityClass,
    call: &'a ToolCall,
    context: &'a ToolExecutionContext,
) -> Pin<Box<dyn Future<Output = Result<ToolResult>> + Send + 'a>> {
    match holon_tools::builtins::BuiltinTool::from_name(&call.name) {
        Some(tool) => {
            execute_builtin_tool_route(runtime, tool, agent_id, authority_class, call, context)
        }
        None => Box::pin(async move { Err(anyhow!("unknown builtin tool {}", call.name)) }),
    }
}

pub(super) fn execute_builtin_tool_route<'a>(
    runtime: &'a RuntimeHandle,
    tool: holon_tools::builtins::BuiltinTool,
    agent_id: &'a str,
    authority_class: &'a AuthorityClass,
    call: &'a ToolCall,
    context: &'a ToolExecutionContext,
) -> Pin<Box<dyn Future<Output = Result<ToolResult>> + Send + 'a>> {
    macro_rules! defer_tool_future {
        ($future:expr) => {
            Box::pin(async move { Box::pin($future).await })
        };
    }

    let future: Pin<Box<dyn Future<Output = Result<ToolResult>> + Send + 'a>> =
        match tool {
            holon_tools::builtins::BuiltinTool::AdvisoryDecision => {
                Box::pin(advisory_decision::execute(
                    runtime,
                    agent_id,
                    authority_class,
                    &call.input,
                    context,
                ))
            }
            holon_tools::builtins::BuiltinTool::Sleep => Box::pin(sleep::execute(
                runtime,
                agent_id,
                authority_class,
                &call.input,
            )),
            holon_tools::builtins::BuiltinTool::WaitFor => Box::pin(wait_for::execute(
                runtime,
                agent_id,
                authority_class,
                &call.input,
                context,
            )),
            holon_tools::builtins::BuiltinTool::CreateTimer => {
                Box::pin(timer::create(runtime, &call.input))
            }
            holon_tools::builtins::BuiltinTool::ListTimer => {
                Box::pin(timer::list(runtime, &call.input))
            }
            holon_tools::builtins::BuiltinTool::GetTimer => {
                Box::pin(timer::get(runtime, &call.input))
            }
            holon_tools::builtins::BuiltinTool::CancelTimer => {
                Box::pin(timer::cancel(runtime, &call.input))
            }
            holon_tools::builtins::BuiltinTool::DeleteAgent => {
                Box::pin(delete_agent::execute(runtime, &call.input))
            }
            holon_tools::builtins::BuiltinTool::GetAgent => {
                Box::pin(get_agent::execute(runtime, &call.input))
            }
            holon_tools::builtins::BuiltinTool::ListSubagents => {
                Box::pin(list_subagents::execute(runtime, &call.input))
            }
            holon_tools::builtins::BuiltinTool::Enqueue => Box::pin(enqueue::execute(
                runtime,
                agent_id,
                authority_class,
                &call.input,
            )),
            holon_tools::builtins::BuiltinTool::CreateAgent => Box::pin(create_agent::execute(
                runtime,
                agent_id,
                authority_class,
                &call.input,
            )),
            holon_tools::builtins::BuiltinTool::SendAgentMessage => Box::pin(
                send_agent_message::execute(runtime, authority_class, &call.id, &call.input),
            ),
            holon_tools::builtins::BuiltinTool::InvokeAgent => Box::pin(invoke_agent::execute(
                runtime,
                agent_id,
                authority_class,
                &call.input,
            )),
            holon_tools::builtins::BuiltinTool::TaskList => Box::pin(task_list::execute(
                runtime,
                agent_id,
                authority_class,
                &call.input,
            )),
            holon_tools::builtins::BuiltinTool::TaskListLegacy => Box::pin(
                task_list::execute_legacy(runtime, agent_id, authority_class, &call.input),
            ),
            holon_tools::builtins::BuiltinTool::TaskStatus => Box::pin(task_status::execute(
                runtime,
                agent_id,
                authority_class,
                &call.input,
            )),
            holon_tools::builtins::BuiltinTool::TaskInput => Box::pin(task_input::execute(
                runtime,
                agent_id,
                authority_class,
                &call.input,
            )),
            holon_tools::builtins::BuiltinTool::TaskStop => Box::pin(task_stop::execute(
                runtime,
                agent_id,
                authority_class,
                &call.input,
            )),
            holon_tools::builtins::BuiltinTool::ListModelProviders => Box::pin(
                list_model_providers::execute(runtime, agent_id, authority_class, &call.input),
            ),
            holon_tools::builtins::BuiltinTool::ListProviderModels => Box::pin(
                list_provider_models::execute(runtime, agent_id, authority_class, &call.input),
            ),
            holon_tools::builtins::BuiltinTool::CreateWorkItem => Box::pin(
                create_work_item::execute(runtime, agent_id, authority_class, &call.input),
            ),
            holon_tools::builtins::BuiltinTool::PickWorkItem => Box::pin(pick_work_item::execute(
                runtime,
                agent_id,
                authority_class,
                &call.input,
            )),
            holon_tools::builtins::BuiltinTool::GetWorkItem => Box::pin(get_work_item::execute(
                runtime,
                agent_id,
                authority_class,
                &call.input,
            )),
            holon_tools::builtins::BuiltinTool::GenerateImage => Box::pin(generate_image::execute(
                runtime,
                agent_id,
                authority_class,
                &call.input,
            )),
            holon_tools::builtins::BuiltinTool::ListWorkItems => Box::pin(
                list_work_items::execute(runtime, agent_id, authority_class, &call.input),
            ),
            holon_tools::builtins::BuiltinTool::UpdateWorkItem => Box::pin(
                update_work_item::execute(runtime, agent_id, authority_class, &call.input),
            ),
            holon_tools::builtins::BuiltinTool::CompleteWorkItem => {
                defer_tool_future!(complete_work_item::execute(
                    runtime,
                    agent_id,
                    authority_class,
                    &call.input,
                    context,
                ))
            }
            holon_tools::builtins::BuiltinTool::MemorySearch => Box::pin(memory_search::execute(
                runtime,
                agent_id,
                authority_class,
                &call.input,
            )),
            holon_tools::builtins::BuiltinTool::MemoryGet => Box::pin(memory_get::execute(
                runtime,
                agent_id,
                authority_class,
                &call.input,
            )),
            holon_tools::builtins::BuiltinTool::GetWorkspaceState => Box::pin(
                get_workspace_state::execute(runtime, agent_id, authority_class, &call.input),
            ),
            holon_tools::builtins::BuiltinTool::AttachWorkspace => Box::pin(
                attach_workspace::execute(runtime, agent_id, authority_class, &call.input),
            ),
            holon_tools::builtins::BuiltinTool::DetachWorkspace => Box::pin(
                detach_workspace::execute(runtime, agent_id, authority_class, &call.input),
            ),
            holon_tools::builtins::BuiltinTool::SwitchWorkspace => Box::pin(
                switch_workspace::execute(runtime, agent_id, authority_class, &call.input),
            ),
            holon_tools::builtins::BuiltinTool::CreateWorktree => Box::pin(
                create_worktree::execute(runtime, agent_id, authority_class, &call.input),
            ),
            holon_tools::builtins::BuiltinTool::RemoveWorktree => Box::pin(
                remove_worktree::execute(runtime, agent_id, authority_class, &call.input),
            ),
            holon_tools::builtins::BuiltinTool::ScheduleDestructiveOperation => {
                Box::pin(schedule_destructive_operation::execute(
                    runtime,
                    agent_id,
                    authority_class,
                    &call.input,
                    context,
                ))
            }
            holon_tools::builtins::BuiltinTool::CreateExternalTrigger => Box::pin(
                create_external_trigger::execute(runtime, agent_id, authority_class, &call.input),
            ),
            holon_tools::builtins::BuiltinTool::CancelExternalTrigger => Box::pin(
                cancel_external_trigger::execute(runtime, agent_id, authority_class, &call.input),
            ),
            holon_tools::builtins::BuiltinTool::ApplyPatchTool => Box::pin(
                apply_patch_tool::execute(runtime, agent_id, authority_class, &call.input),
            ),
            holon_tools::builtins::BuiltinTool::ExecCommand => defer_tool_future!(
                exec_command::execute(runtime, agent_id, authority_class, &call.input, context,)
            ),
            holon_tools::builtins::BuiltinTool::ExecCommandBatch => {
                defer_tool_future!(exec_command_batch::execute(
                    runtime,
                    agent_id,
                    authority_class,
                    &call.input,
                    context,
                ))
            }
            holon_tools::builtins::BuiltinTool::UseWorkspace => Box::pin(use_workspace::execute(
                runtime,
                agent_id,
                authority_class,
                &call.input,
            )),
            holon_tools::builtins::BuiltinTool::ViewImage => Box::pin(view_image::execute(
                runtime,
                agent_id,
                authority_class,
                &call.input,
            )),
            holon_tools::builtins::BuiltinTool::WebFetch => Box::pin(web_fetch::execute(
                runtime,
                agent_id,
                authority_class,
                &call.input,
            )),
            holon_tools::builtins::BuiltinTool::WebSearch => Box::pin(web_search::execute(
                runtime,
                agent_id,
                authority_class,
                &call.input,
            )),
            holon_tools::builtins::BuiltinTool::ListWebFetchProviders => {
                Box::pin(web_fetch::list_providers(runtime, &call.input))
            }
            holon_tools::builtins::BuiltinTool::ListWebSearchProviders => {
                Box::pin(web_search::list_providers(runtime, &call.input))
            }
            holon_tools::builtins::BuiltinTool::XSearch => Box::pin(x_search::execute(
                runtime,
                agent_id,
                authority_class,
                &call.input,
            )),
        };
    Box::pin(async move {
        let _admission = runtime.execution_admission_lease()?;
        future.await
    })
}

pub(crate) fn render_tool_result_for_model(result: &ToolResult) -> Result<String> {
    holon_tools::builtins::render_tool_result_for_model_with_hooks(result, &RENDER_HOOKS)
}

pub(crate) fn render_tool_result_for_model_with_context(
    result: &ToolResult,
    context: &ToolModelRenderContext<'_>,
) -> Result<String> {
    holon_tools::builtins::render_tool_result_with_context_and_hooks(result, context, &RENDER_HOOKS)
}

const RENDER_HOOKS: holon_tools::builtins::ToolRenderHooks<crate::tool::spec::RuntimeToolState> =
    holon_tools::builtins::ToolRenderHooks {
        image: generate_image::render_for_model,
        workspace: get_workspace_state::render_for_model,
    };

pub(crate) use super::dispatch::attach_result_recovery;

pub(crate) fn serialize_success<T: Serialize>(tool_name: &str, value: &T) -> Result<ToolResult> {
    let value = serde_json::to_value(value)?;
    Ok(success_from_value(tool_name, value))
}

pub(crate) fn success_from_value(tool_name: &str, mut value: Value) -> ToolResult {
    let summary_text = match &mut value {
        Value::Object(map) => map
            .remove("summary_text")
            .and_then(|value| value.as_str().map(ToString::to_string)),
        _ => None,
    };
    ToolResult::success(tool_name, value, summary_text)
}
