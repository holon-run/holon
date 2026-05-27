//! Tool layer implementation
//!
//! This module provides a clear separation of concerns for tool functionality:
//! - `spec`: Tool schema definitions (ToolSpec, ToolCall, ToolResult)
//! - `dispatch`: Tool routing and registry (ToolRegistry)
//! - `helpers`: Shared utility functions
//! - `tools`: Builtin tool modules, one per tool

pub(crate) mod apply_patch;
pub mod dispatch;
pub mod error;
pub(crate) mod helpers;
pub(crate) mod schema_support;
pub mod spec;
pub(crate) mod tools;

use anyhow::Result;
use serde_json::{json, Value};

pub(crate) use schema_support as schema;

// Re-export the key types that are used throughout the codebase
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
            json!({
                "name": name,
                "family": definition.family.label(),
                "stability": tool_stability_level(&definition.spec.name),
                "input_schema": definition.spec.input_schema,
                "freeform_grammar": definition.spec.freeform_grammar,
                "result_envelope": {
                    "canonical": "ToolResultEnvelope",
                    "success_result": tool_success_result_contract(&definition.spec.name),
                    "error_result": "ToolError",
                    "model_rendering": tool_model_rendering_contract(&definition.spec.name),
                },
                "related_surfaces": related_surfaces_for_tool(&definition.spec.name),
                "description": definition.spec.description,
            })
        })
        .collect::<Vec<_>>();

    Ok(json!({
        "version": 1,
        "source_of_truth": {
            "tool_definitions": "src/tool/tools/mod.rs builtin_tool_definitions()",
            "input_schemas": "typed Rust argument structs deriving schemars::JsonSchema",
            "result_envelope": "src/tool/spec.rs ToolResultEnvelope",
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
        "CreateExternalTrigger" | "CancelExternalTrigger" => "deprecated",
        "ApplyPatch" | "ExecCommand" | "ExecCommandBatch" | "Sleep" | "WaitFor" | "TaskList"
        | "TaskStatus" | "TaskInput" | "TaskOutput" | "TaskStop" | "CreateWorkItem"
        | "PickWorkItem" | "GetWorkItem" | "ListWorkItems" | "UpdateWorkItem"
        | "CompleteWorkItem" | "UseWorkspace" | "AgentGet" | "Enqueue" | "SpawnAgent"
        | "MemorySearch" | "MemoryGet" => "stable",
        _ => "experimental",
    }
}

fn tool_success_result_contract(name: &str) -> &'static str {
    match name {
        "AgentGet" => "AgentGetResult",
        "ApplyPatch" => "ApplyPatchResult",
        "CompleteWorkItem" | "CreateWorkItem" | "UpdateWorkItem" => "WorkItemMutationResult",
        "Enqueue" => "EnqueueResult",
        "ExecCommand" => "ExecCommandResult",
        "ExecCommandBatch" => "ExecCommandBatchResult",
        "GetWorkItem" => "GetWorkItemResult",
        "ListWorkItems" => "ListWorkItemsResult",
        "MemoryGet" => "MemoryGetResponse",
        "MemorySearch" => "MemorySearchResponse",
        "PickWorkItem" => "PickWorkItemResult",
        "Sleep" => "SleepResult",
        "WaitFor" => "WaitForResult",
        "SpawnAgent" => "SpawnAgentResult",
        "TaskInput" => "TaskInputResult",
        "TaskList" => "Vec<TaskDigest>",
        "TaskOutput" => "TaskOutputResult",
        "TaskStatus" => "TaskStatusResult",
        "TaskStop" => "TaskStopResult",
        "UseWorkspace" => "UseWorkspaceResult",
        "WebFetch" => "WebFetchResult",
        "WebSearch" => "WebSearchResult",
        _ => "tool-specific JSON payload",
    }
}

fn tool_model_rendering_contract(name: &str) -> &'static str {
    match name {
        "ApplyPatch" | "ExecCommand" | "ExecCommandBatch" | "TaskOutput" => "custom_text_receipt",
        _ => "canonical_json_envelope",
    }
}

fn related_surfaces_for_tool(name: &str) -> Vec<&'static str> {
    match name {
        "ExecCommand" | "ExecCommandBatch" | "TaskInput" | "TaskList" | "TaskOutput"
        | "TaskStatus" | "TaskStop" => {
            vec!["CLI task/process wrappers", "HTTP control-plane task APIs"]
        }
        "CreateWorkItem" | "PickWorkItem" | "GetWorkItem" | "ListWorkItems" | "UpdateWorkItem"
        | "CompleteWorkItem" => {
            vec!["CLI work-item wrappers", "HTTP control-plane WorkItem APIs"]
        }
        "Sleep" | "WaitFor" | "Enqueue" | "AgentGet" | "SpawnAgent" => {
            vec!["runtime agent lifecycle APIs"]
        }
        "ApplyPatch" | "UseWorkspace" => vec!["workspace/runtime file APIs"],
        "WebFetch" | "WebSearch" => vec!["web adapter APIs"],
        "MemorySearch" | "MemoryGet" => vec!["memory/runtime APIs"],
        _ => Vec::new(),
    }
}
