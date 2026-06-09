use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::{apply_patch::ApplyPatchSurface, ToolCall, ToolResult, ToolSpec},
    types::{AuthorityClass, ToolCapabilityFamily},
};

pub(crate) mod agent_get;
pub(crate) mod apply_patch_tool;
pub(crate) mod cancel_external_trigger;
pub(crate) mod complete_work_item;
pub(crate) mod create_external_trigger;
pub(crate) mod create_work_item;
pub(crate) mod enqueue;
pub(crate) mod exec_command;
pub(crate) mod exec_command_batch;
pub(crate) mod get_work_item;
pub(crate) mod list_model_providers;
pub(crate) mod list_provider_models;
pub(crate) mod list_work_items;
pub(crate) mod memory_get;
pub(crate) mod memory_search;
pub(crate) mod pick_work_item;
pub(crate) mod sleep;
pub(crate) mod spawn_agent;
pub(crate) mod task_input;
pub(crate) mod task_list;
pub(crate) mod task_output;
pub(crate) mod task_status;
pub(crate) mod task_stop;
pub(crate) mod update_work_item;
pub(crate) mod use_workspace;
pub(crate) mod wait_for;
pub(crate) mod web_fetch;
pub(crate) mod web_search;
pub(crate) mod work_item_action;
pub(crate) mod work_item_query;

pub(crate) struct BuiltinToolDefinition {
    pub(crate) family: ToolCapabilityFamily,
    pub(crate) spec: ToolSpec,
}

