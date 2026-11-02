//! Borrow only original command addresses, before any canonical business SQL.
use super::*;
use crate::runtime_db::current_read_writes::CurrentFamily;

type Visitor<'a> = dyn FnMut(CurrentFamily, &str) -> Result<()> + 'a;

fn events(values: &[AuditEvent], visit: &mut Visitor<'_>) -> Result<()> {
    for event in values {
        visit(CurrentFamily::Event, &event.id)?;
    }
    Ok(())
}
fn wait(command: &WaitTransitionCommand, visit: &mut Visitor<'_>) -> Result<()> {
    events(&command.audit_events, visit)?;
    if let Some(admission) = &command.task_result_admission {
        visit(CurrentFamily::Queue, &admission.message.id)?;
    }
    Ok(())
}
pub(super) fn visit_original(
    prepared: &PreparedInputTransition,
    memory_finish: Option<
        &crate::runtime_db::dispatch_memory::ValidatedDispatchMemoryFinish<'_, '_>,
    >,
    visit: &mut Visitor<'_>,
) -> Result<()> {
    match prepared.command.as_ref() {
        InputTransitionCommand::Audit { events: values, .. } => events(values, visit)?,
        InputTransitionCommand::ContextBudgetStopRecovery { audit_events, .. } => {
            events(audit_events, visit)?;
        }
        InputTransitionCommand::Posture { command, .. } => events(&command.audit_events, visit)?,
        InputTransitionCommand::Terminal(command) => events(&command.audit_events, visit)?,
        InputTransitionCommand::Work { command, .. } => events(&command.audit_events, visit)?,
        InputTransitionCommand::Focus { command, .. } => events(&command.audit_events, visit)?,
        InputTransitionCommand::Wait { command, .. } => wait(command, visit)?,
        InputTransitionCommand::Task(task) => {
            events(&task.command.audit_events, visit)?;
            if task.original_child_terminal.is_some() {
                child_terminal::visit_source_audits(prepared, visit)?;
            }
            if let Some(queue) = &task.command.queue_entry {
                visit(CurrentFamily::Queue, &queue.message_id)?;
            }
            // A Task can publish its result before Queue admission.
            for message in &task.command.message_evidence {
                visit(CurrentFamily::Queue, &message.id)?;
            }
        }
        InputTransitionCommand::Queue(queue) => {
            events(&queue.command.audit_events, visit)?;
            let record = match &queue.command.mutation {
                QueueMutation::Consume(record) | QueueMutation::Upsert(record) => record,
                QueueMutation::CompareAndSet { record, .. }
                | QueueMutation::SettleWithProviderRecovery { record, .. }
                | QueueMutation::ParkProviderDispatch { record, .. }
                | QueueMutation::ConsumeSettledTaskResultRecovery { record, .. } => record,
            };
            visit(CurrentFamily::Queue, &record.message_id)?;
            if let QueueMutation::SettleWithProviderRecovery { recovery, .. } =
                &queue.command.mutation
            {
                visit(CurrentFamily::Queue, &recovery.id)?;
            }
            if let Some(wait_command) = &queue.wait_registration {
                wait(wait_command, visit)?;
            }
        }
        InputTransitionCommand::QueueHeadNoProgress {
            command,
            audit_event_id,
            ..
        } => {
            visit(CurrentFamily::Event, audit_event_id)?;
            visit(CurrentFamily::Queue, &command.expected.message_id)?;
        }
        InputTransitionCommand::Timer(timer) => {
            if let Some(id) = timer.queue_write_address() {
                visit(CurrentFamily::Queue, id)?;
            }
        }
        InputTransitionCommand::TaskResult(_) => {}
        InputTransitionCommand::LiveInputAttachment(command) => {
            for queue in &command.queues {
                visit(CurrentFamily::Queue, &queue.message_id)?;
            }
            visit(CurrentFamily::Event, &command.audit_event_id)?;
        }
        InputTransitionCommand::LiveInputRecovery(command) => {
            visit(CurrentFamily::Event, &command.audit_event_id)?;
        }
        InputTransitionCommand::ProviderResultHandoff(command) => {
            visit(CurrentFamily::Event, &command.audit_event_id)?;
        }
        InputTransitionCommand::ExternalTrigger(_)
        | InputTransitionCommand::TaskStatusMetadata(_)
        | InputTransitionCommand::TaskStopProgress(_)
        | InputTransitionCommand::TaskStopContinuation(_) => {
            anyhow::bail!("focused phase cannot enter ordinary Current writer")
        }
    }
    match &prepared.memory {
        Some(crate::runtime_db::dispatch_memory::DispatchMemoryIntent::Deferred {
            frozen, ..
        }) => {
            visit(CurrentFamily::Event, &frozen.audit_event_id)?;
        }
        Some(crate::runtime_db::dispatch_memory::DispatchMemoryIntent::Finish { .. }) => {
            // A prior obligation owns the frozen address, not this Finish intent.
            let finish = memory_finish.context("Current omitted validated memory finish")?;
            visit(CurrentFamily::Event, finish.audit_event_id())?;
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn prepare_generated(
    prepared: &PreparedInputTransition,
    current: &mut crate::runtime_db::current_read_writes::OriginalCurrentWrites<'_, '_>,
    material: &mut crate::runtime_db::input_evidence_snapshot::read_material::InputReadMaterial<'_>,
) -> Result<()> {
    match prepared.command.as_ref() {
        InputTransitionCommand::Task(task) if task.command.queue_entry.is_some() => {
            for message in &task.command.message_evidence {
                current.prepare_trigger_events(
                    &message.id,
                    task.command
                        .wait_conditions
                        .iter()
                        .map(|wait| wait.id.as_str()),
                    material,
                )?;
            }
        }
        InputTransitionCommand::Queue(queue)
            if queue.command.operation == QueueOperation::Admit =>
        {
            for message in &queue.command.message_evidence {
                current.prepare_trigger_events(
                    &message.id,
                    queue
                        .wait_registration
                        .iter()
                        .flat_map(|command| &command.wait_conditions)
                        .map(|wait| wait.id.as_str()),
                    material,
                )?;
            }
        }
        InputTransitionCommand::Timer(timer) => {
            if let Some((message, _)) = timer.new_wake() {
                current.prepare_trigger_events(&message.id, std::iter::empty(), material)?;
            }
            if let PreparedTimerTransition::Cancel { command, .. } = timer {
                current.prepare_timer_cancellation_events(command, material)?;
            }
        }
        _ => {}
    }
    Ok(())
}
