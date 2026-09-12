use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    memory::refs::{RuntimeRef, ALLOWED_SOURCE_REF_PREFIXES},
    runtime::RuntimeHandle,
    tool::{helpers::invalid_tool_input, spec::typed_spec, ToolError},
    types::{AuthorityClass, ToolCapabilityFamily},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::parse_tool_args;

pub(crate) const NAME: &str = crate::tool::names::MEMORY_GET;
const MAX_CHARS_MAX: usize = 50_000;

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
            include_str!("../tool_descriptions/memory_get.md"),
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: MemoryGetArgs = parse_tool_args(NAME, input)?;
    let source_ref = validate_source_ref(args.source_ref)?;
    let max_chars = validate_max_chars(args.max_chars)?;
    let Some(mut memory) = runtime.get_memory_snapshot(&source_ref).await? else {
        return Err(ToolError::new(
            "memory_source_not_found",
            format!("memory source `{source_ref}` was not found"),
        )
        .with_details(json!({
            "source_ref": source_ref,
            "allowed_source_ref_prefixes": ALLOWED_SOURCE_REF_PREFIXES,
            "reason": "source_ref was syntactically valid but is not present in the current visible runtime sources",
        }))
        .with_recovery_hint(
            "copy an available prompt source_ref such as brief_ref/cmd_ref/stdout_ref/stderr_ref/output_ref verbatim, or call MemorySearch to discover a visible source_ref",
        )
        .into());
    };
    let source_chars = memory.content.chars().count();
    let artifact = match runtime
        .persist_tool_text_artifact("memory-source", &memory.content)
        .await
    {
        Ok(path) => {
            json!({"path": path, "complete": !memory.truncated, "reason": memory.metadata.get("source_incomplete_reason"), "chars": source_chars, "encoding": "utf-8", "range_unit": "unicode_scalar"})
        }
        Err(error) => {
            json!({"complete": false, "reason": format!("source artifact could not be saved: {error}")})
        }
    };
    let limit = max_chars.unwrap_or(12_000);
    if source_chars > limit {
        memory.content = memory.content.chars().take(limit).collect();
        memory.truncated = true;
    }
    let mut result = serialize_success(NAME, &MemoryGetResponse { memory })?;
    result
        .envelope
        .result
        .as_mut()
        .expect("serialized memory result")["memory"]["source_artifact"] = artifact;
    result.envelope.summary_text = Some(format!(
        "Read {} of {source_chars} source characters.",
        source_chars.min(limit)
    ));
    Ok(result)
}

