use super::serialize_success;
use crate::runtime::RuntimeHandle;
use crate::runtime_db::destructive_operations::command_digest;
use crate::runtime_db::DestructiveOperationPhase;
use crate::runtime_db::DestructiveOperationRecord;
use crate::tool::helpers::parse_tool_args;
use crate::tool::spec::ToolExecutionContext;
use crate::tool::ToolResult;
use crate::types::AuthorityClass;
use crate::types::CommandTaskSpec;
use crate::types::ExecCommandOutcome;
use anyhow::anyhow;
use anyhow::ensure;
use anyhow::Result;
use serde_json::json;
use serde_json::Value;

pub(crate) use holon_tools::builtins::schedule_destructive_operation::*;

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    authority_class: &AuthorityClass,
    input: &Value,
    context: &ToolExecutionContext,
) -> Result<ToolResult> {
    let args: ScheduleDestructiveOperationArgs = parse_tool_args(NAME, input)?;
    ensure!(
        !args.operation_id.trim().is_empty(),
        "operation_id must not be empty"
    );
    ensure!(!args.command.trim().is_empty(), "command must not be empty");
    ensure!(
        !args.verification_target.trim().is_empty(),
        "verification_target must not be empty"
    );
    let turn_id = context
        .turn_id
        .as_deref()
        .ok_or_else(|| anyhow!("destructive lifecycle operation requires a durable turn"))?;
    ensure!(
        context.effective_work_item_id.is_some(),
        "destructive lifecycle operation requires an owner WorkItem"
    );

    ensure!(
        context.external_command_attempt.is_some(),
        "destructive lifecycle operation requires a bound tool call"
    );

    let record = DestructiveOperationRecord {
        operation_id: args.operation_id.clone(),
        owner_turn_id: turn_id.to_string(),
        owner_work_item_id: context.effective_work_item_id.clone(),
        command_digest: command_digest(&args.command),
        phase: DestructiveOperationPhase::Planned,
        verification_target: args.verification_target.clone(),
        recovery_policy: "verify_only".into(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    let planned = runtime.plan_destructive_operation(record).await?;
    let should_dispatch = planned.should_dispatch;
    let operation = planned.record;
    if !should_dispatch {
        return serialize_success(
            NAME,
            &json!({
                "operation_id": operation.operation_id,
                "phase": format!("{:?}", operation.phase).to_lowercase(),
                "already_scheduled": true,
                "summary_text": "destructive operation already scheduled; verify the target instead of dispatching again",
            }),
        );
    }

    let unit = format!(
        "holon-destructive-{}",
        operation
            .operation_id
            .chars()
            .map(|ch| if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '-'
            })
            .collect::<String>()
    );
    let command = format!(
        "systemd-run --user --unit={} --collect --on-active=2s /bin/sh -lc {}",
        unit,
        shell_quote(&args.command)
    );
    let result = runtime
        .managed_tasks()
        .execute_exec_command_once(
            CommandTaskSpec {
                cmd: command,
                workdir: None,
                shell: Some("/bin/sh".into()),
                login: false,
                tty: false,
                yield_time_ms: 10_000,
                max_output_tokens: Some(2_000),
                accepts_input: false,
                terminal_reentry: false,
            },
            authority_class,
            context,
            NAME,
            input,
        )
        .await?;
    let launched = matches!(
        result.outcome,
        ExecCommandOutcome::Completed {
            exit_status: Some(0),
            ..
        }
    );
    if !launched {
        ensure!(
            runtime.runtime_db().destructive_operations().transition(
            &operation.operation_id,
            DestructiveOperationPhase::Scheduled,
            DestructiveOperationPhase::Planned,
            )?,
            "destructive operation {} dispatch failed but its scheduled marker could not be rolled back",
            operation.operation_id
        );
    }
    serialize_success(
        NAME,
        &json!({
            "operation_id": operation.operation_id,
            "phase": if launched { "scheduled" } else { "dispatch_failed" },
            "already_scheduled": false,
            "external_dispatch": result,
            "summary_text": if launched {
                "destructive operation scheduled outside the daemon cgroup"
            } else {
                "destructive operation dispatch failed; marker returned to planned"
            },
        }),
    )
}
