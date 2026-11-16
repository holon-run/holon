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
    },
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
mod tests {
    use serde_json::json;

    use crate::tool::helpers::parse_tool_args;

    use super::{reject_legacy_agent_invocation_input, InvokeAgentArgs, NAME};

    #[test]
    fn invoke_agent_reply_timeout_is_optional_zero_or_positive_only() {
        for timeout in [None, Some(0_u64), Some(3_600_000)] {
            let mut input = json!({
                "target": {"kind": "existing_agent", "agent_id": "peer"},
                "initial_message": "answer",
            });
            if let Some(timeout) = timeout {
                input["target"]["reply_timeout_ms"] = json!(timeout);
            }
            let parsed = parse_tool_args::<InvokeAgentArgs>(NAME, &input).unwrap();
            assert!(
                matches!(parsed.target, super::InvokeAgentToolTarget::ExistingAgent {
                reply_timeout_ms, ..
            } if reply_timeout_ms == timeout)
            );
        }
        let invalid = json!({
            "target": {"kind": "existing_agent", "agent_id": "peer", "reply_timeout_ms": -1},
            "initial_message": "answer",
        });
        assert!(parse_tool_args::<InvokeAgentArgs>(NAME, &invalid).is_err());
    }

    #[test]
    fn invoke_agent_contract_accepts_exactly_the_two_target_variants() {
        parse_tool_args::<InvokeAgentArgs>(
            NAME,
            &json!({
                "target": {
                    "kind": "existing_agent",
                    "agent_id": "release-bot"
                },
                "initial_message": "prepare the release"
            }),
        )
        .expect("existing agent target should deserialize");
        parse_tool_args::<InvokeAgentArgs>(
            NAME,
            &json!({
                "target": {
                    "kind": "new_subagent",
                    "workspace_mode": "worktree"
                },
                "initial_message": "review the implementation"
            }),
        )
        .expect("new subagent target should deserialize");
    }

    #[test]
    fn invoke_agent_rejects_target_field_mixing() {
        let error = parse_tool_args::<InvokeAgentArgs>(
            NAME,
            &json!({
                "target": {
                    "kind": "existing_agent",
                    "agent_id": "release-bot",
                    "workspace_mode": "worktree"
                },
                "initial_message": "prepare the release"
            }),
        )
        .expect_err("existing targets must not accept new-subagent options");

        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn invoke_agent_rejects_caller_controlled_provenance() {
        let error = parse_tool_args::<InvokeAgentArgs>(
            NAME,
            &json!({
                "target": {
                    "kind": "existing_agent",
                    "agent_id": "release-bot"
                },
                "initial_message": "prepare the release",
                "authority_class": "operator",
                "origin": "operator"
            }),
        )
        .expect_err("trusted caller context must not be request-controlled");

        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn invoke_agent_rejects_legacy_identity_fields_with_stable_error() {
        let error = reject_legacy_agent_invocation_input(&json!({
            "initial_message": "do work",
            "target": {
                "kind": "new_subagent",
                "preset": "private_child"
            }
        }))
        .expect_err("legacy identity fields should have a stable typed error");

        let tool_error = error
            .downcast_ref::<crate::tool::ToolError>()
            .expect("typed tool error");
        assert_eq!(tool_error.kind, "legacy_agent_invocation_input");
    }
}
