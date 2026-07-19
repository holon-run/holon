//! Tool layer implementation
//!
//! This module provides a clear separation of concerns for tool functionality:
//! - `spec`: Tool schema definitions (ToolSpec, ToolCall, ToolResult)
//! - `dispatch`: Tool routing and registry (ToolRegistry)
//! - `helpers`: Shared utility functions
//! - `tools`: Builtin tool modules, one per tool

use crate::tool::names as tn;
use crate::types::{
    EnqueueResult, GenerateImageResult, TaskInputResult, TaskOutputResult, TaskStatusResult,
    TaskStopResult,
};
use anyhow::Result;
use serde_json::{json, Value};

pub(crate) mod apply_patch;
pub mod dispatch;
pub mod error;
pub(crate) mod helpers;
pub mod names;
pub(crate) mod schema_support;
pub mod spec;
pub(crate) mod summary;
pub(crate) mod tools;

pub(crate) use schema_support as schema;

// Re-export the key types that are used throughout the codebase
pub use apply_patch::ApplyPatchSurface;
pub use dispatch::ToolRegistry;
pub use error::ToolError;
pub use spec::{ToolCall, ToolResult, ToolSpec};

/// Generate the checked-in model-facing built-in tool schema inventory.
///
/// The Rust tool definitions remain the source of truth. The generated
/// inventory is snapshot-tested so CI catches accidental drift in names,
/// families, input schemas, freeform grammars, and result envelope metadata.
pub fn model_tool_schema_inventory() -> Result<Value> {
    let registry = ToolRegistry::new(std::path::PathBuf::from("."));
    let model_facing_names = registry
        .tool_specs()?
        .into_iter()
        .map(|spec| spec.name)
        .collect::<std::collections::BTreeSet<_>>();
    let tools = tools::builtin_tool_definitions()?
        .into_iter()
        .filter(|definition| model_facing_names.contains(&definition.spec.name))
        .map(|definition| {
            let name = definition.spec.name.clone();
            let success_result_schema = tool_success_result_schema(&definition.spec.name)?;
            Ok(json!({
                "name": name,
                "family": definition.family.label(),
                "stability": tool_stability_level(&definition.spec.name),
                "input_schema": definition.spec.input_schema,
                "freeform_grammar": definition.spec.freeform_grammar,
                "result_envelope": {
                    "canonical": "ToolResultEnvelope",
                    "success_result": tool_success_result_contract(&definition.spec.name),
                    "success_result_schema": success_result_schema,
                    "error_result": "ToolError",
                    "model_rendering": tool_model_rendering_contract(&definition.spec.name),
                },
                "related_surfaces": related_surfaces_for_tool(&definition.spec.name),
                "description": definition.spec.description,
            }))
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(json!({
        "version": 2,
        "source_of_truth": {
            "tool_definitions": "src/tool/tools/mod.rs builtin_tool_definitions()",
            "input_schemas": "typed Rust argument structs deriving schemars::JsonSchema",
            "result_envelope": "src/tool/spec.rs ToolResultEnvelope",
            "covered_result_schemas": "typed stable result structs deriving schemars::JsonSchema",
            "model_rendering": "src/tool/tools/mod.rs render_tool_result_for_model()",
        },
        "stability_policy": {
            "stable": "Name, input schema, result envelope family, and documented model rendering are intended to be compatibility-preserving.",
            "experimental": "Surface is available but may change while the runtime contract is still settling.",
            "deprecated": "Surface remains for compatibility but should not be introduced into new workflows.",
        },
        "tools": tools,
    }))
}

fn tool_stability_level(name: &str) -> &'static str {
    match name {
        n if tn::DEPRECATED_TOOL_NAMES.contains(&n) => "deprecated",
        n if tn::STABLE_TOOL_NAMES.contains(&n) => "stable",
        _ => "experimental",
    }
}

fn tool_success_result_contract(name: &str) -> &'static str {
    match name {
        tn::AGENT_GET => "AgentGetResult",
        tn::APPLY_PATCH => "ApplyPatchResult",
        tn::ATTACH_WORKSPACE => "AttachWorkspaceResult",
        tn::COMPLETE_WORK_ITEM | tn::CREATE_WORK_ITEM | tn::UPDATE_WORK_ITEM => {
            "WorkItemMutationResult"
        }
        tn::CREATE_WORKTREE => "CreateWorktreeResult",
        tn::DETACH_WORKSPACE => "DetachWorkspaceResult",
        tn::ENQUEUE => "EnqueueResult",
        tn::EXEC_COMMAND => "ExecCommandResult",
        tn::EXEC_COMMAND_BATCH => "ExecCommandBatchResult",
        tn::GENERATE_IMAGE => "GenerateImageResult",
        tn::GET_WORK_ITEM => "GetWorkItemResult",
        tn::GET_WORKSPACE_STATE => "WorkspaceStateResult",
        tn::LIST_MODEL_PROVIDERS => "ListModelProvidersResult",
        tn::LIST_PROVIDER_MODELS => "ListProviderModelsResult",
        tn::LIST_WORK_ITEMS => "ListWorkItemsResult",
        tn::MEMORY_GET => "MemoryGetResponse",
        tn::MEMORY_SEARCH => "MemorySearchResponse",
        tn::PICK_WORK_ITEM => "PickWorkItemResult",
        tn::REMOVE_WORKTREE => "RemoveWorktreeResult",
        tn::SLEEP => "SleepResult",
        tn::WAIT_FOR => "WaitForResult",
        tn::SPAWN_AGENT => "SpawnAgentResult",
        tn::TASK_INPUT => "TaskInputResult",
        tn::LIST_TASKS => "ListTasksResult",
        tn::TASK_OUTPUT => "TaskOutputResult",
        tn::TASK_STATUS => "TaskStatusResult",
        tn::TASK_STOP => "TaskStopResult",
        tn::SWITCH_WORKSPACE => "SwitchWorkspaceResult",
        tn::USE_WORKSPACE => "UseWorkspaceResult",
        tn::WEB_FETCH => "WebFetchResult",
        tn::WEB_SEARCH => "WebSearchResult",
        tn::X_SEARCH => "XSearchResult",
        _ => "tool-specific JSON payload",
    }
}

