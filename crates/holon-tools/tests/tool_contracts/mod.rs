mod apply_patch;
mod exec_command;
mod exec_command_batch;
mod generate_image;
mod get_workspace_state;
mod inventory;
mod list_provider_models;
mod memory_get;
mod memory_search;
mod sleep;
mod timer;
mod use_workspace;
mod view_image;
mod wait_for;

use holon_tools::ToolResult;
use serde::Serialize;

fn serialize_success(tool_name: &str, value: &impl Serialize) -> serde_json::Result<ToolResult> {
    let mut value = serde_json::to_value(value)?;
    let summary_text = value
        .as_object_mut()
        .and_then(|fields| fields.remove("summary_text"))
        .and_then(|summary| summary.as_str().map(str::to_owned));
    Ok(ToolResult::success(tool_name, value, summary_text))
}
