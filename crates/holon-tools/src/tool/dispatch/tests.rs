use super::{build_tool_spec_catalog, ToolRegistry};
use crate::{tool::schema::validate_source_tool_schema, types::ToolCapabilityFamily};
use holon_provider::provider::{
    emitted_tool_json_schema, validate_emitted_tool_schema, ToolSchemaContract,
};
use serde_json::Value;
use std::path::PathBuf;

fn assert_final_schema_shape(schema: &Value) {
    match schema.get("type").and_then(Value::as_str) {
        Some("object") => {
            assert!(
                schema
                    .get("properties")
                    .and_then(Value::as_object)
                    .is_some(),
                "object schema should expose properties: {schema}"
            );
            assert!(
                schema.get("required").and_then(Value::as_array).is_some(),
                "object schema should expose required array: {schema}"
            );
            assert_eq!(
                schema.get("additionalProperties"),
                Some(&Value::Bool(false)),
                "object schema should disable additionalProperties: {schema}"
            );
            for property in schema["properties"]
                .as_object()
                .expect("properties should be object")
                .values()
            {
                assert_final_schema_shape(property);
            }
        }
        Some("array") => {
            if let Some(items) = schema.get("items") {
                assert_final_schema_shape(items);
            }
        }
        _ => {}
    }
}

#[test]
fn stable_tool_specs_expose_canonical_coding_tools() {
    let registry = ToolRegistry::new(PathBuf::from("."));
    let specs = registry.tool_specs().unwrap();
    let names = specs
        .iter()
        .map(|spec| spec.name.clone())
        .collect::<Vec<_>>();

    for expected in [
        "GetAgent",
        "WaitFor",
        "CreateAgent",
        "SendAgentMessage",
        "InvokeAgent",
        "ListTasks",
        "TaskInput",
        "TaskStatus",
        "CreateWorkItem",
        "PickWorkItem",
        "GetWorkItem",
        "ListWorkItems",
        "UpdateWorkItem",
        "CompleteWorkItem",
        "MemorySearch",
        "MemoryGet",
        "GetWorkspaceState",
        "AttachWorkspace",
        "DetachWorkspace",
        "SwitchWorkspace",
        "CreateWorktree",
        "RemoveWorktree",
        "ApplyPatch",
        "ExecCommand",
        "ViewImage",
        "WebFetch",
        "WebSearch",
    ] {
        assert!(names.iter().any(|name| name == expected));
    }
    for hidden_discovery in [
        "ListModelProviders",
        "ListProviderModels",
        "ListWebFetchProviders",
        "ListWebSearchProviders",
    ] {
        assert!(
            !names.iter().any(|name| name == hidden_discovery),
            "{hidden_discovery} should be demoted from default model tool_specs()"
        );
    }
    assert!(!names.iter().any(|name| name == "Sleep"));
    assert!(!names.iter().any(|name| name == "SpawnAgent"));

    for removed in [
        "Glob",
        "Grep",
        "Read",
        "Write",
        "Edit",
        "ListFiles",
        "SearchText",
        "ReadFile",
        "WriteFile",
        "EditFile",
        "KillCommand",
        "NotifyOperator",
    ] {
        assert!(!names.iter().any(|name| name == removed));
    }

    let exec_command = specs
        .iter()
        .find(|spec| spec.name == "ExecCommand")
        .expect("ExecCommand should be present");
    assert!(exec_command.input_schema["properties"]
        .get("run_in_background")
        .is_none());
    assert!(exec_command.input_schema["properties"]
        .get("background")
        .is_none());
    assert!(exec_command.input_schema["properties"]
        .get("sandbox_permissions")
        .is_none());
    assert!(exec_command.input_schema["properties"]
        .get("justification")
        .is_none());
    assert!(exec_command.input_schema["properties"]
        .get("prefix_rule")
        .is_none());
    assert!(exec_command.input_schema["properties"]
        .get("command")
        .is_none());
    assert!(exec_command.input_schema["properties"]
        .get("status")
        .is_none());
    assert!(exec_command.input_schema["properties"]
        .get("commentary")
        .is_none());

    let generate_image = specs
        .iter()
        .find(|spec| spec.name == "GenerateImage")
        .expect("GenerateImage should be present");
    assert!(!generate_image.description.contains("workspace://"));

    let update_work_item = specs
        .iter()
        .find(|spec| spec.name == "UpdateWorkItem")
        .expect("UpdateWorkItem should be present");
    assert!(update_work_item.input_schema["properties"]
        .get("objective")
        .is_some());

    let catalog = build_tool_spec_catalog().expect("catalog should build");
    let list_model_providers = catalog
        .entries
        .iter()
        .map(|entry| &entry.spec)
        .find(|spec| spec.name == "ListModelProviders")
        .expect("ListModelProviders should remain in dispatch catalog");
    assert!(list_model_providers.input_schema["properties"]
        .get("include_unavailable")
        .is_some());

    let list_provider_models = catalog
        .entries
        .iter()
        .map(|entry| &entry.spec)
        .find(|spec| spec.name == "ListProviderModels")
        .expect("ListProviderModels should remain in dispatch catalog");
    assert!(list_provider_models.input_schema["properties"]
        .get("provider")
        .is_some());

    let create_agent = specs
        .iter()
        .find(|spec| spec.name == "CreateAgent")
        .expect("CreateAgent should be present");
    assert!(create_agent.input_schema["properties"]
        .get("agent_id")
        .is_some());
    assert!(create_agent.input_schema["properties"]
        .get("initial_message")
        .is_some());
    assert!(create_agent.input_schema["properties"]
        .get("template")
        .is_some());
    assert!(create_agent.input_schema["properties"]
        .get("model")
        .is_some());
    assert!(create_agent.input_schema["properties"]["model"]
        .to_string()
        .contains("allow_fallback"));
    for forbidden in [
        "authority_class",
        "origin",
        "lineage_parent_agent_id",
        "supervision",
        "durability",
        "lifecycle_attachment",
    ] {
        assert!(create_agent.input_schema["properties"]
            .get(forbidden)
            .is_none());
    }

    let invoke_agent = specs
        .iter()
        .find(|spec| spec.name == "InvokeAgent")
        .expect("InvokeAgent should be present");
    assert!(invoke_agent.input_schema["properties"]
        .get("initial_message")
        .is_some());
    let target_schema = &invoke_agent.input_schema["properties"]["target"];
    let target_schema_text = target_schema.to_string();
    assert!(target_schema_text.contains("existing_agent"));
    assert!(target_schema_text.contains("new_subagent"));
    assert!(target_schema_text.contains("agent_id"));
    assert!(target_schema_text.contains("workspace_mode"));
    assert!(target_schema_text.contains("allow_fallback"));
    for forbidden in ["authority_class", "origin", "caller_agent_id", "trust"] {
        assert!(invoke_agent.input_schema["properties"]
            .get(forbidden)
            .is_none());
    }
}

