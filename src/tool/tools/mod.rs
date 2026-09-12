use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::Value;
use std::{future::Future, pin::Pin};

use crate::{
    runtime::RuntimeHandle,
    tool::{
        apply_patch::ApplyPatchSurface, spec::ToolExecutionContext, ToolCall, ToolResult, ToolSpec,
    },
    types::{AuthorityClass, ToolCapabilityFamily},
};

pub(crate) mod apply_patch_tool;
pub(crate) mod attach_workspace;
pub(crate) mod cancel_external_trigger;
pub(crate) mod complete_work_item;
pub(crate) mod create_agent;
pub(crate) mod create_external_trigger;
pub(crate) mod create_work_item;
pub(crate) mod create_worktree;
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
pub(crate) mod list_work_items;
pub(crate) mod memory_get;
pub(crate) mod memory_search;
pub(crate) mod pick_work_item;
pub(crate) mod remove_worktree;
pub(crate) mod semantic_projection;
pub(crate) mod sleep;
pub(crate) mod switch_workspace;
pub(crate) mod task_input;
pub(crate) mod task_list;
pub(crate) mod task_output;
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

pub(crate) struct BuiltinToolDefinition {
    pub(crate) family: ToolCapabilityFamily,
    pub(crate) spec: ToolSpec,
}

pub(crate) struct ToolModelRenderContext<'a> {
    pub(crate) tool_execution_id: &'a str,
    pub(crate) tool_output_budget_estimated_tokens: usize,
}

pub(crate) fn builtin_tool_definitions() -> Result<Vec<BuiltinToolDefinition>> {
    Ok(vec![
        sleep::definition()?,
        wait_for::definition()?,
        timer::create_definition()?,
        timer::list_definition()?,
        timer::get_definition()?,
        timer::cancel_definition()?,
        get_agent::definition()?,
        enqueue::definition()?,
        create_agent::definition()?,
        invoke_agent::definition()?,
        task_list::definition()?,
        task_list::legacy_definition()?,
        task_status::definition()?,
        task_input::definition()?,
        task_output::definition()?,
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

pub(crate) fn builtin_tool_definitions_for_apply_patch_surface(
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

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn execute_builtin_tool<'a>(
    runtime: &'a RuntimeHandle,
    agent_id: &'a str,
    authority_class: &'a AuthorityClass,
    call: &'a ToolCall,
) -> Pin<Box<dyn Future<Output = Result<ToolResult>> + Send + 'a>> {
    Box::pin(async move {
        let context = ToolExecutionContext::default();
        execute_builtin_tool_inner(runtime, agent_id, authority_class, call, &context).await
    })
}

pub(crate) fn execute_builtin_tool_with_context<'a>(
    runtime: &'a RuntimeHandle,
    agent_id: &'a str,
    authority_class: &'a AuthorityClass,
    call: &'a ToolCall,
    context: &'a ToolExecutionContext,
) -> Pin<Box<dyn Future<Output = Result<ToolResult>> + Send + 'a>> {
    execute_builtin_tool_inner(runtime, agent_id, authority_class, call, context)
}

