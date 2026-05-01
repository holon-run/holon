use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::{ToolCall, ToolResult, ToolSpec},
    types::{ToolCapabilityFamily, TrustLevel},
};

pub(crate) mod agent_get;
pub(crate) mod apply_patch_tool;
pub(crate) mod cancel_external_trigger;
pub(crate) mod create_external_trigger;
pub(crate) mod enqueue;
pub(crate) mod exec_command;
pub(crate) mod exec_command_batch;
pub(crate) mod get_active_work_item;
pub(crate) mod get_work_item;
pub(crate) mod list_work_items;
pub(crate) mod memory_get;
pub(crate) mod memory_search;
pub(crate) mod notify_operator;
pub(crate) mod sleep;
pub(crate) mod spawn_agent;
pub(crate) mod task_input;
pub(crate) mod task_list;
pub(crate) mod task_output;
pub(crate) mod task_status;
pub(crate) mod task_stop;
pub(crate) mod update_work_item;
pub(crate) mod update_work_plan;
pub(crate) mod use_workspace;
pub(crate) mod work_item_query;

pub(crate) struct BuiltinToolDefinition {
    pub(crate) family: ToolCapabilityFamily,
    pub(crate) spec: ToolSpec,
}

pub(crate) fn builtin_tool_definitions() -> Result<Vec<BuiltinToolDefinition>> {
    Ok(vec![
        sleep::definition()?,
        agent_get::definition()?,
        notify_operator::definition()?,
        enqueue::definition()?,
        spawn_agent::definition()?,
        task_list::definition()?,
        task_status::definition()?,
        task_input::definition()?,
        task_output::definition()?,
        task_stop::definition()?,
        get_active_work_item::definition()?,
        get_work_item::definition()?,
        list_work_items::definition()?,
        update_work_item::definition()?,
        memory_search::definition()?,
        memory_get::definition()?,
        update_work_plan::definition()?,
        create_external_trigger::definition()?,
        cancel_external_trigger::definition()?,
        apply_patch_tool::definition()?,
        exec_command::definition()?,
        exec_command_batch::definition()?,
        use_workspace::definition()?,
    ])
}

pub(crate) async fn execute_builtin_tool(
    runtime: &RuntimeHandle,
    agent_id: &str,
    trust: &TrustLevel,
    call: &ToolCall,
) -> Result<ToolResult> {
    match call.name.as_str() {
        sleep::NAME => sleep::execute(runtime, agent_id, trust, &call.input).await,
        agent_get::NAME => agent_get::execute(runtime, agent_id, trust, &call.input).await,
        notify_operator::NAME => {
            notify_operator::execute(runtime, agent_id, trust, &call.input).await
        }
        enqueue::NAME => enqueue::execute(runtime, agent_id, trust, &call.input).await,
        spawn_agent::NAME => spawn_agent::execute(runtime, agent_id, trust, &call.input).await,
        task_list::NAME => task_list::execute(runtime, agent_id, trust, &call.input).await,
        task_status::NAME => task_status::execute(runtime, agent_id, trust, &call.input).await,
        task_input::NAME => task_input::execute(runtime, agent_id, trust, &call.input).await,
        task_output::NAME => task_output::execute(runtime, agent_id, trust, &call.input).await,
        task_stop::NAME => task_stop::execute(runtime, agent_id, trust, &call.input).await,
        get_active_work_item::NAME => {
            get_active_work_item::execute(runtime, agent_id, trust, &call.input).await
        }
        get_work_item::NAME => get_work_item::execute(runtime, agent_id, trust, &call.input).await,
        list_work_items::NAME => {
            list_work_items::execute(runtime, agent_id, trust, &call.input).await
        }
        update_work_item::NAME => {
            update_work_item::execute(runtime, agent_id, trust, &call.input).await
        }
        memory_search::NAME => memory_search::execute(runtime, agent_id, trust, &call.input).await,
        memory_get::NAME => memory_get::execute(runtime, agent_id, trust, &call.input).await,
        update_work_plan::NAME => {
            update_work_plan::execute(runtime, agent_id, trust, &call.input).await
        }
        create_external_trigger::NAME => {
            create_external_trigger::execute(runtime, agent_id, trust, &call.input).await
        }
        cancel_external_trigger::NAME => {
            cancel_external_trigger::execute(runtime, agent_id, trust, &call.input).await
        }
        apply_patch_tool::NAME => {
            apply_patch_tool::execute(runtime, agent_id, trust, &call.input).await
        }
        exec_command::NAME => exec_command::execute(runtime, agent_id, trust, &call.input).await,
        exec_command_batch::NAME => {
            exec_command_batch::execute(runtime, agent_id, trust, &call.input).await
        }
        use_workspace::NAME => use_workspace::execute(runtime, agent_id, trust, &call.input).await,
        _ => Err(anyhow!("unknown builtin tool {}", call.name)),
    }
}

pub(crate) fn render_tool_result_for_model(result: &ToolResult) -> Result<String> {
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
}
