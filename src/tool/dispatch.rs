//! Tool routing and dispatch logic.

use anyhow::{anyhow, Result};
use serde_json::json;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::LazyLock;
use uuid::Uuid;

use crate::{
    runtime::RuntimeHandle,
    tool::ToolError,
    types::{ToolCapabilityFamily, ToolExecutionRecord, ToolExecutionStatus, TrustLevel},
};

use super::{
    spec::{ToolCall, ToolResult, ToolSpec},
    tools,
};

/// Tool registry that manages tool execution.
#[derive(Clone)]
pub struct ToolRegistry;

#[derive(Clone)]
struct ToolSpecCatalog {
    entries: Vec<ToolCatalogEntry>,
}

#[derive(Clone)]
struct ToolCatalogEntry {
    #[cfg_attr(not(test), allow(dead_code))]
    family: ToolCapabilityFamily,
    spec: ToolSpec,
}

static TOOL_SPEC_CATALOG: LazyLock<Result<ToolSpecCatalog, String>> =
    LazyLock::new(|| build_tool_spec_catalog().map_err(|error| error.to_string()));

impl ToolRegistry {
    /// Create a new tool registry.
    pub fn new(_workspace_root: PathBuf) -> Self {
        Self
    }

    /// Get the stable tool specifications for this runtime.
    pub fn tool_specs(&self) -> Result<Vec<ToolSpec>> {
        let catalog = TOOL_SPEC_CATALOG
            .as_ref()
            .map_err(|error| anyhow!("failed to build tool schemas: {error}"))?;
        Ok(catalog
            .entries
            .iter()
            .map(|entry| entry.spec.clone())
            .collect())
    }

    pub(crate) fn tool_specs_with_families(&self) -> Result<Vec<(ToolCapabilityFamily, ToolSpec)>> {
        let catalog = TOOL_SPEC_CATALOG
            .as_ref()
            .map_err(|error| anyhow!("failed to build tool schemas: {error}"))?;
        Ok(catalog
            .entries
            .iter()
            .map(|entry| (entry.family, entry.spec.clone()))
            .collect())
    }

    pub(crate) fn family_for_tool(&self, tool_name: &str) -> Result<Option<ToolCapabilityFamily>> {
        let catalog = TOOL_SPEC_CATALOG
            .as_ref()
            .map_err(|error| anyhow!("failed to build tool schemas: {error}"))?;
        Ok(catalog
            .entries
            .iter()
            .find(|entry| entry.spec.name == tool_name)
            .map(|entry| entry.family))
    }

    /// Execute a tool call and return the result along with an execution record.
    pub async fn execute(
        &self,
        runtime: &RuntimeHandle,
        agent_id: &str,
        trust: &TrustLevel,
        call: &ToolCall,
    ) -> Result<(ToolResult, ToolExecutionRecord)> {
        let started_at = chrono::Utc::now();
        let required_family = match self.family_for_tool(&call.name)? {
            Some(required_family) => required_family,
            None => {
                return Err(ToolError::new(
                    "unknown_tool",
                    format!("{} is not an available tool", call.name),
                )
                .with_details(json!({
                    "tool_name": call.name,
                }))
                .with_recovery_hint(format!(
                    "use one of the advertised tool names from tool_specs() instead of {}",
                    call.name
                ))
                .into());
            }
        };
        let identity = runtime.agent_identity_view().await?;
        if !identity
            .profile_preset
            .allows_tool_capability_family(required_family)
        {
            return Err(ToolError::new(
                "unsupported_agent_profile_capability",
                format!(
                    "{} is not available for agents with the `{}` profile",
                    call.name,
                    identity.profile_preset.label()
                ),
            )
            .with_details(json!({
                "tool_name": call.name,
                "required_family": required_family.label(),
                "profile_preset": identity.profile_preset.label(),
            }))
            .with_recovery_hint(format!(
                "run {} from an agent whose profile allows the `{}` capability family",
                call.name,
                required_family.label()
            ))
            .into());
        }
        let result = tools::execute_builtin_tool(runtime, agent_id, trust, call).await?;
        if !result.is_error() {
            if let Err(error) =
                maybe_refresh_memory_index_after_tool(runtime, call.name.as_str(), &result).await
            {
                eprintln!(
                    "failed to refresh memory index after {}: {error:#}",
                    call.name
                );
            }
        }

        let output_value = json!({
            "envelope": result.envelope,
            "is_error": result.is_error(),
            "should_sleep": result.should_sleep,
            "sleep_duration_ms": result.sleep_duration_ms,
            "error": result.tool_error().cloned(),
        });
        let completed_at = chrono::Utc::now();
        let duration_ms = completed_at
            .signed_duration_since(started_at)
            .num_milliseconds()
            .max(0) as u64;
        let record = ToolExecutionRecord {
            id: Uuid::new_v4().to_string(),
            agent_id: agent_id.to_string(),
            work_item_id: None,
            turn_index: 0,
            tool_name: call.name.clone(),
            created_at: started_at,
            completed_at: Some(completed_at),
            duration_ms,
            trust: trust.clone(),
            status: if result.is_error() {
                ToolExecutionStatus::Error
            } else {
                ToolExecutionStatus::Success
            },
            input: call.input.clone(),
            output: output_value,
            summary: tool_result_summary(&result),
            invocation_surface: tool_invocation_surface(call),
        };
        Ok((result, record))
    }
}

