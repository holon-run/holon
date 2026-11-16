//! One qualified reply/owner cut for input selection and transactional revalidation.
use super::*;
use crate::{
    runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    runtime_db::{
        context_inputs::ContextDurableRead,
        work_snapshot::{preflight_named_work_snapshot_tx, WorkDomainLoadRequest},
    },
};

pub(crate) fn reply_work_owner_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    message_id: &str,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<Option<String>> {
    let read = ContextDurableRead::new(tx, &scope.agent_id).scope_read()?;
    read.validate(tx, scope)?;
    let limits = ledger.limits();
    let record_limit = usize::try_from(limits.record_limit.min(MAX_RECORDS as u64))?;
    let byte_limit = usize::try_from(
        limits
            .history_per_agent_bytes
            .checked_add(limits.pending_per_agent_bytes)
            .context("reply owner byte limit overflow")?
            .min(MAX_BYTES as u64),
    )?;
    crate::diagnostics::resource_io::admit_cloned_value(&message_id)?;
    let query = TaskQuerySelection {
        reply_message_ids: BTreeSet::from([message_id.to_owned()]),
        ..Default::default()
    };
    let request = TaskDomainLoadRequest {
        query,
        capture: TaskCaptureRequest {
            include_active: false,
            include_unsettled: false,
            terminal_limit: 0,
            recovery_source_ids: Default::default(),
            required_keys: Default::default(),
        },
        recovery_at: None,
        record_limit,
        byte_limit,
    };
    let tasks = preflight_named_task_snapshot_tx(tx, scope, &request, ledger, &read)?
        .require_current()?
        .resident
        .with_budget(ledger.clone())?
        .capture_query(&request.query)?;
    let work_request = WorkDomainLoadRequest {
        capture: crate::runtime::task_reads::reply_work_request(&tasks, message_id)?,
        record_limit,
        byte_limit,
    };
    let work = preflight_named_work_snapshot_tx(tx, scope, &work_request, &read)?
        .require_current()?
        .resident
        .with_budget(ledger.clone())?
        .capture(&work_request.capture)?;
    let owner = crate::runtime::task_reads::reply_work_owner(&tasks, &work, message_id)?;
    crate::diagnostics::resource_io::admit_cloned_value(&owner)?;
    Ok(owner.map(str::to_owned))
}
