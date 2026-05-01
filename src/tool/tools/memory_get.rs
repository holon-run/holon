use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    runtime::RuntimeHandle,
    tool::{helpers::invalid_tool_input, spec::typed_spec, ToolError},
    types::{ToolCapabilityFamily, TrustLevel},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::{parse_tool_args, validate_non_empty};

pub(crate) const NAME: &str = "MemoryGet";
const MAX_CHARS_MAX: usize = 50_000;
const ALLOWED_SOURCE_REF_PREFIXES: &[&str] = &[
    "agent_memory:",
    "workspace_profile:",
    "brief:",
    "episode:",
    "work_item:",
];

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct MemoryGetArgs {
    pub(crate) source_ref: String,
    #[schemars(range(min = 1, max = 50000))]
    pub(crate) max_chars: Option<usize>,
}

#[derive(Serialize)]
struct MemoryGetResponse {
    memory: crate::memory::MemoryGetResult,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<MemoryGetArgs>(
            NAME,
            "Fetch exact bounded Holon memory content by a source_ref copied verbatim from MemorySearch.results[].source_ref. Do not invent source_ref values or use MemoryGet for skill paths, file paths, or URLs.",
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _trust: &TrustLevel,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: MemoryGetArgs = parse_tool_args(NAME, input)?;
    let source_ref = validate_source_ref(args.source_ref)?;
    let max_chars = validate_max_chars(args.max_chars)?;
    let Some(memory) = runtime.get_memory(&source_ref, max_chars).await? else {
        return Err(ToolError::new(
            "memory_source_not_found",
            format!("memory source `{source_ref}` was not found"),
        )
        .with_details(json!({
            "source_ref": source_ref,
            "allowed_source_ref_prefixes": ALLOWED_SOURCE_REF_PREFIXES,
            "reason": "source_ref was syntactically valid but is not present in the current visible memory index",
        }))
        .with_recovery_hint(
            "call MemorySearch again and pass one of its returned source_ref values verbatim",
        )
        .into());
    };
    serialize_success(NAME, &MemoryGetResponse { memory })
}

fn validate_source_ref(source_ref: String) -> Result<String> {
    let source_ref = validate_non_empty(source_ref, NAME, "source_ref")?;
    if source_ref.chars().any(char::is_whitespace) {
        return Err(invalid_tool_input(
            NAME,
            "MemoryGet `source_ref` must be a single opaque handle without whitespace",
            json!({
                "field": "source_ref",
                "source_ref": source_ref,
                "validation_error": "must not contain whitespace",
            }),
            "copy a source_ref exactly from MemorySearch.results[].source_ref; do not paste snippets or paths",
        ));
    }

    let Some((prefix, suffix)) = source_ref.split_once(':') else {
        return Err(invalid_source_ref_error(
            &source_ref,
            "missing source_ref prefix",
        ));
    };
    if suffix.is_empty() {
        return Err(invalid_source_ref_error(
            &source_ref,
            "missing source_ref identifier",
        ));
    }

    let prefix = format!("{prefix}:");
    if !ALLOWED_SOURCE_REF_PREFIXES.contains(&prefix.as_str()) {
        return Err(invalid_source_ref_error(
            &source_ref,
            "unsupported source_ref prefix",
        ));
    }

    Ok(source_ref)
}

fn validate_max_chars(max_chars: Option<usize>) -> Result<Option<usize>> {
    let Some(max_chars) = max_chars else {
        return Ok(None);
    };
    if !(1..=MAX_CHARS_MAX).contains(&max_chars) {
        return Err(invalid_tool_input(
            NAME,
            "MemoryGet `max_chars` must be between 1 and 50000 when provided",
            json!({
                "field": "max_chars",
                "max_chars": max_chars,
                "validation_error": "out of range",
                "minimum": 1,
                "maximum": MAX_CHARS_MAX,
            }),
            "omit `max_chars` for the default bound, or provide an integer from 1 through 50000",
        ));
    }
    Ok(Some(max_chars))
}

fn invalid_source_ref_error(source_ref: &str, validation_error: &'static str) -> anyhow::Error {
    invalid_tool_input(
        NAME,
        "MemoryGet `source_ref` must be copied from MemorySearch results",
        json!({
            "field": "source_ref",
            "source_ref": source_ref,
            "validation_error": validation_error,
            "allowed_source_ref_prefixes": ALLOWED_SOURCE_REF_PREFIXES,
        }),
        "call MemorySearch first and copy one returned source_ref verbatim; use ExecCommand for workspace files or skill docs instead of MemoryGet",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_error(error: anyhow::Error) -> crate::tool::ToolError {
        error
            .downcast_ref::<crate::tool::ToolError>()
            .expect("tool error")
            .clone()
    }

    #[test]
    fn source_ref_accepts_known_memory_prefixes() {
        for source_ref in [
            "agent_memory:self",
            "workspace_profile:ws-123",
            "brief:abc",
            "episode:ep_123",
            "work_item:work_123",
        ] {
            assert_eq!(
                validate_source_ref(source_ref.to_string()).unwrap(),
                source_ref
            );
        }
    }

    #[test]
    fn source_ref_rejects_paths_and_unknown_prefixes() {
        for source_ref in [
            "/Users/jolestar/.agents/skills/agentinbox/SKILL.md",
            "skill:/Users/jolestar/.agents/skills/agentinbox/SKILL.md",
            "skill.md:/Users/jolestar/.agents/skills/agentinbox/SKILL.md",
            "agentinbox:///SKILL.md",
            "memory:invalid-ref-123",
        ] {
            let error = tool_error(validate_source_ref(source_ref.to_string()).unwrap_err());
            assert_eq!(error.kind, "invalid_tool_input");
            assert_eq!(
                error.recovery_hint.as_deref(),
                Some("call MemorySearch first and copy one returned source_ref verbatim; use ExecCommand for workspace files or skill docs instead of MemoryGet")
            );
        }
    }

    #[test]
    fn source_ref_rejects_empty_suffix_and_whitespace() {
        let empty_suffix = tool_error(validate_source_ref("brief:".to_string()).unwrap_err());
        assert_eq!(empty_suffix.kind, "invalid_tool_input");
        assert!(empty_suffix
            .details
            .as_ref()
            .and_then(|details| details.get("validation_error"))
            .and_then(Value::as_str)
            .is_some_and(|message| message.contains("missing")));

        let whitespace = tool_error(validate_source_ref("brief:abc def".to_string()).unwrap_err());
        assert_eq!(whitespace.kind, "invalid_tool_input");
        assert!(whitespace
            .details
            .as_ref()
            .and_then(|details| details.get("validation_error"))
            .and_then(Value::as_str)
            .is_some_and(|message| message.contains("whitespace")));
    }

    #[test]
    fn max_chars_accepts_omitted_and_bounded_values() {
        assert_eq!(validate_max_chars(None).unwrap(), None);
        assert_eq!(validate_max_chars(Some(1)).unwrap(), Some(1));
        assert_eq!(
            validate_max_chars(Some(MAX_CHARS_MAX)).unwrap(),
            Some(MAX_CHARS_MAX)
        );
    }

    #[test]
    fn max_chars_rejects_zero_and_oversized_values() {
        for max_chars in [0, MAX_CHARS_MAX + 1] {
            let error = tool_error(validate_max_chars(Some(max_chars)).unwrap_err());
            assert_eq!(error.kind, "invalid_tool_input");
            assert!(error
                .details
                .as_ref()
                .and_then(|details| details.get("maximum"))
                .is_some());
        }
    }
}
