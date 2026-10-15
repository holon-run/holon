//! Original Control owns business permission; CurrentRead joins its exact SQL ACK.
use super::*;
use crate::runtime::{
    current_read_facts::{borrowed_current_read_material, CurrentReadKey, CurrentReadRequest},
    input_evidence_budget::{InputEvidenceBudgetLedger, InputMemoryCategory, OwnedBytesEstimate},
    resident_input_evidence::InputEvidenceScope,
};
use crate::runtime_db::current_read_snapshot::{self as reads, CurrentReadBusinessTransaction};
use crate::runtime_db::transitions::{wait_trigger_audit_id, WaitTriggerAuditKind};

pub(super) struct ControlCurrentReads<'tx, 'conn> {
    business: CurrentReadBusinessTransaction<'tx, 'conn>,
    existing_queues: BTreeSet<CurrentReadKey>,
}
impl<'tx, 'conn> ControlCurrentReads<'tx, 'conn> {
    pub(super) fn business_mut(&mut self) -> &mut CurrentReadBusinessTransaction<'tx, 'conn> {
        &mut self.business
    }
    pub(super) fn compare_queue_tx(
        &mut self,
        tx: &Transaction<'_>,
        expected: &QueueEntryRecord,
        record: &QueueEntryRecord,
    ) -> Result<bool> {
        // Raw original cleanup remains lawful but cannot acquire membership.
        if self
            .existing_queues
            .iter()
            .any(|key| key.id == expected.message_id)
        {
            self.business.compare_and_set_queue_tx(tx, expected, record)
        } else {
            compare_and_set_queue_entry_tx(tx, expected, record)
        }
    }
    pub(super) fn finish(
        self,
        state: &AgentState,
    ) -> Result<reads::CommittedCurrentReadMaintenance> {
        self.business.finish_original_maintenance(state)
    }
}
pub(super) fn prepare_tx<'tx, 'conn>(
    tx: &'tx Transaction<'conn>,
    command: &PreparedControl,
    ledger: &InputEvidenceBudgetLedger,
    event_roles: Option<&crate::runtime::current_event_queries::CurrentEventRoleSnapshot>,
) -> Result<ControlCurrentReads<'tx, 'conn>> {
    let limits = ledger.limits();
    let mut request = CurrentReadRequest::current(
        usize::try_from(
            limits
                .history_per_agent_bytes
                .checked_add(limits.pending_per_agent_bytes)
                .context("Control CurrentRead allowance overflow")?,
        )?,
        usize::try_from(limits.record_limit)?,
    );
    let role_prior = match event_roles {
        Some(roles) => roles.prepare_address_request(
            &command.key.agent_id,
            command.key.identity_incarnation,
            ledger,
            &mut request,
        )?,
        None => (0, 0),
    };
    let address_bytes = command
        .key
        .agent_id
        .len()
        .checked_mul(8)
        .and_then(|n| n.checked_add(512))
        .context("Control CurrentRead address overflow")?;
    request.check_material(
        address_bytes
            .checked_add(role_prior.0)
            .context("Control Event role address bytes overflow")?,
        32usize
            .checked_add(role_prior.1)
            .context("Control Event role address nodes overflow")?,
    )?;
    let address = ledger.register_host_owned_bytes(
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: 0,
            metadata_estimate_bytes: u64::try_from(address_bytes)?,
        },
    )?;
    let scope = InputEvidenceScope {
        agent_id: command.key.agent_id.clone(),
        identity_incarnation: command.key.identity_incarnation,
    };
    let (body_bytes, nodes) =
        borrowed_current_read_material(&(&command.expected.queue_entries, &command.audit_events))?;
    let mut bytes = body_bytes;
    let mut keys = command
        .expected
        .queue_entries
        .len()
        .checked_add(command.audit_events.len())
        .context("Control CurrentRead key count overflow")?;
    for result in &command.task_results {
        bytes = bytes
            .checked_add(
                result
                    .message
                    .id
                    .len()
                    .checked_mul(3)
                    .context("Control CurrentRead key bytes overflow")?,
            )
            .and_then(|n| {
                n.checked_add(
                    WaitTriggerAuditKind::Triggered.prefix().len()
                        + WaitTriggerAuditKind::Stale.prefix().len()
                        + 3 * 256,
                )
            })
            .context("Control CurrentRead key bytes overflow")?;
        keys = keys
            .checked_add(3)
            .context("Control CurrentRead key count overflow")?;
    }
    bytes = bytes
        .checked_add(scope.agent_id.len())
        .and_then(|n| n.checked_add(512))
        .and_then(|n| n.checked_mul(8))
        .and_then(|n| n.checked_add(address_bytes))
        .context("Control CurrentRead material overflow")?;
    let records = nodes
        .checked_mul(2)
        .and_then(|n| keys.checked_mul(8).and_then(|k| n.checked_add(k)))
        .and_then(|n| n.checked_add(64))
        .context("Control CurrentRead native overflow")?;
    let prior = (
        bytes
            .checked_add(role_prior.0)
            .context("Control Event role bytes overflow")?,
        records
            .checked_add(role_prior.1)
            .context("Control Event role nodes overflow")?,
    );
    request.check_material(prior.0, prior.1)?;
    let guard = ledger.register_owned_bytes(
        &scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: u64::try_from(bytes)?,
            metadata_estimate_bytes: 0,
        },
    )?;
    drop(address);
    // All owned ID/set/record copies below were admitted by this borrowed pass.
    let existing = command
        .expected
        .queue_entries
        .iter()
        .map(|queue| CurrentReadKey::queue(&queue.message_id))
        .collect();
    let selected = reads::qualified_existing_cleanup_tx(
        tx,
        &scope,
        &existing,
        &request,
        ledger,
        prior,
        vec![guard],
    )?;
    let mut frozen = selected.keys.clone();
    for event in &command.audit_events {
        frozen.insert(CurrentReadKey::event(&event.id));
    }
    for result in &command.task_results {
        frozen.insert(CurrentReadKey::queue(&result.message.id));
        frozen.insert(CurrentReadKey::event(wait_trigger_audit_id(
            &result.message.id,
            WaitTriggerAuditKind::Triggered,
        )));
        frozen.insert(CurrentReadKey::event(wait_trigger_audit_id(
            &result.message.id,
            WaitTriggerAuditKind::Stale,
        )));
    }
    let mut business = CurrentReadBusinessTransaction::prepare_original_maintenance_tx(
        tx,
        &scope,
        &command.expected.state,
        &frozen,
        &request,
        ledger,
        selected.material,
        selected.guards,
    )?;
    business.observe_original_control_queue_tx(tx)?;
    Ok(ControlCurrentReads {
        business,
        existing_queues: selected.keys,
    })
}

