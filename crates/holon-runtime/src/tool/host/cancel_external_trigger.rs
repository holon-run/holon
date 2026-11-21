use super::serialize_success;
use crate::runtime::RuntimeHandle;
use crate::tool::helpers::parse_tool_args;
use crate::tool::helpers::validate_non_empty;
use crate::types::AuthorityClass;
use anyhow::Result;
use serde_json::Value;

pub(crate) use holon_tools::builtins::cancel_external_trigger::*;

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: CancelExternalTriggerArgs = parse_tool_args(NAME, input)?;
    let result = if let Some(external_trigger_id) = args.external_trigger_id {
        let external_trigger_id =
            validate_non_empty(external_trigger_id, NAME, "external_trigger_id")?;
        runtime
            .revoke_external_trigger(&external_trigger_id)
            .await?
    } else {
        anyhow::bail!("CancelExternalTrigger requires external_trigger_id");
    };
    serialize_success(NAME, &result)
}
