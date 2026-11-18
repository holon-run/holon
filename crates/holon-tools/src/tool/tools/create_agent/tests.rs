use serde_json::json;

use crate::tool::helpers::parse_tool_args;

use super::{reject_legacy_agent_creation_input, CreateAgentArgs, NAME};

#[test]
fn create_agent_rejects_caller_controlled_provenance() {
    let error = parse_tool_args::<CreateAgentArgs>(
        NAME,
        &json!({
            "agent_id": "release-bot",
            "authority_class": "operator",
            "lineage_parent_agent_id": "root-agent"
        }),
    )
    .expect_err("trusted caller context must not be request-controlled");

    assert!(error.to_string().contains("unknown field"));
}

#[test]
fn create_agent_rejects_unknown_nested_model_fields() {
    let error = parse_tool_args::<CreateAgentArgs>(
        NAME,
        &json!({
            "agent_id": "release-bot",
            "model": {
                "provider": "anthropic",
                "model": "claude-haiku-4-5",
                "max_output_token": 1000
            }
        }),
    )
    .expect_err("nested model typos should be rejected");

    assert!(error.to_string().contains("max_output_token"));
}

#[test]
fn create_agent_rejects_legacy_identity_fields_with_stable_error() {
    let error = reject_legacy_agent_creation_input(&json!({
        "agent_id": "release-bot",
        "profile_preset": "public_named"
    }))
    .expect_err("legacy identity fields should have a stable typed error");

    let tool_error = error
        .downcast_ref::<crate::tool::ToolError>()
        .expect("typed tool error");
    assert_eq!(tool_error.kind, "legacy_agent_creation_input");
}