#[test]
fn stable_tool_specs_include_task_control_and_mutating_tools() {
    let registry = ToolRegistry::new(PathBuf::from("."));
    let names = registry
        .tool_specs()
        .unwrap()
        .into_iter()
        .map(|spec| spec.name)
        .collect::<Vec<_>>();

    assert!(!names.iter().any(|name| name == "TaskOutput"));
    assert!(names.iter().any(|name| name == "TaskStatus"));
    assert!(names.iter().any(|name| name == "ListTasks"));
    assert!(names.iter().any(|name| name == "TaskInput"));
    assert!(names.iter().any(|name| name == "CreateWorkItem"));
    assert!(names.iter().any(|name| name == "PickWorkItem"));
    assert!(names.iter().any(|name| name == "GetWorkItem"));
    assert!(names.iter().any(|name| name == "ListWorkItems"));
    assert!(names.iter().any(|name| name == "UpdateWorkItem"));
    assert!(names.iter().any(|name| name == "CompleteWorkItem"));
    assert!(names.iter().any(|name| name == "MemorySearch"));
    assert!(names.iter().any(|name| name == "MemoryGet"));
    assert!(names.iter().all(|name| name != "CreateExternalTrigger"));
    assert!(names.iter().all(|name| name != "CancelExternalTrigger"));
    assert!(names.iter().all(|name| name != "TaskList"));
    assert!(names.iter().all(|name| name != "UseWorkspace"));
    assert!(names.iter().any(|name| name == "GetWorkspaceState"));
    assert!(names.iter().any(|name| name == "AttachWorkspace"));
    assert!(names.iter().any(|name| name == "DetachWorkspace"));
    assert!(names.iter().any(|name| name == "SwitchWorkspace"));
    assert!(names.iter().any(|name| name == "CreateWorktree"));
    assert!(names.iter().any(|name| name == "RemoveWorktree"));
    assert!(names.iter().any(|name| name == "ApplyPatch"));
    assert!(names.iter().any(|name| name == "ViewImage"));
    assert!(names.iter().any(|name| name == "WebFetch"));
    assert!(names.iter().any(|name| name == "WebSearch"));
    assert!(names.iter().all(|name| name != "CreateTask"));
}