async fn maybe_refresh_memory_index_after_tool(
    runtime: &RuntimeHandle,
    tool_name: &str,
    result: &ToolResult,
) -> Result<()> {
    if tool_name != "ApplyPatch" {
        return Ok(());
    }
    let Some(paths) = result
        .envelope
        .result
        .as_ref()
        .and_then(|value| value.get("changed_paths"))
        .and_then(Value::as_array)
    else {
        return Ok(());
    };
    let changed_paths = paths
        .iter()
        .filter_map(Value::as_str)
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    runtime
        .refresh_memory_index_for_changed_paths(&changed_paths)
        .await
}

fn tool_invocation_surface(call: &ToolCall) -> Option<String> {
    match call.name.as_str() {
        "ApplyPatch" => Some(
            if call.input.is_string() {
                "freeform_grammar"
            } else {
                "function_json"
            }
            .to_string(),
        ),
        _ => None,
    }
}

fn tool_result_summary(result: &ToolResult) -> String {
    if let Some(error) = result.tool_error() {
        return super::helpers::truncate_text(&error.message, 200);
    }
    let summary = result
        .summary_text()
        .map(ToString::to_string)
        .or_else(|| summary_from_result_payload(result.envelope.result.as_ref()))
        .unwrap_or_else(|| result.envelope.tool_name.clone());
    super::helpers::truncate_text(&summary, 200)
}

fn summary_from_result_payload(value: Option<&Value>) -> Option<String> {
    let object = value?.as_object()?;
    for field in [
        "disposition",
        "retrieval_status",
        "status",
        "task_id",
        "agent_id",
        "id",
    ] {
        if let Some(summary) = object.get(field).and_then(Value::as_str) {
            return Some(summary.to_string());
        }
    }
    None
}

fn build_tool_spec_catalog() -> Result<ToolSpecCatalog> {
    Ok(ToolSpecCatalog {
        entries: tools::builtin_tool_definitions()?
            .into_iter()
            .map(|definition| catalog_entry(definition.family, definition.spec))
            .collect(),
    })
}

fn catalog_entry(family: ToolCapabilityFamily, spec: ToolSpec) -> ToolCatalogEntry {
    ToolCatalogEntry { family, spec }
}

