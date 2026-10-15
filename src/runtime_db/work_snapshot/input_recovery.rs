//! Borrow original recovery selectors until their first owned request is admitted.
use super::read_preparation::WorkReadMaterial;
use super::*;
use crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger;
use crate::types::{MessageEnvelope, MessageKind, MessageOrigin};

pub(in crate::runtime_db) fn prepare_original_work_for_input_recovery_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    state: &AgentState,
    capture: &WorkCaptureRequest,
    task_roots: Option<&BTreeSet<String>>,
    pending: &[MessageEnvelope],
    current_roots_only: bool,
    limits: (usize, usize),
    ledger: &InputEvidenceBudgetLedger,
    memory: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
) -> Result<(WorkCurrentSnapshot, BTreeSet<String>)> {
    let policy = WorkDomainLoadRequest {
        capture: WorkCaptureRequest::current(),
        record_limit: limits.0,
        byte_limit: limits.1,
    };
    let mut material = WorkReadMaterial::new(scope, &policy, ledger, memory)?;
    material.selectors(capture)?;
    material.input_ids(task_roots.into_iter().flatten().map(String::as_str))?;
    material.input_ids(state.current_work_item_id.iter().map(String::as_str))?;
    for message in pending {
        material.input_ids(std::iter::once(message.id.as_str()))?;
        if message.kind == MessageKind::TaskResult {
            if let MessageOrigin::Task { task_id } = &message.origin {
                ensure!(
                    message.task_id.as_deref() == Some(task_id.as_str()),
                    "pending TaskResult source differs"
                );
                material.input_ids(std::iter::once(task_id.as_str()))?;
            }
        }
    }
    // All original/derived ID copies and request/selection tree nodes are now
    // covered by this same phase; no qualification follows from these IDs.
    let mut capture = capture.clone();
    capture.mandatory_roots.extend(
        task_roots
            .into_iter()
            .flatten()
            .map(|id| WorkKey::new(WorkFamily::Work, id)),
    );
    for message in pending {
        capture
            .required_trigger_message_ids
            .insert(message.id.clone());
        if message.kind == MessageKind::TaskResult {
            if let MessageOrigin::Task { task_id } = &message.origin {
                capture.required_task_ids.insert(task_id.clone());
            }
        }
    }
    capture
        .latest_result_work_ids
        .extend(state.current_work_item_id.clone());
    let selected = capture.latest_result_work_ids.clone();
    let request = WorkDomainLoadRequest { capture, ..policy };
    let mut current = loader::prepare_work_domain_snapshot_with_material_tx(
        tx,
        scope,
        &request,
        true,
        true,
        &mut material,
    )?
    .require_current()?;
    if current_roots_only {
        current.owning_input_roots =
            loader::current_recovery_owning_input_roots(&current.resident, state, &mut material)?;
    }
    current.material_usage = material.usage();
    current.material_views = material.into_guards();
    Ok((current, selected))
}
