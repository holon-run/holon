//! Borrow the frozen command's possible addresses, never discover body authority.
use super::*;
use crate::runtime::resident_input_evidence::InputEvidenceFamily as Family;

type Visitor<'a> = dyn FnMut(Family, &str) -> Result<()> + 'a;

fn work(values: &[WorkItemMutation], visit: &mut Visitor<'_>) -> Result<()> {
    for row in values {
        visit(Family::WorkExecution, &row.record().id)?;
    }
    Ok(())
}
fn wait(value: &WaitTransitionCommand, visit: &mut Visitor<'_>) -> Result<()> {
    work(&value.work_items, visit)?;
    if let Some(admission) = &value.task_result_admission {
        visit(Family::Message, &admission.message.id)?;
    }
    Ok(())
}
fn protocol(value: &ExecutionProtocolTransition, visit: &mut Visitor<'_>) -> Result<()> {
    crate::runtime_db::input_evidence_snapshot::native_birth::visit_execution_commands(
        &value.commands,
        visit,
    )
}
fn briefs(values: &[BriefRecord], visit: &mut Visitor<'_>) -> Result<()> {
    for row in values {
        visit(Family::Brief, &row.id)?;
    }
    Ok(())
}
fn tools(values: &[ToolExecutionRecord], visit: &mut Visitor<'_>) -> Result<()> {
    for row in values {
        visit(Family::ToolExecution, &row.id)?;
    }
    Ok(())
}
fn messages(values: &[MessageEnvelope], visit: &mut Visitor<'_>) -> Result<()> {
    for row in values {
        visit(Family::Message, &row.id)?;
    }
    Ok(())
}

pub(super) fn visit_original(
    prepared: &PreparedInputTransition,
    visit: &mut Visitor<'_>,
) -> Result<()> {
    match prepared.command.as_ref() {
        InputTransitionCommand::Audit { .. } => {}
        InputTransitionCommand::ContextBudgetStopRecovery {
            protocol: value, ..
        } => {
            protocol(value, visit)?;
        }
        InputTransitionCommand::Posture { started_turn, .. } => {
            if let Some(turn) = started_turn {
                visit(Family::Turn, &turn.turn_id)?;
            }
        }
        InputTransitionCommand::Terminal(command) => {
            visit(Family::Turn, &command.turn_record.turn_id)?;
            tools(&command.terminal_tool_executions, visit)?;
        }
        InputTransitionCommand::Work {
            command,
            execution_protocol,
            ..
        } => {
            visit(Family::WorkExecution, &command.mutation.record().id)?;
            briefs(&command.brief_evidence, visit)?;
            protocol(execution_protocol, visit)?;
        }
        InputTransitionCommand::Focus {
            command,
            execution_protocol,
            completion_tool_execution,
            ..
        } => {
            work(&command.work_items, visit)?;
            briefs(&command.brief_evidence, visit)?;
            protocol(execution_protocol, visit)?;
            if let Some(tool) = completion_tool_execution {
                visit(Family::ToolExecution, &tool.id)?;
            }
        }
        InputTransitionCommand::Wait {
            command,
            execution_protocol,
            ..
        } => {
            wait(command, visit)?;
            protocol(execution_protocol, visit)?;
        }
        InputTransitionCommand::Task(task) => {
            messages(&task.command.message_evidence, visit)?;
            work(&task.command.work_items, visit)?;
            protocol(&task.execution_protocol, visit)?;
        }
        InputTransitionCommand::Queue(queue) => {
            messages(&queue.command.message_evidence, visit)?;
            briefs(&queue.command.brief_evidence, visit)?;
            for row in &queue.command.transcript_entries {
                visit(Family::Transcript, &row.id)?;
            }
            if let Some(turn) = &queue.command.turn_record {
                visit(Family::Turn, &turn.turn_id)?;
            }
            protocol(&queue.execution_protocol, visit)?;
            tools(&queue.terminal_tool_executions, visit)?;
            if let Some(completion) = &queue.completion {
                work(&completion.work_items, visit)?;
                visit(Family::ToolExecution, &completion.tool_execution.id)?;
            }
            if let Some(registration) = &queue.wait_registration {
                wait(registration, visit)?;
            }
        }
        InputTransitionCommand::Timer(timer) => {
            if let Some((message, _)) = timer.new_wake() {
                visit(Family::Message, &message.id)?;
            }
        }
        InputTransitionCommand::TaskResult(_)
        | InputTransitionCommand::QueueHeadNoProgress { .. } => {}
        InputTransitionCommand::LiveInputAttachment(command) => {
            visit(Family::Turn, &command.expected_turn.turn_id)?;
        }
        InputTransitionCommand::LiveInputRecovery(command) => {
            visit(Family::Turn, &command.expected_turn.turn_id)?;
        }
        InputTransitionCommand::ProviderResultHandoff(command) => {
            visit(Family::Turn, &command.expected_turn.turn_id)?;
        }
        InputTransitionCommand::ExternalTrigger(_)
        | InputTransitionCommand::TaskStatusMetadata(_)
        | InputTransitionCommand::TaskStopProgress(_)
        | InputTransitionCommand::TaskStopContinuation(_) => {
            anyhow::bail!("focused phase cannot enter ordinary Input birth core");
        }
    }
    Ok(())
}
