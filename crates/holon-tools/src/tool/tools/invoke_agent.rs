use super::BuiltinToolDefinition;
use crate::tool::spec::typed_spec;
use crate::types::AgentModelRequest;
use crate::types::ChildAgentWorkspaceMode;
use crate::types::ToolCapabilityFamily;
use anyhow::Result;
use holon_domain::ToolError;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

pub const NAME: &str = crate::tool::names::INVOKE_AGENT;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum InvokeAgentToolTarget {
    ExistingAgent {
        agent_id: String,
        /// Defaults to 30 minutes; 0 means unlimited. Survives restart.
        reply_timeout_ms: Option<u64>,
    },
    NewSubagent {
        template: Option<String>,
        #[serde(default)]
        workspace_mode: ChildAgentWorkspaceMode,
        model: Option<AgentModelRequest>,
        /// Selects the child's inner turn loop. Absent keeps the native driver.
        #[serde(default)]
        execution_driver: Option<InvokeAgentExecutionDriver>,
    },
}

/// Execution-driver selection for a newly created subagent.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct InvokeAgentExecutionDriver {
    pub kind: String,
    #[serde(default)]
    pub harness_profile: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InvokeAgentArgs {
    pub target: InvokeAgentToolTarget,
    pub initial_message: String,
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::AgentCreation,
        spec: typed_spec::<InvokeAgentArgs>(
            NAME,
            include_str!("../tool_descriptions/invoke_agent.md"),
        )?,
    })
}

pub fn reject_legacy_agent_invocation_input(input: &Value) -> Result<()> {
    let Some(object) = input.as_object() else {
        return Ok(());
    };
    let top_level = ["preset", "profile_preset", "visibility", "ownership"]
        .into_iter()
        .find(|field| object.contains_key(*field));
    let target_level = object
        .get("target")
        .and_then(Value::as_object)
        .and_then(|target| {
            ["preset", "profile_preset", "visibility", "ownership"]
                .into_iter()
                .find(|field| target.contains_key(*field))
        });
    let Some(field) = top_level.or(target_level) else {
        return Ok(());
    };
    Err(anyhow::Error::from(
        ToolError::new(
            "legacy_agent_invocation_input",
            format!("InvokeAgent no longer accepts the legacy `{field}` field"),
        )
        .with_details(serde_json::json!({
            "tool_name": NAME,
            "field": field,
        }))
        .with_recovery_hint(
            "select target.kind=existing_agent or target.kind=new_subagent without legacy identity presets",
        ),
    ))
}

#[cfg(test)]
mod tests;
