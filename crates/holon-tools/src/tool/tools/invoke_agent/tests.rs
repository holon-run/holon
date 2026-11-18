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
