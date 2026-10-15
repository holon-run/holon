//! Exact original command footprint, before its first Queue/Audit SQL write.
use super::*;
use crate::runtime::{
    current_read_facts::{CurrentReadFamily, CurrentReadKey},
    input_evidence_budget::{InputEvidenceBudgetLedger, InputMemoryCategory, OwnedBytesEstimate},
};
use crate::runtime_db::current_read_snapshot::CurrentReadBusinessTransaction;
use std::sync::Arc;

#[derive(Clone, Copy)]
enum OriginalKey<'a> {
    Exact(CurrentReadFamily, &'a str),
    WaitAudit(&'a str, WaitTriggerAuditKind),
}
impl OriginalKey<'_> {
    fn material_bytes(self) -> Result<usize> {
        match self {
            Self::Exact(_, id) => Ok(id.len()),
            Self::WaitAudit(id, kind) => id
                .len()
                .checked_add(kind.prefix().len())
                .context("CurrentRead stable audit address overflow"),
        }
    }
    fn key(self) -> CurrentReadKey {
        match self {
            Self::Exact(family, id) => CurrentReadKey {
                family,
                id: id.to_owned(),
            },
            Self::WaitAudit(id, kind) => CurrentReadKey::event(wait_trigger_audit_id(id, kind)),
        }
    }
}
fn events<'a>(
    values: &'a [AuditEvent],
    visit: &mut impl FnMut(OriginalKey<'a>) -> Result<()>,
) -> Result<()> {
    for event in values {
        visit(OriginalKey::Exact(CurrentReadFamily::Event, &event.id))?;
    }
    Ok(())
}
fn admitted<'a>(id: &'a str, visit: &mut impl FnMut(OriginalKey<'a>) -> Result<()>) -> Result<()> {
    visit(OriginalKey::Exact(CurrentReadFamily::Queue, id))?;
    // Original selector chooses at most one wait; the branches are exclusive.
    // Unused exact addresses receive no alias or membership.
    visit(OriginalKey::WaitAudit(id, WaitTriggerAuditKind::Triggered))?;
    visit(OriginalKey::WaitAudit(id, WaitTriggerAuditKind::Stale))
}
fn wait<'a>(
    command: &'a WaitTransitionCommand,
    visit: &mut impl FnMut(OriginalKey<'a>) -> Result<()>,
) -> Result<()> {
    events(&command.audit_events, visit)?;
    if let Some(admission) = &command.task_result_admission {
        visit(OriginalKey::Exact(
            CurrentReadFamily::Queue,
            &admission.record.message_id,
        ))?;
    }
    Ok(())
}
fn visit_original<'a>(
    prepared: &'a PreparedInputTransition,
    visit: &mut impl FnMut(OriginalKey<'a>) -> Result<()>,
) -> Result<()> {
    match prepared.command.as_ref() {
        InputTransitionCommand::RuntimeAudit(command) => {
            for event in command.events() {
                visit(OriginalKey::Exact(CurrentReadFamily::Event, &event.id))?;
            }
        }
        InputTransitionCommand::Posture { command, .. } => events(&command.audit_events, visit)?,
        InputTransitionCommand::Terminal(command) => events(&command.audit_events, visit)?,
        InputTransitionCommand::Work { command, .. } => events(&command.audit_events, visit)?,
        InputTransitionCommand::Focus { command, .. } => events(&command.audit_events, visit)?,
        InputTransitionCommand::Wait { command, .. } => wait(command, visit)?,
        InputTransitionCommand::Task(task) => {
            events(&task.command.audit_events, visit)?;
            if let Some(queue) = &task.command.queue_entry {
                admitted(&queue.message_id, visit)?;
            }
        }
        InputTransitionCommand::TaskResult(_) => {}
        InputTransitionCommand::Queue(queue) => {
            let record = queue_mutation_record(&queue.command.mutation);
            if queue.command.operation == QueueOperation::Admit
                && !queue.command.message_evidence.is_empty()
            {
                admitted(&record.message_id, visit)?;
            } else {
                visit(OriginalKey::Exact(
                    CurrentReadFamily::Queue,
                    &record.message_id,
                ))?;
            }
            events(&queue.command.audit_events, visit)?;
            if let Some(event) = queue.original_terminal_state_audit.as_ref() {
                visit(OriginalKey::Exact(CurrentReadFamily::Event, &event.id))?;
            }
            if let Some(command) = &queue.wait_registration {
                wait(command, visit)?;
            }
        }
        InputTransitionCommand::QueueHeadNoProgress {
            command,
            audit_event_id,
            ..
        } => {
            visit(OriginalKey::Exact(
                CurrentReadFamily::Queue,
                &command.expected.message_id,
            ))?;
            visit(OriginalKey::Exact(CurrentReadFamily::Event, audit_event_id))?;
        }
        InputTransitionCommand::Timer(timer) => match timer {
            PreparedTimerTransition::Fire {
                command,
                pending: None,
                ..
            } => admitted(&command.queue_entry.message_id, visit)?,
            PreparedTimerTransition::Cancel {
                pending: Some(wake),
                ..
            } => visit(OriginalKey::Exact(
                CurrentReadFamily::Queue,
                &wake.message_id,
            ))?,
            _ => {}
        },
        InputTransitionCommand::ExternalTrigger(_)
        | InputTransitionCommand::TaskStatusMetadata(_)
        | InputTransitionCommand::TaskStopProgress(_)
        | InputTransitionCommand::TaskStopContinuation(_) => {
            anyhow::bail!("focused phase cannot enter ordinary CurrentRead core")
        }
    }
    Ok(())
}
pub(super) fn accepts_state_source(prepared: &PreparedInputTransition) -> Result<bool> {
    // Physical-only progress consumes this witness in its own closed kernel.
    if let InputTransitionCommand::TaskStopProgress(command) = prepared.command.as_ref() {
        return Ok(!command.has_current_read_events());
    }
    if matches!(
        prepared.command.as_ref(),
        InputTransitionCommand::RuntimeAudit(_)
            | InputTransitionCommand::ExternalTrigger(_)
            | InputTransitionCommand::TaskStatusMetadata(_)
            | InputTransitionCommand::TaskStopProgress(_)
            | InputTransitionCommand::TaskStopContinuation(_)
    ) {
        // Their closed kernels run before ordinary CurrentRead dispatch. They
        // retain no unused State witness and must supply their own actual ACK.
        return Ok(false);
    }
    // Finish may consume an older Deferred audit identity, validated only in TX.
    if matches!(
        prepared.memory.as_ref(),
        Some(crate::runtime_db::dispatch_memory::DispatchMemoryIntent::Finish { .. })
    ) {
        return Ok(false);
    }
    let mut empty = true;
    visit_original(prepared, &mut |_| {
        empty = false;
        Ok(())
    })?;
    Ok(empty)
}
pub(super) fn prepare_tx<'tx, 'conn>(
    tx: &'tx Transaction<'conn>,
    prepared: &PreparedInputTransition,
    scope: &InputEvidenceScope,
    state: &AgentState,
    ledger: &InputEvidenceBudgetLedger,
    finish_memory: Option<
        &crate::runtime_db::dispatch_memory::ValidatedDispatchMemoryFinish<'_, '_>,
    >,
) -> Result<CurrentReadBusinessTransaction<'tx, 'conn>> {
    prepare_with_material_limits_tx(tx,prepared,scope,state,ledger,finish_memory,None)
}
pub(super) fn prepare_with_material_limits_tx<'tx, 'conn>(
    tx: &'tx Transaction<'conn>,
    prepared: &PreparedInputTransition,
    scope: &InputEvidenceScope,
    state: &AgentState,
    ledger: &InputEvidenceBudgetLedger,
    finish_memory: Option<
        &crate::runtime_db::dispatch_memory::ValidatedDispatchMemoryFinish<'_, '_>,
    >,
    material_limits: Option<(usize,usize)>,
) -> Result<CurrentReadBusinessTransaction<'tx, 'conn>> {
    let memory_audit = finish_memory
        .map(|proof| proof.original_audit_id_tx(tx))
        .transpose()?;
    let (mut request, role_prior) = prepared.current_event_request(ledger)?;
    if let Some(limits)=material_limits {
        request.byte_limit=request.byte_limit.min(limits.0);
        request.record_limit=request.record_limit.min(limits.1);
        request.check_material(0,0)?;
    }
    let mut bytes = 0usize;
    let mut records = 0usize;
    let mut count_key = |key: OriginalKey<'_>| {
        bytes = bytes
            .checked_add(key.material_bytes()?)
            .and_then(|n| n.checked_add(256))
            .context("CurrentRead footprint bytes overflow")?;
        records = records
            .checked_add(4)
            .context("CurrentRead footprint count overflow")?;
        Ok(())
    };
    visit_original(prepared, &mut count_key)?;
    if let Some(id) = memory_audit {
        count_key(OriginalKey::Exact(CurrentReadFamily::Event, id))?;
    }
    bytes = bytes
        .checked_add(scope.agent_id.len())
        .and_then(|n| n.checked_add(512))
        .and_then(|n| n.checked_mul(8))
        .context("CurrentRead footprint bytes overflow")?;
    records = records
        .checked_add(32)
        .and_then(|n| n.checked_mul(8))
        .context("CurrentRead footprint records overflow")?;
    let prior = (
        bytes
            .checked_add(role_prior.0)
            .context("Event role footprint bytes overflow")?,
        records
            .checked_add(role_prior.1)
            .context("Event role footprint nodes overflow")?,
    );
    request.check_material(prior.0, prior.1)?;
    let guard = ledger.register_owned_bytes(
        scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: 0,
            metadata_estimate_bytes: u64::try_from(bytes)?,
        },
    )?;
    // The borrowed pass precedes all String/BTreeSet/implicit audit ID copies.
    let mut keys = BTreeSet::new();
    visit_original(prepared, &mut |key| {
        keys.insert(key.key());
        Ok(())
    })?;
    if let Some(id) = memory_audit {
        keys.insert(CurrentReadKey::event(id));
    }
    if keys.is_empty() {
        if let Some(source) = prepared.current_read_source.as_ref() {
            return CurrentReadBusinessTransaction::prepare_state_source_tx(
                tx,
                scope,
                state,
                &request,
                ledger,
                prior,
                vec![guard],
                source.clone(),
            );
        }
    }
    CurrentReadBusinessTransaction::prepare_tx(
        tx,
        scope,
        state,
        &keys,
        &request,
        ledger,
        prior,
        vec![guard],
    )
}
pub(super) fn finish(
    current: CurrentReadBusinessTransaction<'_, '_>,
    value: &mut TransitionCommit,
    before: &AgentState,
) -> Result<()> {
    let final_state = value
        .effects
        .agent_state
        .as_ref()
        .map(|mutation| mutation.record.as_ref())
        .unwrap_or(before);
    value.effects.current_read_facts = Some(Arc::new(current.finish(final_state)?));
    Ok(())
}

#[cfg(test)]
#[path = "prepared_current_reads_tests.rs"]
mod tests;