fn validate_source_ref(source_ref: String) -> Result<String> {
    RuntimeRef::parse(&source_ref)
        .map(|parsed| parsed.source_ref())
        .map_err(|error| invalid_source_ref_error(&source_ref, error.validation_error()))
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
        "MemoryGet `source_ref` must be a standardized runtime ref",
        json!({
            "field": "source_ref",
            "source_ref": source_ref,
            "validation_error": validation_error,
            "allowed_source_ref_prefixes": ALLOWED_SOURCE_REF_PREFIXES,
        }),
        "copy an available prompt source_ref verbatim, or call MemorySearch to discover a visible source_ref; use ExecCommand for workspace files or skill docs instead of MemoryGet",
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
        types::{
            AuthorityClass, TaskKind, TaskRecord, TaskStatus, ToolExecutionRecord,
            ToolExecutionStatus,
        },
    };

    fn tool_error(error: anyhow::Error) -> crate::tool::ToolError {
        error
            .downcast_ref::<crate::tool::ToolError>()
            .expect("tool error")
            .clone()
    }

    #[tokio::test]
    async fn full_source_artifact_survives_inline_and_command_projection_limits() {
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
        let source = format!(
            "{}\n{}\r\nEND 🦀",
            "中文🦀\"\\ ".repeat(9000),
            "line\n".repeat(100)
        );
        let path = crate::agent_template::agent_memory_self_path(runtime.storage().data_dir());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &source).unwrap();
        let memory_result = execute(
            &runtime,
            "default",
            &AuthorityClass::OperatorInstruction,
            &json!({"source_ref": "agent_memory:self", "max_chars": 50_000}),
        )
        .await
        .unwrap();
        let value = memory_result.envelope.result.as_ref().unwrap();
        let memory = &value["memory"];
        assert_eq!(memory["content"].as_str().unwrap().chars().count(), 50_000);
        assert_eq!(memory["truncated"], true);
        assert_eq!(memory["source_artifact"]["complete"], true);
        let artifact = memory["source_artifact"]["path"].as_str().unwrap();
        assert_eq!(std::fs::read_to_string(artifact).unwrap(), source);
        let rendered = super::super::render_tool_result_for_model_with_context(
            &memory_result,
            &super::super::ToolModelRenderContext {
                tool_execution_id: "memory-full-source",
                tool_output_budget_estimated_tokens: 800,
            },
        )
        .unwrap();
        let projected: Value = serde_json::from_str(&rendered).unwrap();
        assert!(source.starts_with(
            projected["result"]["memory"]["preview"]["content"]
                .as_str()
                .unwrap()
        ));

        let mut rebuilt = String::new();
        let mut offset = 0;
        let mut batch = false;
        let total = source.chars().count();
        let execution_context = crate::tool::spec::ToolExecutionContext::default();
        while offset < total {
            let code = format!("import sys; sys.stdout.write(open({}, encoding='utf-8', newline='').read()[{}:{}])",
                serde_json::to_string(artifact).unwrap(), offset, offset + 8000);
            let cmd = format!("python3 -c '{}'", code.replace('\'', "'\\''"));
            batch = !batch;
            let command_result = if batch {
                super::super::exec_command_batch::execute(
                    &runtime,
                    "default",
                    &AuthorityClass::OperatorInstruction,
                    &json!({"items": [{"cmd": cmd}], "max_output_tokens": 4000}),
                    &execution_context,
                )
                .await
                .unwrap()
            } else {
                super::super::exec_command::execute(
                    &runtime,
                    "default",
                    &AuthorityClass::OperatorInstruction,
                    &json!({"cmd": cmd, "max_output_tokens": 4000}),
                    &execution_context,
                )
                .await
                .unwrap()
            };
            let rendered = super::super::render_tool_result_for_model_with_context(
                &command_result,
                &super::super::ToolModelRenderContext {
                    tool_execution_id: "range-read",
                    tool_output_budget_estimated_tokens: 1000,
                },
            )
            .unwrap();
            // Force JSON projection for the final small chunk as well.
            let projected: Value = serde_json::from_str(&rendered).unwrap_or_else(|_| {
                serde_json::from_str(
                    &super::super::semantic_projection::project(
                        &command_result.envelope,
                        None,
                        1000,
                    )
                    .unwrap(),
                )
                .unwrap()
            });
            let output = if batch {
                &projected["result"]["items"][0]["result"]
            } else {
                &projected["result"]
            };
            let prefix = output["stdout_preview"]["content"].as_str().unwrap();
            let shown = output["stdout_preview"]["shown_chars"].as_u64().unwrap() as usize;
            assert!(shown > 0);
            assert_eq!(shown, prefix.chars().count());
            rebuilt.push_str(prefix);
            offset += shown;
        }
        assert_eq!(rebuilt, source);
    }

    #[tokio::test]
    async fn source_artifact_write_failure_is_explicit_without_losing_inline_content() {
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
        let path = crate::agent_template::agent_memory_self_path(runtime.storage().data_dir());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "available source text").unwrap();
        std::fs::write(
            runtime.storage().data_dir().join("tool-artifacts"),
            "not a directory",
        )
        .unwrap();
        let result = execute(
            &runtime,
            "default",
            &AuthorityClass::OperatorInstruction,
            &json!({"source_ref": "agent_memory:self"}),
        )
        .await
        .unwrap();
        let memory = &result.envelope.result.as_ref().unwrap()["memory"];
        assert_eq!(memory["content"], "available source text");
        assert_eq!(memory["source_artifact"]["complete"], false);
        assert!(memory["source_artifact"].get("path").is_none());
        assert!(memory["source_artifact"]["reason"]
            .as_str()
            .unwrap()
            .contains("could not be saved"));
    }

    #[test]
    fn source_ref_accepts_known_memory_prefixes() {
        for source_ref in [
            "agent_memory:self",
            "workspace_profile:ws-123",
            "brief:abc",
            "turn:turn_123",
            "episode:ep_123",
            "work_item:work_123",
            "task:task_123",
            "tool_execution:tool-123:cmd",
            "tool_execution:tool-123:stdout",
            "tool_execution:tool-123:stderr",
            "tool_execution:tool-123:output",
            "tool_execution:tool-123:batch_item:2:cmd",
            "tool_execution:tool-123:batch_item:2:stdout",
            "tool_execution:tool-123:batch_item:2:stderr",
            "tool_execution:tool-123:batch_item:2:output",
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
            "turn:../ledger/turn-1",
            "episode:../ledger/episode-1",
            "work_item:work_123?raw=true",
            "tool_execution:tool-123",
            "tool_execution:tool-123:batch_item:0:cmd",
            "tool_execution:tool-123:batch_item:02:cmd",
            "tool_execution:tool-123:batch_item:abc:cmd",
            "tool_execution:tool-123:batch_item:2",
            "tool_execution:tool-123:batch_item:2:artifact",
            "tool_execution:tool-123:artifact",
        ] {
            let error = tool_error(validate_source_ref(source_ref.to_string()).unwrap_err());
            assert_eq!(error.kind, "invalid_tool_input");
            assert_eq!(
                error.recovery_hint.as_deref(),
                Some("copy an available prompt source_ref verbatim, or call MemorySearch to discover a visible source_ref; use ExecCommand for workspace files or skill docs instead of MemoryGet")
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
                turn_index: 0,
                turn_id: None,
                tool_name: "ExecCommand".into(),
                created_at: Utc::now(),
                completed_at: Some(Utc::now()),
                duration_ms: 10,
                authority_class: AuthorityClass::OperatorInstruction,
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
            &AuthorityClass::OperatorInstruction,
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

    #[tokio::test]
    async fn memory_get_tool_accepts_command_output_source_refs() {
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
        runtime
            .storage()
            .append_tool_execution(&ToolExecutionRecord {
                id: "tool-output-1246".into(),
                agent_id: "default".into(),
                work_item_id: None,
                turn_index: 0,
                turn_id: None,
                tool_name: "ExecCommand".into(),
                created_at: Utc::now(),
                completed_at: Some(Utc::now()),
                duration_ms: 10,
                authority_class: AuthorityClass::OperatorInstruction,
                status: ToolExecutionStatus::Success,
                input: json!({"cmd": "printf memory_get_stdout_1246"}),
                output: json!({
                    "disposition": "completed",
                    "exit_status": 0,
                    "stdout_preview": "memory_get_stdout_1246\n",
                    "stderr_preview": "",
                    "truncated": false,
                    "artifacts": []
                }),
                summary: "command exited with status 0".into(),
                invocation_surface: None,
            })
            .unwrap();

        let result = execute(
            &runtime,
            "default",
            &AuthorityClass::OperatorInstruction,
            &json!({
                "source_ref": "tool_execution:tool-output-1246:stdout"
            }),
        )
        .await
        .unwrap();
        let content = result.envelope.result.unwrap()["memory"]["content"]
            .as_str()
            .unwrap()
            .to_string();

        assert_eq!(content, "memory_get_stdout_1246\n");
    }

    #[tokio::test]
    async fn memory_get_tool_accepts_generic_tool_output_source_refs() {
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
        runtime
            .storage()
            .append_tool_execution(&ToolExecutionRecord {
                id: "tool-generic-get-1246".into(),
                agent_id: "default".into(),
                work_item_id: None,
                turn_index: 0,
                turn_id: None,
                tool_name: "ViewImage".into(),
                created_at: Utc::now(),
                completed_at: Some(Utc::now()),
                duration_ms: 10,
                authority_class: AuthorityClass::OperatorInstruction,
                status: ToolExecutionStatus::Success,
                input: json!({"path": "fixtures/pixel.png", "prompt": "inspect"}),
                output: json!({
                    "envelope": {
                        "result": {
                            "visual_observation": "memory_get_generic_tool_output_1246"
                        }
                    },
                    "is_error": false
                }),
                summary: "validated image metadata".into(),
                invocation_surface: None,
            })
            .unwrap();

        let result = execute(
            &runtime,
            "default",
            &AuthorityClass::OperatorInstruction,
            &json!({
                "source_ref": "tool_execution:tool-generic-get-1246:output"
            }),
        )
        .await
        .unwrap();
        let content = result.envelope.result.unwrap()["memory"]["content"]
            .as_str()
            .unwrap()
            .to_string();

        assert!(content.contains("\"source_type\": \"tool_execution_output\""));
        assert!(content.contains("\"tool_name\": \"ViewImage\""));
        assert!(content.contains("memory_get_generic_tool_output_1246"));
    }

    #[tokio::test]
    async fn memory_get_tool_accepts_exec_command_batch_top_level_output_ref() {
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
        runtime
            .storage()
            .append_tool_execution(&ToolExecutionRecord {
                id: "tool-batch-get-1246".into(),
                agent_id: "default".into(),
                work_item_id: None,
                turn_index: 0,
                turn_id: None,
                tool_name: "ExecCommandBatch".into(),
                created_at: Utc::now(),
                completed_at: Some(Utc::now()),
                duration_ms: 10,
                authority_class: AuthorityClass::OperatorInstruction,
                status: ToolExecutionStatus::Success,
                input: json!({
                    "items": [
                        {"cmd": "echo first"},
                        {"cmd": "echo second"}
                    ]
                }),
                output: json!({
                    "envelope": {
                        "result": {
                            "completed_count": 2,
                            "item_count": 2,
                            "items": [
                                {"index": 1, "result": {"stdout_preview": "memory_get_batch_first_1246\n"}},
                                {"index": 2, "result": {"stdout_preview": "memory_get_batch_second_1246\n"}}
                            ]
                        }
                    },
                    "is_error": false
                }),
                summary: "ExecCommandBatch completed 2/2 items".into(),
                invocation_surface: None,
            })
            .unwrap();

        let result = execute(
            &runtime,
            "default",
            &AuthorityClass::OperatorInstruction,
            &json!({
                "source_ref": "tool_execution:tool-batch-get-1246:output"
            }),
        )
        .await
        .unwrap();
        let content = result.envelope.result.unwrap()["memory"]["content"]
            .as_str()
            .unwrap()
            .to_string();

        assert!(content.contains("\"tool_name\": \"ExecCommandBatch\""));
        assert!(content.contains("memory_get_batch_first_1246"));
        assert!(content.contains("memory_get_batch_second_1246"));
    }

    #[tokio::test]
    async fn memory_get_tool_accepts_task_source_refs() {
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

        runtime
            .storage()
            .append_task(&TaskRecord {
                id: "task-get-1246".into(),
                agent_id: "default".into(),
                kind: TaskKind::CommandTask,
                status: TaskStatus::Completed,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                parent_message_id: None,
                work_item_id: Some("wi-get-1246".into()),
                summary: Some("task get memory source ref".into()),
                detail: Some(serde_json::json!({"cmd": "echo task-get-1246"})),
                recovery: None,
            })
            .unwrap();

        let result = execute(
            &runtime,
            "default",
            &AuthorityClass::OperatorInstruction,
            &json!({
                "source_ref": "task:task-get-1246"
            }),
        )
        .await
        .unwrap();
        let content = result.envelope.result.unwrap()["memory"]["content"]
            .as_str()
            .unwrap()
            .to_string();

        assert!(content.contains("task_id: task-get-1246"));
        assert!(content.contains("summary: task get memory source ref"));
        assert!(content.contains("cmd: echo task-get-1246"));
        assert!(content.contains("work_item_id: wi-get-1246"));
    }
}
