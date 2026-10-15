//! Borrow only the original frozen core's possible canonical Input addresses.
use super::*;
use crate::runtime::resident_input_evidence::InputEvidenceFamily as Family;
use crate::runtime_db::input_evidence_snapshot::native_birth::NativeInputWriteProof;

type Visitor<'a> = dyn FnMut(Family, &str) -> Result<()> + 'a;
fn messages(values: &[MessageEnvelope], visit: &mut Visitor<'_>) -> Result<()> {
    for row in values {
        visit(Family::Message, &row.id)?;
    }
    Ok(())
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
fn protocol(value: &ExecutionProtocolTransition, visit: &mut Visitor<'_>) -> Result<()> {
    use crate::domain::execution_protocol::ExecutionProtocolCommand as C;
    // Bootstrap is the old aggregate/import baseline, not a native producer.
    for command in &value.commands {
        match command {
            C::RegisterWorkItem(c) => visit(Family::WorkExecution, &c.work_item_id)?,
            C::AdvanceWorkItemSourceRevision(c) => visit(Family::WorkExecution, &c.work_item_id)?,
            C::SetWorkItemReadiness(c) => visit(Family::WorkExecution, &c.work_item_id)?,
            C::SuspendWorkItemContinuation(c) => visit(Family::WorkExecution, &c.work_item_id)?,
            C::ResumeWorkItemContinuation(c) => visit(Family::WorkExecution, &c.work_item_id)?,
            C::ReconcileWorkItemContinuationYield(c) => {
                visit(Family::WorkExecution, &c.work_item_id)?
            }
            C::SetWorkItemWaiting(c) => visit(Family::WorkExecution, &c.work_item_id)?,
            C::CompleteWorkItem(c) => visit(Family::WorkExecution, &c.work_item_id)?,
            C::Admit(c) => visit(Family::Attempt, &c.attempt.attempt_id)?,
            C::Settle(c) => visit(Family::Outcome, &c.outcome.outcome_id)?,
            C::Interrupt(c) => visit(Family::Outcome, &c.outcome_id)?,
            C::RecoverInterruptedTaskResultClaim(c) => visit(Family::Outcome, &c.outcome_id)?,
            C::RecoverUnadvancedTaskResultClaim(c) => visit(Family::Outcome, &c.outcome_id)?,
        }
    }
    Ok(())
}
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
fn visit_original(prepared: &PreparedInputTransition, visit: &mut Visitor<'_>) -> Result<()> {
    match prepared.command.as_ref() {
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
        InputTransitionCommand::RuntimeAudit(_)
        | InputTransitionCommand::TaskResult(_)
        | InputTransitionCommand::QueueHeadNoProgress { .. } => {}
        InputTransitionCommand::ExternalTrigger(_)
        | InputTransitionCommand::TaskStatusMetadata(_)
        | InputTransitionCommand::TaskStopProgress(_)
        | InputTransitionCommand::TaskStopContinuation(_) => {
            anyhow::bail!("focused phase cannot enter ordinary Input birth core")
        }
    }
    Ok(())
}
pub(super) fn prepare_tx<'tx, 'conn, 'phase>(
    tx: &'tx Transaction<'conn>,
    prepared: &PreparedInputTransition,
    scope: &'phase InputEvidenceScope,
    ledger: &'phase crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    request: &crate::runtime_db::work_snapshot::WorkDomainLoadRequest,
    input_source: Option<&std::sync::Arc<crate::runtime::mutation_owner::OriginalInputQuerySource>>,
    work: Option<&crate::runtime_db::work_snapshot::WorkWriteProof>,
    tasks: Option<&super::task_writes::TaskTransaction<'_, '_, '_>>,
    scheduling: Option<&super::timer_writes::SchedulingTransaction<'_, '_>>,
    current: Option<&crate::runtime_db::current_read_snapshot::CurrentReadBusinessTransaction<'_, '_>>,
    memory: Option<&'phase mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
    first: Option<super::workspace_input::OriginalInputWriterMaterial<'phase>>,
) -> Result<Option<NativeInputWriteProof<'tx, 'conn>>> {
    if input_source.is_some() {
        let policy = crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
            windows: Vec::new(), mandatory_roots: BTreeSet::new(),
            interrupted_sources: BTreeSet::new(), selected_completion_brief_ids: BTreeSet::new(),
            byte_limit: request.byte_limit, record_limit: request.record_limit,
        };
        let (mut material,current_prior,first_phase,memory_cumulative)=match first {
            Some(first)=>{
                match memory {
                    Some(memory)=>(first.input.resume_after_original_memory(memory)?,first.current_prior,true,true),
                    None=>{
                        (first.input,first.current_prior,true,false)
                    },
                }
            }
            None=>(crate::runtime_db::input_evidence_snapshot::read_material::InputReadMaterial::new_with_memory(
                scope,&policy,ledger,(0,0),memory)?,(0,0),false,false),
        };
        // Original proof counters include their real held bodies and guards;
        // Work/Task/Scheduling have already debited this Memory allowance.
        if let Some(work) = work {
            let actual=work.input_writer_material(scope,ledger)?;
            if !memory_cumulative && !first_phase {material.observe_original_writer_prior(actual)?;}
        }
        if let Some(tasks) = tasks {
            let actual=tasks.input_writer_material(scope,ledger)?;
            if !memory_cumulative && !first_phase {material.observe_original_writer_prior(actual)?;}
        }
        if let Some(scheduling) = scheduling {
            let actual=scheduling.input_writer_material(tx,scope,ledger)?;
            if !memory_cumulative && !first_phase {material.observe_original_writer_prior(actual)?;}
        }
        // Current has not borrowed this Memory phase before. Its sealed actual
        // observer performs that single debit, with no duplicate guard.
        if let Some(current) = current {
            let observed=current.input_writer_material(tx)?;
            material.observe_current_prior(&observed.after_admitted(current_prior)?)?;
        }
        let proof=NativeInputWriteProof::prepare_using_input_material_tx(
            tx, scope, |visit| visit_original(prepared, visit), &mut material, input_source,
        )?;
        anyhow::ensure!(!first_phase || proof.is_some(),"first Input material requires installed original Native source");
        return proof.map(|proof|proof.retain_input_phase(material)).transpose();
    }
    anyhow::ensure!(first.is_none(),"Workspace first Input material requires its original selectors");
    NativeInputWriteProof::prepare_with_query_source_tx(
        tx,
        scope,
        |visit| visit_original(prepared, visit),
        ledger,
        (request.byte_limit, request.record_limit),
        input_source,
    )
}

/// Before original canonical serialization and Effects clones, reserve only
/// Tool DTOs that the frozen command already owns. No SQL discovery occurs.
pub(super) fn reserve_original_tools(
    prepared: &PreparedInputTransition,
    writer: &dyn crate::runtime_db::input_evidence_snapshot::tool_writer::OriginalToolInputWriter,
) -> Result<()> {
    match prepared.command.as_ref() {
        InputTransitionCommand::Terminal(command) => {
            for tool in &command.terminal_tool_executions { writer.reserve_original_tool(tool)?; }
        }
        InputTransitionCommand::Focus { completion_tool_execution: Some(tool), .. } => writer.reserve_original_tool(tool)?,
        InputTransitionCommand::Queue(queue) => {
            for tool in &queue.terminal_tool_executions { writer.reserve_original_tool(tool)?; }
            if let Some(completion) = &queue.completion { writer.reserve_original_tool(&completion.tool_execution)?; }
        }
        _ => {},
    }
    Ok(())
}
