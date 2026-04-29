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
    pub(crate) waiting_intent_id: String,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::ExternalTrigger,
        spec: typed_spec::<CancelExternalTriggerArgs>(
            NAME,
            "Cancel a previously created waiting intent and revoke its external trigger capability.",
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
    let waiting_intent_id = validate_non_empty(args.waiting_intent_id, NAME, "waiting_intent_id")?;
    let result = runtime.cancel_waiting(&waiting_intent_id).await?;
    serialize_success(NAME, &result)
}