fn tool_success_result_schema(name: &str) -> Result<Option<Value>> {
    let schema = match name {
        tn::ENQUEUE => schema::tool_result_schema::<EnqueueResult>()?,
        tn::GENERATE_IMAGE => schema::tool_result_schema::<GenerateImageResult>()?,
        tn::LIST_TASKS => schema::tool_result_schema::<tools::task_list::ListTasksResult>()?,
        tn::TASK_INPUT => schema::tool_result_schema::<TaskInputResult>()?,
        tn::TASK_OUTPUT => schema::tool_result_schema::<TaskOutputResult>()?,
        tn::TASK_STATUS => schema::tool_result_schema::<TaskStatusResult>()?,
        tn::TASK_STOP => schema::tool_result_schema::<TaskStopResult>()?,
        _ => return Ok(None),
    };
    Ok(Some(schema))
}

fn tool_model_rendering_contract(name: &str) -> &'static str {
    if tn::CUSTOM_TEXT_RECEIPT_TOOLS.contains(&name) {
        "custom_text_receipt"
    } else {
        "canonical_json_envelope"
    }
}

fn related_surfaces_for_tool(name: &str) -> Vec<&'static str> {
    match name {
        tn::EXEC_COMMAND
        | tn::EXEC_COMMAND_BATCH
        | tn::TASK_INPUT
        | tn::LIST_TASKS
        | tn::TASK_OUTPUT
        | tn::TASK_STATUS
        | tn::TASK_STOP => {
            vec!["CLI task/process wrappers", "HTTP control-plane task APIs"]
        }
        tn::CREATE_WORK_ITEM
        | tn::PICK_WORK_ITEM
        | tn::GET_WORK_ITEM
        | tn::LIST_WORK_ITEMS
        | tn::UPDATE_WORK_ITEM
        | tn::COMPLETE_WORK_ITEM => {
            vec!["CLI work-item wrappers", "HTTP control-plane WorkItem APIs"]
        }
        tn::SLEEP | tn::WAIT_FOR | tn::ENQUEUE | tn::AGENT_GET | tn::SPAWN_AGENT => {
            vec!["runtime agent lifecycle APIs"]
        }
        tn::APPLY_PATCH
        | tn::GET_WORKSPACE_STATE
        | tn::SWITCH_WORKSPACE
        | tn::CREATE_WORKTREE
        | tn::REMOVE_WORKTREE
        | tn::USE_WORKSPACE => vec!["workspace/runtime file APIs"],
        tn::ATTACH_WORKSPACE | tn::DETACH_WORKSPACE => {
            vec!["workspace binding control APIs"]
        }
        tn::WEB_FETCH | tn::WEB_SEARCH => vec!["web adapter APIs"],
        tn::X_SEARCH => vec!["xAI hosted search adapter"],
        tn::MEMORY_SEARCH | tn::MEMORY_GET => vec!["memory/runtime APIs"],
        _ => Vec::new(),
    }
}
