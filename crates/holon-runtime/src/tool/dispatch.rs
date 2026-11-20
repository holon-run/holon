//! Runtime capabilities used by the built-in tool router.

use anyhow::Result;
use holon_tools::{
    builtins::BuiltinTool,
    dispatch::{ToolFuture, ToolRuntime},
};
use std::path::PathBuf;

use crate::{
    runtime::RuntimeHandle,
    types::{ApplyPatchSurface, AuthorityClass, ToolCapabilityFamily, ToolExecutionRecord},
};

use super::{
    spec::{RuntimeToolState, ToolCall, ToolExecutionContext, ToolResult, ToolSpec},
    tools,
};

/// Tool access through the application's capability and execution boundaries.
#[derive(Clone)]
pub struct ToolRegistry;

impl ToolRegistry {
    pub fn new(_workspace_root: PathBuf) -> Self {
        Self
    }

    pub fn tool_specs(&self) -> Result<Vec<ToolSpec>> {
        holon_tools::dispatch::ToolRegistry.tool_specs()
    }

    #[cfg(test)]
    pub(crate) fn tool_specs_with_families(&self) -> Result<Vec<(ToolCapabilityFamily, ToolSpec)>> {
        holon_tools::dispatch::ToolRegistry.tool_specs_with_families()
    }

    pub(crate) fn tool_specs_with_families_for_apply_patch_surface(
        &self,
        surface: ApplyPatchSurface,
    ) -> Result<Vec<(ToolCapabilityFamily, ToolSpec)>> {
        holon_tools::dispatch::ToolRegistry
            .tool_specs_with_families_for_apply_patch_surface(surface)
    }

    #[cfg(test)]
    pub(crate) fn family_for_tool(&self, name: &str) -> Result<Option<ToolCapabilityFamily>> {
        holon_tools::dispatch::ToolRegistry.family_for_tool(name)
    }

    pub async fn execute(
        &self,
        runtime: &RuntimeHandle,
        agent_id: &str,
        authority: &AuthorityClass,
        call: &ToolCall,
    ) -> Result<(ToolResult, ToolExecutionRecord)> {
        holon_tools::dispatch::ToolRegistry
            .execute(&RuntimeToolHost(runtime), agent_id, authority, call)
            .await
    }

    pub(crate) async fn execute_with_context(
        &self,
        runtime: &RuntimeHandle,
        agent_id: &str,
        authority: &AuthorityClass,
        call: &ToolCall,
        context: &ToolExecutionContext,
    ) -> Result<(ToolResult, ToolExecutionRecord)> {
        holon_tools::dispatch::ToolRegistry
            .execute_with_context(
                &RuntimeToolHost(runtime),
                agent_id,
                authority,
                call,
                context,
            )
            .await
    }
}

pub(crate) async fn attach_result_recovery(
    runtime: &RuntimeHandle,
    result: &mut ToolResult,
    execution_id: &str,
) -> Result<()> {
    holon_tools::dispatch::attach_result_recovery(&RuntimeToolHost(runtime), result, execution_id)
        .await
}

struct RuntimeToolHost<'a>(&'a RuntimeHandle);

impl ToolRuntime for RuntimeToolHost<'_> {
    type State = RuntimeToolState;

    fn allows_tool(&self, family: ToolCapabilityFamily) -> ToolFuture<'_, bool> {
        Box::pin(async move {
            Ok(self
                .0
                .agent_capability_policy()
                .await?
                .is_none_or(|policy| policy.allows(family)))
        })
    }

    fn invoke_builtin<'a>(
        &'a self,
        tool: BuiltinTool,
        agent_id: &'a str,
        authority: &'a AuthorityClass,
        call: &'a ToolCall,
        context: &'a ToolExecutionContext,
    ) -> ToolFuture<'a, ToolResult> {
        tools::execute_builtin_tool_route(self.0, tool, agent_id, authority, call, context)
    }

    fn persist_tool_text_artifact<'a>(
        &'a self,
        kind: &'a str,
        text: &'a str,
    ) -> ToolFuture<'a, String> {
        Box::pin(self.0.persist_tool_text_artifact(kind, text))
    }

    fn record_tool_execution(
        &self,
        name: &str,
        elapsed: std::time::Duration,
        output_bytes: Option<usize>,
    ) {
        crate::diagnostics::record_tool_execution(name, elapsed, output_bytes);
    }

    fn refresh_memory_index_for_changed_paths<'a>(
        &'a self,
        paths: &'a [String],
    ) -> ToolFuture<'a, ()> {
        Box::pin(self.0.refresh_memory_index_for_changed_paths(paths))
    }
}
