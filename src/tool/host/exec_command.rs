use super::serialize_success;
use crate::runtime::RuntimeHandle;
use crate::tool::helpers::parse_tool_args;
use crate::tool::spec::ToolExecutionContext;
use crate::tool::ToolResult;
use crate::types::AuthorityClass;
use crate::types::CommandTaskSpec;
use crate::types::ExecCommandResult;
use anyhow::Result;
use serde_json::Value;

pub(crate) use holon_tools::builtins::exec_command::*;

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    authority_class: &AuthorityClass,
    input: &Value,
    context: &ToolExecutionContext,
) -> Result<ToolResult> {
    let args: ExecCommandArgs = parse_tool_args(NAME, input)?;
    let tty = args.tty.unwrap_or(false);
    let duplicate_policy = args.duplicate_policy;
    let spec = CommandTaskSpec {
        cmd: args.cmd,
        workdir: args.workdir,
        shell: args.shell,
        login: args.login.unwrap_or(true),
        tty,
        yield_time_ms: args.yield_time_ms.unwrap_or(10_000),
        max_output_tokens: args.max_output_tokens,
        accepts_input: args.accepts_input.unwrap_or(tty),
        terminal_reentry: false,
    };
    let result: ExecCommandResult = runtime
        .managed_tasks()
        .execute_exec_command(
            spec,
            duplicate_policy,
            authority_class,
            context.trace_context.as_ref(),
            context
                .turn_id
                .as_deref()
                .zip(context.tool_call_id.as_deref()),
            context
                .external_command_attempt
                .as_deref()
                .map(|attempt| (attempt, input)),
        )
        .await?;
    serialize_success(NAME, &result)
}
