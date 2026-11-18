//! Tool routing and dispatch logic.

use anyhow::{anyhow, Result};
use holon_domain::ApplyPatchSurface;
use serde_json::json;
use serde_json::Value;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::LazyLock;

use crate::{
    tool::ToolError,
    types::{AuthorityClass, ToolCapabilityFamily, ToolExecutionRecord, ToolExecutionStatus},
};

use super::{
    helpers::capture_tool_input_coercion,
    spec::{ToolCall, ToolExecutionContext, ToolSpec},
    tools,
};

use std::{future::Future, pin::Pin};

use super::spec::ToolOutcome;
use super::tools::BuiltinTool;

pub type ToolFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// Capabilities used by the router. The application owns their execution state.
pub trait ToolRuntime: Send + Sync {
    type State: Default + Send + Sync;

    fn allows_tool(&self, family: ToolCapabilityFamily) -> ToolFuture<'_, bool>;
    fn invoke_builtin<'a>(
        &'a self,
        tool: BuiltinTool,
        agent_id: &'a str,
        authority_class: &'a AuthorityClass,
        call: &'a ToolCall,
        context: &'a ToolExecutionContext,
    ) -> ToolFuture<'a, ToolOutcome<Self::State>>;
    fn persist_tool_text_artifact<'a>(
        &'a self,
        kind: &'a str,
        text: &'a str,
    ) -> ToolFuture<'a, String>;
    fn record_tool_execution(
        &self,
        name: &str,
        elapsed: std::time::Duration,
        output_bytes: Option<usize>,
    );
    fn refresh_memory_index_for_changed_paths<'a>(
        &'a self,
        paths: &'a [String],
    ) -> ToolFuture<'a, ()>;
}

/// Tool registry that manages tool execution.
#[derive(Clone)]
pub struct ToolRegistry;

#[derive(Default)]
struct ByteCountingWriter {
    bytes: usize,
}

impl Write for ByteCountingWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes = self.bytes.saturating_add(buffer.len());
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Clone)]
struct ToolSpecCatalog {
    entries: Vec<ToolCatalogEntry>,
}

#[derive(Clone)]
struct ToolCatalogEntry {
    #[cfg_attr(not(test), allow(dead_code))]
    family: ToolCapabilityFamily,
    spec: ToolSpec,
}

static TOOL_SPEC_CATALOG: LazyLock<Result<ToolSpecCatalog, String>> =
    LazyLock::new(|| build_tool_spec_catalog().map_err(|error| error.to_string()));

impl ToolRegistry {
    /// Create a new tool registry.
    pub fn new(_workspace_root: PathBuf) -> Self {
        Self
    }