fn execute_builtin_tool_inner<'a>(
    runtime: &'a RuntimeHandle,
    agent_id: &'a str,
    authority_class: &'a AuthorityClass,
    call: &'a ToolCall,
    context: &'a ToolExecutionContext,
) -> Pin<Box<dyn Future<Output = Result<ToolResult>> + Send + 'a>> {
    match call.name.as_str() {
        sleep::NAME => Box::pin(sleep::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        wait_for::NAME => Box::pin(wait_for::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        timer::CREATE_NAME => Box::pin(timer::create(runtime, &call.input)),
        timer::LIST_NAME => Box::pin(timer::list(runtime, &call.input)),
        timer::GET_NAME => Box::pin(timer::get(runtime, &call.input)),
        timer::CANCEL_NAME => Box::pin(timer::cancel(runtime, &call.input)),
        get_agent::NAME => Box::pin(get_agent::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        enqueue::NAME => Box::pin(enqueue::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        create_agent::NAME => Box::pin(create_agent::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        invoke_agent::NAME => Box::pin(invoke_agent::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        task_list::NAME => Box::pin(task_list::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        task_list::LEGACY_NAME => Box::pin(task_list::execute_legacy(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        task_status::NAME => Box::pin(task_status::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        task_input::NAME => Box::pin(task_input::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        task_output::NAME => Box::pin(task_output::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        task_stop::NAME => Box::pin(task_stop::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        list_model_providers::NAME => Box::pin(list_model_providers::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        list_provider_models::NAME => Box::pin(list_provider_models::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        create_work_item::NAME => Box::pin(create_work_item::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        pick_work_item::NAME => Box::pin(pick_work_item::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        get_work_item::NAME => Box::pin(get_work_item::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        generate_image::NAME => Box::pin(generate_image::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        list_work_items::NAME => Box::pin(list_work_items::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        update_work_item::NAME => Box::pin(update_work_item::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        complete_work_item::NAME => Box::pin(complete_work_item::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
            context,
        )),
        memory_search::NAME => Box::pin(memory_search::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        memory_get::NAME => Box::pin(memory_get::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        get_workspace_state::NAME => Box::pin(get_workspace_state::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        attach_workspace::NAME => Box::pin(attach_workspace::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        detach_workspace::NAME => Box::pin(detach_workspace::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        switch_workspace::NAME => Box::pin(switch_workspace::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        create_worktree::NAME => Box::pin(create_worktree::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        remove_worktree::NAME => Box::pin(remove_worktree::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        create_external_trigger::NAME => Box::pin(create_external_trigger::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        cancel_external_trigger::NAME => Box::pin(cancel_external_trigger::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        apply_patch_tool::NAME => Box::pin(apply_patch_tool::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        exec_command::NAME => Box::pin(exec_command::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
            context,
        )),
        exec_command_batch::NAME => Box::pin(exec_command_batch::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
            context,
        )),
        use_workspace::NAME => Box::pin(use_workspace::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        view_image::NAME => Box::pin(view_image::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        web_fetch::NAME => Box::pin(web_fetch::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        web_search::NAME => Box::pin(web_search::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        x_search::NAME => Box::pin(x_search::execute(
            runtime,
            agent_id,
            authority_class,
            &call.input,
        )),
        _ => Box::pin(async move { Err(anyhow!("unknown builtin tool {}", call.name)) }),
    }
}

pub(crate) fn render_tool_result_for_model(result: &ToolResult) -> Result<String> {
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
        task_output::NAME => task_output::render_for_model(result),
        generate_image::NAME => generate_image::render_for_model(result),
        view_image::NAME => view_image::render_for_model(result),
        _ => canonical_json_render(result),
    }
}

pub(crate) fn render_tool_result_for_model_with_context(
    result: &ToolResult,
    context: &ToolModelRenderContext<'_>,
) -> Result<String> {
    let rendered = if result.envelope.tool_name == get_workspace_state::NAME && !result.is_error() {
        get_workspace_state::render_for_model(result, context)?
    } else {
        render_tool_result_for_model(result)?
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

fn estimated_tokens(text: &str) -> usize {
    text.chars().count().saturating_add(3) / 4
}

pub(crate) fn truncate_chars(value: &str, max_chars: usize) -> String {
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

/// Export once at execution, including deferred completion resolution, never
/// while projecting historical rounds.
pub(crate) async fn attach_result_recovery(
    runtime: &RuntimeHandle,
    result: &mut ToolResult,
    execution_id: &str,
) -> Result<()> {
    if let Some(Value::Object(value)) = result.envelope.result.as_mut() {
        value
            .entry("output_ref")
            .or_insert_with(|| serde_json::json!(format!("tool_execution:{execution_id}:output")));
    }
    let canonical = serde_json::to_string_pretty(&result.envelope)?;
    if canonical.chars().count() > 4096 {
        let artifact = match runtime
            .persist_tool_text_artifact("tool-result", &canonical)
            .await
        {
            Ok(path) => serde_json::json!({
                "path": path, "complete": true, "encoding": "utf-8",
                "range_unit": "unicode_scalar", "chars": canonical.chars().count(),
            }),
            Err(error) => serde_json::json!({
                "complete": false,
                "reason": format!("canonical artifact could not be saved: {error}"),
            }),
        };
        if let Some(Value::Object(value)) = result.envelope.result.as_mut() {
            value.insert("recovery_artifact".into(), artifact);
        }
    }
    Ok(())
}

pub(crate) fn canonical_json_render(result: &ToolResult) -> Result<String> {
    serde_json::to_string(&result.envelope).map_err(Into::into)
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_path(path: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(path)
    }

    fn description_path(tool_name: &str) -> Option<&'static str> {
        Some(match tool_name {
            "GetAgent" => "src/tool/tool_descriptions/get_agent.md",
            "ApplyPatch" => "src/tool/tool_descriptions/apply_patch_unified_diff_json.md",
            "AttachWorkspace" => "src/tool/tool_descriptions/attach_workspace.md",
            "CancelExternalTrigger" => "src/tool/tool_descriptions/cancel_external_trigger.md",
            "CancelTimer" => "src/tool/tool_descriptions/cancel_timer.md",
            "CompleteWorkItem" => "src/tool/tool_descriptions/complete_work_item.md",
            "CreateExternalTrigger" => "src/tool/tool_descriptions/create_external_trigger.md",
            "CreateTimer" => "src/tool/tool_descriptions/create_timer.md",
            "CreateWorkItem" => "src/tool/tool_descriptions/create_work_item.md",
            "CreateWorktree" => "src/tool/tool_descriptions/create_worktree.md",
            "DetachWorkspace" => "src/tool/tool_descriptions/detach_workspace.md",
            "Enqueue" => "src/tool/tool_descriptions/enqueue.md",
            "ExecCommand" => "src/tool/tool_descriptions/exec_command.md",
            "ExecCommandBatch" => "src/tool/tool_descriptions/exec_command_batch.md",
            "GenerateImage" => "src/tool/tool_descriptions/generate_image.md",
            "GetTimer" => "src/tool/tool_descriptions/get_timer.md",
            "GetWorkItem" => "src/tool/tool_descriptions/get_work_item.md",
            "GetWorkspaceState" => "src/tool/tool_descriptions/get_workspace_state.md",
            "ListModelProviders" => "src/tool/tool_descriptions/list_model_providers.md",
            "ListProviderModels" => "src/tool/tool_descriptions/list_provider_models.md",
            "ListTasks" => "src/tool/tool_descriptions/list_tasks.md",
            "ListTimers" => "src/tool/tool_descriptions/list_timers.md",
            "ListWorkItems" => "src/tool/tool_descriptions/list_work_items.md",
            "MemoryGet" => "src/tool/tool_descriptions/memory_get.md",
            "MemorySearch" => "src/tool/tool_descriptions/memory_search.md",
            "PickWorkItem" => "src/tool/tool_descriptions/pick_work_item.md",
            "RemoveWorktree" => "src/tool/tool_descriptions/remove_worktree.md",
            "Sleep" => "src/tool/tool_descriptions/sleep.md",
            "CreateAgent" => "src/tool/tool_descriptions/create_agent.md",
            "InvokeAgent" => "src/tool/tool_descriptions/invoke_agent.md",
            "TaskInput" => "src/tool/tool_descriptions/task_input.md",
            "TaskList" => "src/tool/tool_descriptions/task_list_legacy.md",
            "TaskOutput" => "src/tool/tool_descriptions/task_output.md",
            "TaskStatus" => "src/tool/tool_descriptions/task_status.md",
            "TaskStop" => "src/tool/tool_descriptions/task_stop.md",
            "SwitchWorkspace" => "src/tool/tool_descriptions/switch_workspace.md",
            "UpdateWorkItem" => "src/tool/tool_descriptions/update_work_item.md",
            "UseWorkspace" => "src/tool/tool_descriptions/use_workspace.md",
            "ViewImage" => "src/tool/tool_descriptions/view_image.md",
            "WaitFor" => "src/tool/tool_descriptions/wait_for.md",
            "WebFetch" => "src/tool/tool_descriptions/web_fetch.md",
            "WebSearch" => "src/tool/tool_descriptions/web_search.md",
            "XSearch" => "src/tool/tool_descriptions/x_search.md",
            _ => return None,
        })
    }

    #[test]
    fn builtin_tool_descriptions_come_from_markdown_files() {
        let definitions = builtin_tool_definitions().unwrap();

        for definition in definitions {
            let path = description_path(&definition.spec.name)
                .unwrap_or_else(|| panic!("missing description path for {}", definition.spec.name));
            let markdown = std::fs::read_to_string(manifest_path(path)).unwrap();
            assert_eq!(
                definition.spec.description, markdown,
                "{}",
                definition.spec.name
            );
        }

        let codex = apply_patch_tool::definition_for_surface(ApplyPatchSurface::CodexDslFreeform)
            .unwrap()
            .spec
            .description;
        assert_eq!(
            codex,
            std::fs::read_to_string(manifest_path(
                "src/tool/tool_descriptions/apply_patch_codex_dsl_freeform.md"
            ))
            .unwrap()
        );
    }

    #[test]
    fn non_command_tools_default_to_canonical_json_render() {
        let result = ToolResult::success(
            "GetAgent",
            serde_json::json!({"agent": {"id": "default"}}),
            None,
        );
        let rendered = render_tool_result_for_model(&result).unwrap();
        assert!(rendered.starts_with("{\"tool_name\":\"GetAgent\""));
    }

    #[test]
    fn tool_errors_use_shared_model_visible_receipt() {
        let result = ToolResult::error(
            "ExecCommand",
            crate::tool::ToolError::new("invalid_tool_input", "missing required field")
                .with_details(serde_json::json!({ "field": "cmd" }))
                .with_recovery_hint("provide `cmd`"),
        );
        let rendered = render_tool_result_for_model(&result).unwrap();
        let receipt: serde_json::Value = serde_json::from_str(&rendered).unwrap();

        assert_eq!(receipt["ok"], false);
        assert_eq!(receipt["tool_name"], "ExecCommand");
        assert_eq!(receipt["kind"], "invalid_tool_input");
        assert_eq!(receipt["field"], "cmd");
        assert_eq!(receipt["hint"], "provide `cmd`");
    }
}
