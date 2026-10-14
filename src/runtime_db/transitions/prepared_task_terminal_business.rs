//! Shared original Task terminal business projection; this helper grants no producer authority.
use super::*;
use crate::runtime::input_evidence_budget::{BudgetViewGuard, InputEvidenceBudgetLedger};
use crate::runtime_db::{command_task_terminal as producer, task_snapshot as tasks};
use std::sync::Arc;

pub(super) fn project_original_terminal_business_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    task: crate::types::TaskRecord,
    message: Option<&crate::types::MessageEnvelope>,
    at: chrono::DateTime<Utc>,
    fault: Option<TransitionFaultPoint>,
    mode: producer::CommandTaskTerminalInstallMode,
    no_op: bool,
    task_will_change: bool,
    event_kind: &'static str,
    request: &tasks::TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
    guards: &mut Vec<Arc<BudgetViewGuard>>,
) -> Result<(TaskTransitionCommand, Option<(String, String)>)> {
    project_original_terminal_business_inner_tx(
        tx,
        scope,
        task,
        message,
        at,
        fault,
        mode,
        no_op,
        task_will_change,
        event_kind,
        request,
        ledger,
        guards,
        None,
    )
}
pub(super) fn project_original_child_business_using_phase_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    task: crate::types::TaskRecord,
    message: Option<&crate::types::MessageEnvelope>,
    at: chrono::DateTime<Utc>,
    fault: Option<TransitionFaultPoint>,
    mode: producer::CommandTaskTerminalInstallMode,
    no_op: bool,
    task_will_change: bool,
    event_kind: &'static str,
    request: &tasks::TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
    guards: &mut Vec<Arc<BudgetViewGuard>>,
    phase: &mut crate::runtime_db::task_status_metadata::PhaseMaterial<'_>,
) -> Result<(TaskTransitionCommand, Option<(String, String)>)> {
    project_original_terminal_business_inner_tx(
        tx,
        scope,
        task,
        message,
        at,
        fault,
        mode,
        no_op,
        task_will_change,
        event_kind,
        request,
        ledger,
        guards,
        Some(phase),
    )
}
pub(super) fn project_original_terminal_business_inner_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    task: crate::types::TaskRecord,
    message: Option<&crate::types::MessageEnvelope>,
    at: chrono::DateTime<Utc>,
    fault: Option<TransitionFaultPoint>,
    mode: producer::CommandTaskTerminalInstallMode,
    no_op: bool,
    task_will_change: bool,
    event_kind: &'static str,
    request: &tasks::TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
    guards: &mut Vec<Arc<BudgetViewGuard>>,
    mut phase: Option<&mut crate::runtime_db::task_status_metadata::PhaseMaterial<'_>>,
) -> Result<(TaskTransitionCommand, Option<(String, String)>)> {
    let mut command = TaskTransitionCommand {
        agent_id: scope.agent_id.clone(),
        task,
        agent_deletion: None,
        task_result_settlement: None,
        queue_entry: None,
        work_items: vec![],
        expected_wait_conditions: vec![],
        wait_conditions: vec![],
        agent_state: None,
        message_evidence: vec![],
        audit_events: vec![],
        index_changes: vec![],
        notify_scheduler: false,
        commit_on_idempotent: false,
        fault,
    };
    command.commit_on_idempotent = !no_op && message.is_some() && !task_will_change;
    if task_will_change {
        command
            .index_changes
            .push(crate::runtime_db::RuntimeIndexChange::for_task(
                &command.task,
            ));
    }
    let mut requested = None;
    if mode == producer::CommandTaskTerminalInstallMode::Current && !no_op {
        let (state, state_guard) = if let Some(phase) = phase.as_deref_mut() {
            let raw:usize=tx.query_row("SELECT length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+COALESCE(length(CAST(current_run_id AS BLOB)),0) FROM agent_states WHERE agent_id=?1",[&scope.agent_id],|row|row.get(0))?;
            let raw_bytes = raw
                .checked_add(256)
                .and_then(|n| n.checked_mul(8))
                .context("child prepared State bytes overflow")?;
            guards.push(producer::charge_material(
                scope,
                raw_bytes,
                8,
                &phase.remaining()?,
                ledger,
            )?);
            phase.include(raw_bytes, 8)?;
            let nodes: usize = tx.query_row(
                "SELECT COUNT(*) FROM agent_states s,json_tree(s.payload_json) WHERE s.agent_id=?1",
                [&scope.agent_id],
                |row| row.get(0),
            )?;
            let node_records = nodes
                .checked_mul(8)
                .context("child prepared State nodes overflow")?;
            let node_bytes = node_records
                .checked_mul(256)
                .context("child prepared State node bytes overflow")?;
            guards.push(producer::charge_material(
                scope,
                node_bytes,
                node_records,
                &phase.remaining()?,
                ledger,
            )?);
            phase.include(node_bytes, node_records)?;
            producer::read_original_state_tx(tx, scope, &phase.remaining()?, ledger)?
        } else {
            producer::read_original_state_tx(tx, scope, request, ledger)?
        };
        guards.push(state_guard);
        let mut next = state.clone();
        // Same projection as the original reducer: this producer cannot
        // clear another dispatch's run or wake an administratively stopped owner.
        if next.current_run_id.is_none()
            && !matches!(
                next.status,
                crate::types::AgentStatus::Asleep | crate::types::AgentStatus::Stopped
            )
        {
            next.status = crate::types::AgentStatus::AwakeIdle;
        }
        if let Some(message) = message {
            let mut message = message.clone();
            message.normalize_admission_fields();
            if let Some(metadata) = message.metadata.as_mut().and_then(|v| v.as_object_mut()) {
                metadata.insert(
                    "task_detail".into(),
                    serde_json::to_value(&command.task.detail)?,
                );
            }
            let bytes: usize = tx.query_row("SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB)) FROM queue_entries WHERE message_id=?1),0)", [&message.id], |r| r.get(0))?;
            let remaining = phase.as_ref().map(|phase| phase.remaining()).transpose()?;
            guards.push(producer::charge_material(
                scope,
                bytes
                    .checked_add(512)
                    .and_then(|n| n.checked_mul(8))
                    .context("Command queue material overflow")?,
                8,
                remaining.as_ref().unwrap_or(request),
                ledger,
            )?);
            if let Some(phase) = phase.as_deref_mut() {
                phase.include(
                    bytes
                        .checked_add(512)
                        .and_then(|n| n.checked_mul(8))
                        .context("child prepared Queue bytes overflow")?,
                    8,
                )?;
            }
            let old: Option<String> = tx
                .query_row(
                    "SELECT payload_json FROM queue_entries WHERE message_id=?1",
                    [&message.id],
                    |r| r.get(0),
                )
                .optional()?;
            let old = old
                .map(|raw| crate::runtime_db::repositories::decode_queue_entry_payload(&raw))
                .transpose()?;
            if let Some(old) = &old {
                anyhow::ensure!(
                    old.agent_id == scope.agent_id && old.message_id == message.id,
                    "Command result Queue owner differs"
                );
            }
            let admissible = old.as_ref().is_none_or(|old| {
                matches!(
                    old.status,
                    QueueEntryStatus::Queued | QueueEntryStatus::Interrupted
                )
            });
            if admissible {
                command.queue_entry = Some(QueueEntryRecord {
                    message_id: message.id.clone(),
                    agent_id: scope.agent_id.clone(),
                    priority: message.priority.clone(),
                    status: QueueEntryStatus::Queued,
                    created_at: old
                        .as_ref()
                        .map_or(message.created_at, |old| old.created_at),
                    updated_at: at,
                });
                let pending: usize = tx.query_row("SELECT COUNT(*) FROM queue_entries WHERE agent_id=?1 AND status IN ('queued','interrupted')", [&scope.agent_id], |r|r.get(0))?;
                let already_pending = old.as_ref().is_some_and(|old| {
                    matches!(
                        old.status,
                        QueueEntryStatus::Queued
                            | QueueEntryStatus::Dequeued
                            | QueueEntryStatus::Interrupted
                    )
                });
                next.pending = pending
                    .checked_add(usize::from(!already_pending))
                    .context("Command pending count overflow")?;
                next.last_wake_reason = Some("TaskResult".into());
                let (count, exists):(usize,bool) = tx.query_row("SELECT (SELECT COUNT(*) FROM messages WHERE agent_id=?1),EXISTS(SELECT 1 FROM messages WHERE agent_id=?1 AND evidence_id=?2)", params![scope.agent_id,message.id],|r|Ok((r.get(0)?,r.get(1)?)))?;
                next.total_message_count = count
                    .checked_add(usize::from(!exists))
                    .context("Command Message count overflow")?;
                crate::runtime::original_task_message_wake_projection(&mut next);
                command.notify_scheduler = !already_pending;
                if !exists {
                    command.audit_events.push(crate::types::AuditEvent::legacy("message_admitted", serde_json::json!({
                            "message_id":message.id,"agent_id":message.agent_id,"kind":message.kind,"origin":message.origin,
                            "authority_class":message.authority_class,"delivery_surface":message.delivery_surface,"admission_context":message.admission_context,
                            "trigger_kind":message.trigger_kind,"work_item_id":message.work_item_id,"task_id":message.task_id,
                            "source_refs":message.source_refs,"correlation_id":message.correlation_id,"causation_id":message.causation_id,
                        })));
                    command.audit_events.push(crate::types::AuditEvent::typed(
                        crate::runtime_event::RuntimeEventKind::MessageEnqueued,
                        &crate::types::MessageLifecycleAuditEvent::from_message(&message),
                    )?);
                }
            }
            command.task_result_settlement =
                crate::runtime_db::TaskResultSettlementRecord::pending(
                    &command.task,
                    &message,
                    at,
                )?;
            requested = Some((message.id.clone(), canonical_command_digest(&message)?));
            command.message_evidence.push(message);
        }
        if next != state {
            command.agent_state = Some(AgentStateMutation {
                expected: Some(Box::new(state)),
                record: Box::new(next),
            });
        }
    }
    // Original stable terminal audit is preserved for Current and NoState;
    // retired disposition cannot describe a canonical Task update.
    if mode != producer::CommandTaskTerminalInstallMode::Retired && !no_op && message.is_some() {
        let payload = crate::types::TaskLifecycleAuditEvent::from_task(&command.task);
        let mut event = if let Some(kind) =
            crate::runtime_event::RuntimeEventKind::from_wire_name(event_kind)
        {
            crate::types::AuditEvent::typed(kind, &payload)?
        } else {
            crate::types::AuditEvent::legacy(event_kind, serde_json::to_value(payload)?)
        };
        event.id = crate::runtime::original_terminal_task_event_id(event_kind, &command.task);
        event.created_at = command.task.updated_at;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM audit_events WHERE audit_event_id=?1)",
            [&event.id],
            |r| r.get(0),
        )?;
        if !exists {
            command.audit_events.push(event);
        }
    }
    Ok((command, requested))
}