    /// Get the stable tool specifications for this runtime.
    pub fn tool_specs(&self) -> Result<Vec<ToolSpec>> {
        let catalog = TOOL_SPEC_CATALOG
            .as_ref()
            .map_err(|error| anyhow!("failed to build tool schemas: {error}"))?;
        Ok(catalog
            .entries
            .iter()
            .filter(|entry| is_model_facing_tool(&entry.spec.name))
            .map(|entry| entry.spec.clone())
            .collect())
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn tool_specs_with_families(&self) -> Result<Vec<(ToolCapabilityFamily, ToolSpec)>> {
        let catalog = TOOL_SPEC_CATALOG
            .as_ref()
            .map_err(|error| anyhow!("failed to build tool schemas: {error}"))?;
        Ok(catalog
            .entries
            .iter()
            .filter(|entry| is_model_facing_tool(&entry.spec.name))
            .map(|entry| (entry.family, entry.spec.clone()))
            .collect())
    }

    pub fn tool_specs_with_families_for_apply_patch_surface(
        &self,
        surface: ApplyPatchSurface,
    ) -> Result<Vec<(ToolCapabilityFamily, ToolSpec)>> {
        Ok(
            tools::builtin_tool_definitions_for_apply_patch_surface(surface)?
                .into_iter()
                .filter(|definition| is_model_facing_tool(&definition.spec.name))
                .map(|definition| (definition.family, definition.spec))
                .collect(),
        )
    }

    pub fn family_for_tool(&self, tool_name: &str) -> Result<Option<ToolCapabilityFamily>> {
        let catalog = TOOL_SPEC_CATALOG
            .as_ref()
            .map_err(|error| anyhow!("failed to build tool schemas: {error}"))?;
        Ok(catalog
            .entries
            .iter()
            .find(|entry| entry.spec.name == tool_name)
            .map(|entry| entry.family))
    }

    /// Execute a tool call and return the result along with an execution record.
    pub async fn execute<S: Default + Send + Sync>(
        &self,
        runtime: &dyn ToolRuntime<State = S>,
        agent_id: &str,
        authority_class: &AuthorityClass,
        call: &ToolCall,
    ) -> Result<(ToolOutcome<S>, ToolExecutionRecord)> {
        self.execute_with_context(
            runtime,
            agent_id,
            authority_class,
            call,
            &ToolExecutionContext::default(),
        )
        .await
    }

    pub async fn execute_with_context<S: Default + Send + Sync>(
        &self,
        runtime: &dyn ToolRuntime<State = S>,
        agent_id: &str,
        authority_class: &AuthorityClass,
        call: &ToolCall,
        context: &ToolExecutionContext,
    ) -> Result<(ToolOutcome<S>, ToolExecutionRecord)> {
        let started_at = chrono::Utc::now();
        if call.name == "SpawnAgent" {
            return Err(ToolError::new("legacy_tool_removed", "SpawnAgent has been removed")
                .with_details(json!({
                    "tool_name": call.name,
                    "replacement_tools": ["CreateAgent", "InvokeAgent"],
                }))
                .with_recovery_hint(
                    "use CreateAgent for persistent named agents or InvokeAgent for child-agent work",
                )
                .into());
        }
        let required_family = match self.family_for_tool(&call.name)? {
            Some(required_family) => required_family,
            None => {
                return Err(ToolError::new(
                    "unknown_tool",
                    format!("{} is not an available tool", call.name),
                )
                .with_details(json!({
                    "tool_name": call.name,
                }))
                .with_recovery_hint(format!(
                    "use one of the advertised tool names from tool_specs() instead of {}",
                    call.name
                ))
                .into());
            }
        };
        if !runtime.allows_tool(required_family).await? {
            return Err(ToolError::new(
                "agent_capability_not_allowed",
                format!(
                    "{} is not allowed by the agent capability policy",
                    call.name
                ),
            )
            .with_details(json!({
                "tool_name": call.name,
                "required_family": required_family.label(),
            }))
            .with_recovery_hint(format!(
                "run {} from an agent whose capability policy allows the `{}` family",
                call.name,
                required_family.label()
            ))
            .into());
        }
        let tool_started = std::time::Instant::now();
        let tool = BuiltinTool::from_name(&call.name)
            .ok_or_else(|| anyhow!("unknown builtin tool {}", call.name))?;
        let (result, input_coercion) = capture_tool_input_coercion(runtime.invoke_builtin(
            tool,
            agent_id,
            authority_class,
            call,
            context,
        ))
        .await;
        let mut result = result?;
        let pending = context
            .external_command_attempt
            .as_ref()
            .and_then(|attempt| attempt.submitted.get());
        let execution_id = pending.map_or_else(holon_domain::ids::tool_execution_id, |record| {
            record.id.clone()
        });
        let started_at = pending.map_or(started_at, |record| record.created_at);
        if !result.is_error() {
            result.envelope.input_coercion = input_coercion;
        }
        attach_result_recovery(runtime, &mut result, &execution_id).await?;
        let mut byte_counter = ByteCountingWriter::default();
        let output_bytes = serde_json::to_writer(&mut byte_counter, &result.envelope)
            .ok()
            .map(|_| byte_counter.bytes);
        runtime.record_tool_execution(&call.name, tool_started.elapsed(), output_bytes);
        if !result.is_error() {
            if let Err(error) =
                maybe_refresh_memory_index_after_tool(runtime, call.name.as_str(), &result).await
            {
                eprintln!(
                    "failed to refresh memory index after {}: {error:#}",
                    call.name
                );
            }
        }

        let output_value = json!({
            "envelope": result.envelope,
            "is_error": result.is_error(),
            "should_sleep": result.should_sleep,
            "sleep_duration_ms": result.sleep_duration_ms,
            "error": result.tool_error().cloned(),
        });
        let finished_at = chrono::Utc::now();
        let duration_ms = finished_at
            .signed_duration_since(started_at)
            .num_milliseconds()
            .max(0) as u64;
        let completed_at = (!matches!(
            result.envelope.status,
            crate::tool::spec::ToolResultStatus::Deferred
        ))
        .then_some(finished_at);
        let record = ToolExecutionRecord {
            id: execution_id,
            agent_id: agent_id.to_string(),
            work_item_id: pending.and_then(|record| record.work_item_id.clone()),
            turn_index: pending.map_or(0, |record| record.turn_index),
            turn_id: pending.and_then(|record| record.turn_id.clone()),
            tool_name: call.name.clone(),
            created_at: started_at,
            completed_at,
            duration_ms,
            authority_class: authority_class.clone(),
            status: match result.envelope.status {
                crate::tool::spec::ToolResultStatus::Deferred => ToolExecutionStatus::Deferred,
                crate::tool::spec::ToolResultStatus::Success => ToolExecutionStatus::Success,
                crate::tool::spec::ToolResultStatus::Error => ToolExecutionStatus::Error,
            },
            input: call.input.clone(),
            output: output_value,
            summary: super::summary::tool_result_summary(&result.envelope),
            call_binding: pending.and_then(|record| record.call_binding.clone()),
            invocation_surface: tool_invocation_surface(call),
        };
        Ok((result, record))
    }
}

async fn maybe_refresh_memory_index_after_tool<S: Default + Send + Sync>(
    runtime: &dyn ToolRuntime<State = S>,
    tool_name: &str,
    result: &ToolOutcome<S>,
) -> Result<()> {
    if tool_name != "ApplyPatch" {
        return Ok(());
    }
    let Some(paths) = result
        .envelope
        .result
        .as_ref()
        .and_then(|value| value.get("changed_paths"))
        .and_then(Value::as_array)
    else {
        return Ok(());
    };
    let changed_paths = paths
        .iter()
        .filter_map(Value::as_str)
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    runtime
        .refresh_memory_index_for_changed_paths(&changed_paths)
        .await
}

fn tool_invocation_surface(call: &ToolCall) -> Option<String> {
    match call.name.as_str() {
        "ApplyPatch" => Some(
            if call.input.is_string() {
                "freeform_grammar"
            } else {
                "function_json"
            }
            .to_string(),
        ),
        _ => None,
    }
}

fn is_model_facing_tool(name: &str) -> bool {
    !super::names::HIDDEN_FROM_MODEL_TOOLS.contains(&name)
}

fn build_tool_spec_catalog() -> Result<ToolSpecCatalog> {
    Ok(ToolSpecCatalog {
        entries: tools::builtin_tool_definitions()?
            .into_iter()
            .map(|definition| catalog_entry(definition.family, definition.spec))
            .collect(),
    })
}

fn catalog_entry(family: ToolCapabilityFamily, spec: ToolSpec) -> ToolCatalogEntry {
    ToolCatalogEntry { family, spec }
}

/// Export once at execution, including deferred completion resolution, never
/// while projecting historical rounds.
pub async fn attach_result_recovery<S: Default + Send + Sync>(
    runtime: &dyn ToolRuntime<State = S>,
    result: &mut ToolOutcome<S>,
    execution_id: &str,
) -> Result<()> {
    if let Some(Value::Object(value)) = result.envelope.result.as_mut() {
        value
            .entry("output_ref")
            .or_insert_with(|| serde_json::json!(format!("tool_execution:{execution_id}:output")));
    }
    let canonical = serde_json::to_string_pretty(&result.envelope)?;
    if canonical.chars().count() > 4096 {
        let artifact = match runtime
            .persist_tool_text_artifact("tool-result", &canonical)
            .await
        {
            Ok(path) => serde_json::json!({
                "path": path, "complete": true, "encoding": "utf-8",
                "range_unit": "unicode_scalar", "chars": canonical.chars().count(),
            }),
            Err(error) => serde_json::json!({
                "complete": false,
                "reason": format!("canonical artifact could not be saved: {error}"),
            }),
        };
        if let Some(Value::Object(value)) = result.envelope.result.as_mut() {
            value.insert("recovery_artifact".into(), artifact);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