#[cfg(test)]
mod tests {
    use super::{
        build_tool_spec_catalog, summary_from_result_payload, tool_result_summary, ToolRegistry,
    };
    use crate::{
        provider::{emitted_tool_json_schema, validate_emitted_tool_schema, ToolSchemaContract},
        tool::schema::validate_source_tool_schema,
        tool::{ToolError, ToolResult},
        types::ToolCapabilityFamily,
    };
    use serde_json::{json, Value};
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
            "AgentGet",
            "NotifyOperator",
            "SpawnAgent",
            "TaskInput",
            "TaskOutput",
            "CreateWorkItem",
            "PickWorkItem",
            "GetWorkItem",
            "ListWorkItems",
            "UpdateWorkItem",
            "CompleteWorkItem",
            "MemorySearch",
            "MemoryGet",
            "CreateExternalTrigger",
            "CancelExternalTrigger",
            "ApplyPatch",
            "ExecCommand",
        ] {
            assert!(names.iter().any(|name| name == expected));
        }

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

        let spawn_agent = specs
            .iter()
            .find(|spec| spec.name == "SpawnAgent")
            .expect("SpawnAgent should be present");
        assert!(spawn_agent.input_schema["properties"]
            .get("preset")
            .is_some());
        assert!(spawn_agent.input_schema["properties"]
            .get("agent_id")
            .is_some());
        assert!(spawn_agent.input_schema["properties"]
            .get("template")
            .is_some());
        assert!(spawn_agent.input_schema["properties"]["preset"]
            .to_string()
            .contains("public_named"));

        let all_of = spawn_agent
            .input_schema
            .get("allOf")
            .and_then(Value::as_array)
            .expect("SpawnAgent schema should define allOf rules");
        let contract_rules = all_of
            .iter()
            .filter(|rule| rule.get("if").is_some() && rule.get("then").is_some())
            .collect::<Vec<_>>();
        assert!(
            contract_rules.len() >= 2,
            "SpawnAgent schema should include preset contract if/then rules"
        );

        let requires_agent_id_for_public_named = contract_rules.iter().any(|variant| {
            variant
                .get("if")
                .and_then(|value| value.get("properties"))
                .and_then(|value| value.get("preset"))
                .and_then(|value| value.get("const"))
                .and_then(Value::as_str)
                .is_some_and(|preset| preset == "public_named")
                && variant
                    .get("then")
                    .and_then(|value| value.get("required"))
                    .and_then(Value::as_array)
                    .and_then(|required| {
                        required
                            .iter()
                            .find(|item| item.as_str() == Some("agent_id"))
                    })
                    .is_some()
        });
        assert!(requires_agent_id_for_public_named);

        let rejects_agent_id_for_default_or_private = contract_rules.iter().any(|variant| {
            variant
                .get("if")
                .and_then(|value| value.get("not"))
                .is_some()
                && variant
                    .get("then")
                    .and_then(|value| value.get("not"))
                    .and_then(|value| value.get("required"))
                    .and_then(Value::as_array)
                    .and_then(|required| {
                        required
                            .iter()
                            .find(|item| item.as_str() == Some("agent_id"))
                    })
                    .is_some()
        });
        assert!(rejects_agent_id_for_default_or_private);

        let constrains_workspace_mode_for_public_named = contract_rules.iter().any(|variant| {
            variant
                .get("if")
                .and_then(|value| value.get("properties"))
                .and_then(|value| value.get("preset"))
                .and_then(|value| value.get("const"))
                .and_then(Value::as_str)
                .is_some_and(|preset| preset == "public_named")
                && variant
                    .get("then")
                    .and_then(|value| value.get("properties"))
                    .and_then(|value| value.get("workspace_mode"))
                    .and_then(|value| value.get("enum"))
                    .and_then(Value::as_array)
                    .is_some_and(|modes| modes.iter().any(|mode| mode.as_str() == Some("inherit")))
        });
        assert!(constrains_workspace_mode_for_public_named);
    }

    #[test]
    fn summary_from_result_payload_prefers_small_status_fields() {
        assert_eq!(
            summary_from_result_payload(Some(&json!({
                "disposition": "completed",
                "stdout_preview": "ignored"
            }))),
            Some("completed".to_string())
        );
        assert_eq!(
            summary_from_result_payload(Some(&json!({
                "retrieval_status": "success",
                "task": {"status": "completed"}
            }))),
            Some("success".to_string())
        );
        assert_eq!(
            summary_from_result_payload(Some(&json!({"stdout_preview": "only"}))),
            None
        );
    }

    #[test]
    fn tool_result_summary_falls_back_to_tool_name_when_payload_has_no_small_fields() {
        let result = ToolResult::success(
            "AgentGet",
            json!({
                "profile": {"name": "default"},
                "active_tasks": [{"id": "task-1"}]
            }),
            None,
        );
        assert_eq!(tool_result_summary(&result), "AgentGet");

        let error = ToolResult::error("ExecCommand", ToolError::new("failure", "command exploded"));
        assert_eq!(tool_result_summary(&error), "command exploded");
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

        assert!(names.iter().any(|name| name == "TaskOutput"));
        assert!(names.iter().any(|name| name == "TaskInput"));
        assert!(names.iter().any(|name| name == "CreateWorkItem"));
        assert!(names.iter().any(|name| name == "PickWorkItem"));
        assert!(names.iter().any(|name| name == "GetWorkItem"));
        assert!(names.iter().any(|name| name == "ListWorkItems"));
        assert!(names.iter().any(|name| name == "UpdateWorkItem"));
        assert!(names.iter().any(|name| name == "CompleteWorkItem"));
        assert!(names.iter().any(|name| name == "MemorySearch"));
        assert!(names.iter().any(|name| name == "MemoryGet"));
        assert!(names.iter().any(|name| name == "CreateExternalTrigger"));
        assert!(names.iter().any(|name| name == "CancelExternalTrigger"));
        assert!(names.iter().any(|name| name == "ApplyPatch"));
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
        assert_eq!(
            family_for("NotifyOperator"),
            ToolCapabilityFamily::CoreAgent
        );
        assert_eq!(family_for("MemorySearch"), ToolCapabilityFamily::CoreAgent);
        assert_eq!(family_for("MemoryGet"), ToolCapabilityFamily::CoreAgent);
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
            family_for("SpawnAgent"),
            ToolCapabilityFamily::AgentCreation
        );
        assert_eq!(
            family_for("CreateExternalTrigger"),
            ToolCapabilityFamily::ExternalTrigger
        );
        assert_eq!(
            family_for("CancelExternalTrigger"),
            ToolCapabilityFamily::ExternalTrigger
        );
        assert_eq!(
            family_for("ExecCommand"),
            ToolCapabilityFamily::LocalEnvironment
        );
        assert_eq!(
            family_for("UseWorkspace"),
            ToolCapabilityFamily::LocalEnvironment
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
    fn emitted_tool_schemas_remain_strict_for_all_exposed_tools() {
        let registry = ToolRegistry::new(PathBuf::from("."));

        for spec in registry.tool_specs().unwrap() {
            validate_source_tool_schema(&spec.input_schema)
                .expect("source tool schema should stay valid");
            let final_schema =
                emitted_tool_json_schema(&spec.input_schema, ToolSchemaContract::Strict)
                    .expect("tool schema should normalize");
            validate_emitted_tool_schema(&final_schema, ToolSchemaContract::Strict)
                .expect("strict emitted schema should stay valid");
            assert_final_schema_shape(&final_schema);
        }
    }

    #[test]
    fn sleep_tool_schema_requires_reason_in_final_emitted_shape() {
        let registry = ToolRegistry::new(PathBuf::from("."));
        let sleep = registry
            .tool_specs()
            .unwrap()
            .into_iter()
            .find(|spec| spec.name == "Sleep")
            .expect("Sleep should be present");
        let final_schema =
            emitted_tool_json_schema(&sleep.input_schema, ToolSchemaContract::Strict)
                .expect("sleep schema");

        assert!(final_schema["properties"].get("reason").is_some());
        assert!(final_schema["properties"].get("duration_ms").is_some());
        assert_eq!(
            final_schema["required"],
            serde_json::json!(["duration_ms", "reason"])
        );
        assert_eq!(final_schema["additionalProperties"], Value::Bool(false));
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
        let final_schema =
            emitted_tool_json_schema(&enqueue.input_schema, ToolSchemaContract::Strict)
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
        assert!(priority_enum.iter().any(|value| value == "interrupt"));
        assert!(priority_enum.iter().any(Value::is_null));
    }

    #[test]
    fn sleep_duration_becomes_nullable_in_strict_emitted_shape() {
        let registry = ToolRegistry::new(PathBuf::from("."));
        let sleep = registry
            .tool_specs()
            .unwrap()
            .into_iter()
            .find(|spec| spec.name == "Sleep")
            .expect("Sleep should be present");
        let final_schema =
            emitted_tool_json_schema(&sleep.input_schema, ToolSchemaContract::Strict)
                .expect("sleep schema");
        let duration_ms = &final_schema["properties"]["duration_ms"];

        assert!(final_schema["required"]
            .as_array()
            .expect("required should be an array")
            .iter()
            .any(|value| value == "duration_ms"));
        let duration_types = duration_ms["type"]
            .as_array()
            .expect("duration_ms type should be an array")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(duration_types.contains(&"integer"));
        assert!(duration_types.contains(&"null"));
        assert_eq!(duration_ms["minimum"], Value::from(1.0));
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
}
