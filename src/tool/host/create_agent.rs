use super::serialize_success;
use crate::host_registry::validate_agent_id_format;
use crate::runtime::RuntimeHandle;
use crate::tool::helpers::invalid_tool_input;
use crate::tool::helpers::normalize_optional_non_empty;
use crate::tool::helpers::parse_tool_args;
use crate::tool::helpers::validate_non_empty;
use crate::types::AuthorityClass;
use crate::types::CreateAgentRequest;
use anyhow::Result;
use serde_json::json;
use serde_json::Value;

pub(crate) use holon_tools::builtins::create_agent::*;

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _caller_agent_id: &str,
    authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    reject_legacy_agent_creation_input(input)?;
    let args: CreateAgentArgs = parse_tool_args(NAME, input)?;
    let agent_id = validate_non_empty(args.agent_id, NAME, "agent_id")?;
    if let Err(error) = validate_agent_id_format(&agent_id) {
        return Err(invalid_tool_input(
            NAME,
            format!("CreateAgent requires a valid `agent_id`: {error}"),
            json!({
                "field": "agent_id",
                "validation_error": error.to_string(),
            }),
            "use a single ASCII agent id containing only letters, digits, '.', '-', or '_'",
        ));
    }
    let model_resolution = runtime
        .resolve_agent_model_request(NAME, args.model)
        .await?;
    let result = runtime
        .agent_creation_service()
        .create(CreateAgentRequest {
            agent_id,
            name: normalize_optional_non_empty(args.name),
            template: normalize_optional_non_empty(args.template),
            initial_message: normalize_optional_non_empty(args.initial_message),
            authority_class: authority_class.clone(),
            model_resolution: Some(model_resolution),
            lineage_parent_agent_id: None,
            inherit_parent_runtime: true,
        })
        .await?;
    serialize_success(NAME, &result)
}