pub(crate) fn builtin_tool_definitions() -> Result<Vec<BuiltinToolDefinition>> {
    Ok(vec![
        sleep::definition()?,
        wait_for::definition()?,
        agent_get::definition()?,
        enqueue::definition()?,
        spawn_agent::definition()?,
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
        list_work_items::definition()?,
        update_work_item::definition()?,
        complete_work_item::definition()?,
        memory_search::definition()?,
        memory_get::definition()?,
        apply_patch_tool::definition()?,
        exec_command::definition()?,
        exec_command_batch::definition()?,
        use_workspace::definition()?,
        web_fetch::definition()?,
        web_search::definition()?,
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

pub(crate) async fn execute_builtin_tool(
    runtime: &RuntimeHandle,
    agent_id: &str,
    authority_class: &AuthorityClass,
    call: &ToolCall,
) -> Result<ToolResult> {
    match call.name.as_str() {
        sleep::NAME => sleep::execute(runtime, agent_id, authority_class, &call.input).await,
        wait_for::NAME => wait_for::execute(runtime, agent_id, authority_class, &call.input).await,
        agent_get::NAME => {
            agent_get::execute(runtime, agent_id, authority_class, &call.input).await
        }
        enqueue::NAME => enqueue::execute(runtime, agent_id, authority_class, &call.input).await,
        spawn_agent::NAME => {
            spawn_agent::execute(runtime, agent_id, authority_class, &call.input).await
        }
        task_list::NAME => {
            task_list::execute(runtime, agent_id, authority_class, &call.input).await
        }
        task_list::LEGACY_NAME => {
            task_list::execute_legacy(runtime, agent_id, authority_class, &call.input).await
        }
        task_status::NAME => {
            task_status::execute(runtime, agent_id, authority_class, &call.input).await
        }
        task_input::NAME => {
            task_input::execute(runtime, agent_id, authority_class, &call.input).await
        }
        task_output::NAME => {
            task_output::execute(runtime, agent_id, authority_class, &call.input).await
        }
        task_stop::NAME => {
            task_stop::execute(runtime, agent_id, authority_class, &call.input).await
        }
        list_model_providers::NAME => {
            list_model_providers::execute(runtime, agent_id, authority_class, &call.input).await
        }
        list_provider_models::NAME => {
            list_provider_models::execute(runtime, agent_id, authority_class, &call.input).await
        }
        create_work_item::NAME => {
            create_work_item::execute(runtime, agent_id, authority_class, &call.input).await
        }
        pick_work_item::NAME => {
            pick_work_item::execute(runtime, agent_id, authority_class, &call.input).await
        }
        get_work_item::NAME => {
            get_work_item::execute(runtime, agent_id, authority_class, &call.input).await
        }
        list_work_items::NAME => {
            list_work_items::execute(runtime, agent_id, authority_class, &call.input).await
        }
        update_work_item::NAME => {
            update_work_item::execute(runtime, agent_id, authority_class, &call.input).await
        }
        complete_work_item::NAME => {
            complete_work_item::execute(runtime, agent_id, authority_class, &call.input).await
        }
        memory_search::NAME => {
            memory_search::execute(runtime, agent_id, authority_class, &call.input).await
        }
        memory_get::NAME => {
            memory_get::execute(runtime, agent_id, authority_class, &call.input).await
        }
        create_external_trigger::NAME => {
            create_external_trigger::execute(runtime, agent_id, authority_class, &call.input).await
        }
        cancel_external_trigger::NAME => {
            cancel_external_trigger::execute(runtime, agent_id, authority_class, &call.input).await
        }
        apply_patch_tool::NAME => {
            apply_patch_tool::execute(runtime, agent_id, authority_class, &call.input).await
        }
        exec_command::NAME => {
            exec_command::execute(runtime, agent_id, authority_class, &call.input).await
        }
        exec_command_batch::NAME => {
            exec_command_batch::execute(runtime, agent_id, authority_class, &call.input).await
        }
        use_workspace::NAME => {
            use_workspace::execute(runtime, agent_id, authority_class, &call.input).await
        }
        web_fetch::NAME => {
            web_fetch::execute(runtime, agent_id, authority_class, &call.input).await
        }
        web_search::NAME => {
            web_search::execute(runtime, agent_id, authority_class, &call.input).await
        }
        _ => Err(anyhow!("unknown builtin tool {}", call.name)),
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
        _ => canonical_json_render(result),
    }
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

    fn description_path(tool_name: &str) -> Option<&'static str> {
        Some(match tool_name {
            "AgentGet" => "src/tool/tool_descriptions/agent_get.md",
            "ApplyPatch" => "src/tool/tool_descriptions/apply_patch_unified_diff_json.md",
            "CancelExternalTrigger" => "src/tool/tool_descriptions/cancel_external_trigger.md",
            "CompleteWorkItem" => "src/tool/tool_descriptions/complete_work_item.md",
            "CreateExternalTrigger" => "src/tool/tool_descriptions/create_external_trigger.md",
            "CreateWorkItem" => "src/tool/tool_descriptions/create_work_item.md",
            "Enqueue" => "src/tool/tool_descriptions/enqueue.md",
            "ExecCommand" => "src/tool/tool_descriptions/exec_command.md",
            "ExecCommandBatch" => "src/tool/tool_descriptions/exec_command_batch.md",
            "GetWorkItem" => "src/tool/tool_descriptions/get_work_item.md",
            "ListModelProviders" => "src/tool/tool_descriptions/list_model_providers.md",
            "ListProviderModels" => "src/tool/tool_descriptions/list_provider_models.md",
            "ListTasks" => "src/tool/tool_descriptions/list_tasks.md",
            "ListWorkItems" => "src/tool/tool_descriptions/list_work_items.md",
            "MemoryGet" => "src/tool/tool_descriptions/memory_get.md",
            "MemorySearch" => "src/tool/tool_descriptions/memory_search.md",
            "PickWorkItem" => "src/tool/tool_descriptions/pick_work_item.md",
            "Sleep" => "src/tool/tool_descriptions/sleep.md",
            "SpawnAgent" => "src/tool/tool_descriptions/spawn_agent.md",
            "TaskInput" => "src/tool/tool_descriptions/task_input.md",
            "TaskList" => "src/tool/tool_descriptions/task_list_legacy.md",
            "TaskOutput" => "src/tool/tool_descriptions/task_output.md",
            "TaskStatus" => "src/tool/tool_descriptions/task_status.md",
            "TaskStop" => "src/tool/tool_descriptions/task_stop.md",
            "UpdateWorkItem" => "src/tool/tool_descriptions/update_work_item.md",
            "UseWorkspace" => "src/tool/tool_descriptions/use_workspace.md",
            "WaitFor" => "src/tool/tool_descriptions/wait_for.md",
            "WebFetch" => "src/tool/tool_descriptions/web_fetch.md",
            "WebSearch" => "src/tool/tool_descriptions/web_search.md",
            _ => return None,
        })
    }

    #[test]
    fn builtin_tool_descriptions_come_from_markdown_files() {
        let definitions = builtin_tool_definitions().unwrap();

        for definition in definitions {
            let path = description_path(&definition.spec.name)
                .unwrap_or_else(|| panic!("missing description path for {}", definition.spec.name));
            let markdown = std::fs::read_to_string(path).unwrap();
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
            std::fs::read_to_string("src/tool/tool_descriptions/apply_patch_codex_dsl_freeform.md")
                .unwrap()
        );
    }

    #[test]
    fn non_command_tools_default_to_canonical_json_render() {
        let result = ToolResult::success(
            "AgentGet",
            serde_json::json!({"agent": {"id": "default"}}),
            None,
        );
        let rendered = render_tool_result_for_model(&result).unwrap();
        assert!(rendered.starts_with("{\"tool_name\":\"AgentGet\""));
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
