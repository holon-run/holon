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
    "tool_execution:",
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
    let prefix = format!("{prefix}:");
    if prefix == "tool_execution:" {
        return validate_tool_execution_source_ref(&source_ref, suffix);
    }

    if suffix.is_empty() {
        return Err(invalid_source_ref_error(
            &source_ref,
            "missing source_ref identifier",
        ));
    }
    if !suffix
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
    {
        return Err(invalid_source_ref_error(
            &source_ref,
            "source_ref identifier must be an opaque id, not a path, URL, or query",
        ));
    }

    if !ALLOWED_SOURCE_REF_PREFIXES.contains(&prefix.as_str()) {
        return Err(invalid_source_ref_error(
            &source_ref,
            "unsupported source_ref prefix",
        ));
    }

    Ok(source_ref)
}

fn validate_tool_execution_source_ref(source_ref: &str, suffix: &str) -> Result<String> {
    let parts = suffix.split(':').collect::<Vec<_>>();
    let valid = match parts.as_slice() {
        [tool_execution_id, "cmd"] => valid_source_ref_segment(tool_execution_id),
        [tool_execution_id, "batch_item", index, "cmd"] => {
            valid_source_ref_segment(tool_execution_id)
                && !index.is_empty()
                && index.chars().all(|ch| ch.is_ascii_digit())
        }
        _ => false,
    };
    if !valid {
        return Err(invalid_source_ref_error(
            source_ref,
            "tool_execution source_ref must match tool_execution:<id>:cmd or tool_execution:<id>:batch_item:<index>:cmd",
        ));
    }
    Ok(source_ref.to_string())
}

fn valid_source_ref_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
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
    use std::sync::Arc;

    use chrono::Utc;
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;
    use crate::{
        context::ContextConfig,
        provider::StubProvider,
        runtime::RuntimeHandle,
        types::{ToolExecutionRecord, ToolExecutionStatus, TrustLevel},
    };

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
            "tool_execution:tool-123:cmd",
            "tool_execution:tool-123:batch_item:2:cmd",
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
            "brief:/Users/jolestar/project/README.md",
            "brief:https://example.com/memory",
            "episode:../ledger/episode-1",
            "work_item:work_123?raw=true",
            "tool_execution:tool-123",
            "tool_execution:tool-123:batch_item:abc:cmd",
            "tool_execution:tool-123:batch_item:2",
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

    #[tokio::test]
    async fn memory_get_tool_accepts_command_receipt_source_refs() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            ContextConfig::default(),
        )
        .unwrap();
        let command = "python - <<'PY'\nprint('memory_get_tool_receipt_1246')\nPY";
        runtime
            .storage()
            .append_tool_execution(&ToolExecutionRecord {
                id: "tool-get-1246".into(),
                agent_id: "default".into(),
                work_item_id: None,
                turn_index: 1,
                tool_name: "ExecCommand".into(),
                created_at: Utc::now(),
                completed_at: Some(Utc::now()),
                duration_ms: 10,
                trust: TrustLevel::TrustedOperator,
                status: ToolExecutionStatus::Success,
                input: json!({
                    "cmd": command,
                    "workdir": "src",
                    "yield_time_ms": 1000,
                    "max_output_tokens": 1200
                }),
                output: json!({"exit_code": 0}),
                summary: "command exited with status 0".into(),
                invocation_surface: None,
            })
            .unwrap();

        let result = execute(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &json!({
                "source_ref": "tool_execution:tool-get-1246:cmd"
            }),
        )
        .await
        .unwrap();
        let content = result.envelope.result.unwrap()["memory"]["content"]
            .as_str()
            .unwrap()
            .to_string();

        assert!(content.contains("memory_get_tool_receipt_1246"));
        assert!(content.contains("\"workdir\": \"src\""));
        assert!(content.contains("\"yield_time_ms\": 1000"));
        assert!(content.contains("\"max_output_tokens\": 1200"));
    }
}