/// Only the original Control reader mints this after validating its whole
/// durable receipt in the same fresh transaction. No caller-supplied scope DTO
/// can turn an incomplete Queue namespace into a Current body domain.
pub(in crate::runtime_db) struct OriginalControlCurrentReadRecovery<'tx, 'conn> {
    tx: &'tx Transaction<'conn>,
    receipt: &'tx ControlReceipt,
}
pub(super) fn original_recovery<'tx, 'conn>(
    tx: &'tx Transaction<'conn>,
    receipt: &'tx ControlReceipt,
) -> OriginalControlCurrentReadRecovery<'tx, 'conn> {
    OriginalControlCurrentReadRecovery { tx, receipt }
}
impl OriginalControlCurrentReadRecovery<'_, '_> {
    pub(in crate::runtime_db) fn prepare_current_tx(
        &self,
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
        state: &AgentState,
        request: &CurrentReadRequest,
        ledger: &InputEvidenceBudgetLedger,
        input: &mut crate::runtime_db::input_evidence_snapshot::read_material::InputReadMaterial<
            '_,
        >,
    ) -> Result<(
        Option<reads::CurrentReadSnapshot>,
        Vec<Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
    )> {
        ensure!(
            std::ptr::eq(self.tx, tx)
                && self.receipt.key.agent_id == scope.agent_id
                && self.receipt.key.identity_incarnation == scope.identity_incarnation,
            "Control CurrentRead recovery differs from its original receipt/view"
        );
        match reads::prepare_current_read_using_input_material_tx(tx,scope,state,request,ledger,input,true) {
            Ok(snapshot)=>Ok((Some(snapshot),Vec::new())),
            Err(error) if matches!(error.downcast_ref::<crate::runtime::current_read_facts::CurrentReadError>(),
                Some(crate::runtime::current_read_facts::CurrentReadError::UnknownNamespace(crate::runtime::current_read_facts::CurrentReadFamily::Queue)
                    |crate::runtime::current_read_facts::CurrentReadError::HistoricalUnavailable)) => {
                // Sparse recovery keeps exact current metadata/other domains;
                // it never invents Queue/Event aliases or a ready Current root.
                let guards=reads::observe_original_incomplete_recovery_using_input_tx(tx,scope,state,request,ledger,input)?;
                Ok((None,guards))
            }
            Err(error)=>Err(error),
        }
    }
}