#[test]
fn stable_tool_specs_cover_expected_capability_families() {
    let catalog = build_tool_spec_catalog().expect("catalog should build");
    let family_for = |name: &str| {
        catalog
            .entries
            .iter()
            .find(|entry| entry.spec.name == name)
            .map(|entry| entry.family)
            .expect("tool should be present")
    };

    assert_eq!(family_for("Sleep"), ToolCapabilityFamily::CoreAgent);
    assert_eq!(family_for("MemorySearch"), ToolCapabilityFamily::CoreAgent);
    assert_eq!(family_for("MemoryGet"), ToolCapabilityFamily::CoreAgent);
    assert_eq!(family_for("ListTasks"), ToolCapabilityFamily::CoreAgent);
    assert_eq!(family_for("TaskList"), ToolCapabilityFamily::CoreAgent);
    assert_eq!(
        family_for("CreateWorkItem"),
        ToolCapabilityFamily::CoreAgent
    );
    assert_eq!(family_for("PickWorkItem"), ToolCapabilityFamily::CoreAgent);
    assert_eq!(family_for("GetWorkItem"), ToolCapabilityFamily::CoreAgent);
    assert_eq!(family_for("ListWorkItems"), ToolCapabilityFamily::CoreAgent);
    assert_eq!(
        family_for("UpdateWorkItem"),
        ToolCapabilityFamily::CoreAgent
    );
    assert_eq!(
        family_for("CompleteWorkItem"),
        ToolCapabilityFamily::CoreAgent
    );
    assert_eq!(
        family_for("CreateAgent"),
        ToolCapabilityFamily::AgentCreation
    );
    assert_eq!(
        family_for("InvokeAgent"),
        ToolCapabilityFamily::AgentCreation
    );
    assert_eq!(
        family_for("ExecCommand"),
        ToolCapabilityFamily::LocalEnvironment
    );
    assert_eq!(
        family_for("UseWorkspace"),
        ToolCapabilityFamily::LocalEnvironment
    );
    assert_eq!(
        family_for("GetWorkspaceState"),
        ToolCapabilityFamily::LocalEnvironment
    );
    assert_eq!(
        family_for("SwitchWorkspace"),
        ToolCapabilityFamily::LocalEnvironment
    );
    assert_eq!(
        family_for("CreateWorktree"),
        ToolCapabilityFamily::LocalEnvironment
    );
    assert_eq!(
        family_for("RemoveWorktree"),
        ToolCapabilityFamily::LocalEnvironment
    );
    assert_eq!(
        family_for("AttachWorkspace"),
        ToolCapabilityFamily::AuthorityExpanding
    );
    assert_eq!(
        family_for("DetachWorkspace"),
        ToolCapabilityFamily::AuthorityExpanding
    );
    assert_eq!(
        family_for("ViewImage"),
        ToolCapabilityFamily::LocalEnvironment
    );
    assert_eq!(family_for("WebFetch"), ToolCapabilityFamily::Web);
    assert_eq!(family_for("WebSearch"), ToolCapabilityFamily::Web);
    assert_eq!(
        family_for("ListWebFetchProviders"),
        ToolCapabilityFamily::Web
    );
    assert_eq!(
        family_for("ListWebSearchProviders"),
        ToolCapabilityFamily::Web
    );
}

#[test]
fn hidden_sleep_stays_dispatchable_but_not_model_facing() {
    let registry = ToolRegistry::new(PathBuf::from("."));
    let names = registry
        .tool_specs()
        .unwrap()
        .into_iter()
        .map(|spec| spec.name)
        .collect::<Vec<_>>();

    assert!(names.iter().all(|name| name != "Sleep"));
    assert!(names.iter().all(|name| name != "TaskList"));
    assert!(names.iter().all(|name| name != "UseWorkspace"));
    for demoted in [
        "ListModelProviders",
        "ListProviderModels",
        "ListWebFetchProviders",
        "ListWebSearchProviders",
    ] {
        assert!(names.iter().all(|name| name != demoted));
        assert!(registry.family_for_tool(demoted).unwrap().is_some());
    }
    assert_eq!(
        registry.family_for_tool("Sleep").unwrap(),
        Some(ToolCapabilityFamily::CoreAgent)
    );
    assert_eq!(
        registry.family_for_tool("UseWorkspace").unwrap(),
        Some(ToolCapabilityFamily::LocalEnvironment)
    );
}

