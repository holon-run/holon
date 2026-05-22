use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::spec::typed_spec,
    types::{ToolCapabilityFamily, TrustLevel},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::{parse_tool_args, validate_non_empty};

pub(crate) const NAME: &str = "CancelExternalTrigger";

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CancelExternalTriggerArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) external_trigger_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) waiting_intent_id: Option<String>,
}

#[allow(dead_code)]
pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::ExternalTrigger,
        spec: typed_spec::<CancelExternalTriggerArgs>(
            NAME,
            "Revoke an external trigger ingress capability by external_trigger_id.",
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _trust: &TrustLevel,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: CancelExternalTriggerArgs = parse_tool_args(NAME, input)?;
    let result = if let Some(external_trigger_id) = args.external_trigger_id {
        let external_trigger_id =
            validate_non_empty(external_trigger_id, NAME, "external_trigger_id")?;
        runtime
            .revoke_external_trigger(&external_trigger_id)
            .await?
    } else if let Some(waiting_intent_id) = args.waiting_intent_id {
        let waiting_intent_id = validate_non_empty(waiting_intent_id, NAME, "waiting_intent_id")?;
        runtime
            .revoke_external_trigger_for_waiting_intent(&waiting_intent_id)
            .await?
    } else {
        anyhow::bail!("CancelExternalTrigger requires external_trigger_id");
    };
    serialize_success(NAME, &result)
}