#[test]
fn stable_tool_specs_do_not_drift_by_trust() {
    let registry = ToolRegistry::new(PathBuf::from("."));
    let first = registry.tool_specs().unwrap();
    let second = registry.tool_specs().unwrap();
    assert_eq!(
        first
            .iter()
            .map(|spec| spec.name.as_str())
            .collect::<Vec<_>>(),
        second
            .iter()
            .map(|spec| spec.name.as_str())
            .collect::<Vec<_>>()
    );
}

#[test]
fn complete_work_item_schema_keeps_completion_report_out_of_tool_input() {
    let registry = ToolRegistry::new(PathBuf::from("."));
    let complete_work_item = registry
        .tool_specs()
        .unwrap()
        .into_iter()
        .find(|spec| spec.name == "CompleteWorkItem")
        .expect("CompleteWorkItem should be present");

    assert!(complete_work_item.input_schema["properties"]["work_item_id"].is_object());
    assert!(complete_work_item.input_schema["properties"]["result_summary"].is_null());
    assert!(complete_work_item
        .description
        .contains("assistant text in the same round"));
}

#[test]
fn emitted_tool_schemas_remain_strict_for_all_exposed_tools() {
    let registry = ToolRegistry::new(PathBuf::from("."));

    for spec in registry.tool_specs().unwrap() {
        validate_source_tool_schema(&spec.input_schema)
            .expect("source tool schema should stay valid");
        let final_schema = emitted_tool_json_schema(&spec.input_schema, ToolSchemaContract::Strict)
            .expect("tool schema should normalize");
        validate_emitted_tool_schema(&final_schema, ToolSchemaContract::Strict)
            .expect("strict emitted schema should stay valid");
        assert_final_schema_shape(&final_schema);
    }
}

#[test]
fn enqueue_priority_becomes_nullable_in_strict_emitted_shape() {
    let registry = ToolRegistry::new(PathBuf::from("."));
    let enqueue = registry
        .tool_specs()
        .unwrap()
        .into_iter()
        .find(|spec| spec.name == "Enqueue")
        .expect("Enqueue should be present");
    let final_schema = emitted_tool_json_schema(&enqueue.input_schema, ToolSchemaContract::Strict)
        .expect("enqueue schema");
    let priority = &final_schema["properties"]["priority"];

    let required = final_schema["required"]
        .as_array()
        .expect("required should be an array");
    assert_eq!(required.len(), 2);
    assert!(required.iter().any(|value| value == "text"));
    assert!(required.iter().any(|value| value == "priority"));
    let priority_types = priority["type"]
        .as_array()
        .expect("priority type should be an array")
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    assert!(priority_types.contains(&"string"));
    assert!(priority_types.contains(&"null"));
    let priority_enum = priority["enum"]
        .as_array()
        .expect("priority enum should be an array");
    assert!(priority_enum.iter().any(|value| value == "interject"));
    assert!(priority_enum.iter().any(Value::is_null));
}

#[test]
fn memory_get_schema_bounds_max_chars_in_final_emitted_shape() {
    let registry = ToolRegistry::new(PathBuf::from("."));
    let memory_get = registry
        .tool_specs()
        .unwrap()
        .into_iter()
        .find(|spec| spec.name == "MemoryGet")
        .expect("MemoryGet should be present");
    let final_schema =
        emitted_tool_json_schema(&memory_get.input_schema, ToolSchemaContract::Strict)
            .expect("memory get schema");
    let max_chars = &final_schema["properties"]["max_chars"];

    assert!(final_schema["required"]
        .as_array()
        .expect("required should be an array")
        .iter()
        .any(|value| value == "max_chars"));
    let max_chars_types = max_chars["type"]
        .as_array()
        .expect("max_chars type should be an array")
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    assert!(max_chars_types.contains(&"integer"));
    assert!(max_chars_types.contains(&"null"));
    assert_eq!(max_chars["minimum"], Value::from(1.0));
    assert_eq!(max_chars["maximum"], Value::from(50000.0));
}
