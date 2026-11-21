//! An initial Stop observes its child readonly; the parent owns its receipt.
use super::*;
use crate::{
    runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    runtime_db::{
        input_mutation_receipts::{
            InputMutationReceipt, InputMutationResult, PreparedInputMutation,
        },
        task_stop,
    },
    types::TaskRecord,
};

pub(super) fn assert_parent_owned_readonly_stop_tx(
    tx: &Transaction<'_>,
    purge_agent_id: &str,
    prepared: &PreparedInputMutation,
    receipt: &InputMutationReceipt,
) -> Result<()> {
    let InputMutationResult::Task {
        task_id,
        stop: Some(binding),
        deletion: None,
    } = &receipt.outcome.result
    else {
        bail!("shared Stop is not an original readonly child observation");
    };
    // A maintenance-local bounded view, never a runtime owner/capability.
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let source = task_stop::load_task_stop_by_sequence_tx(
        tx,
        &binding.scope,
        binding.effect_seq,
        64 * 1024 * 1024,
        &ledger,
    )?;
    let record = source
        .records
        .first()
        .context("original shared Stop source absent")?;
    let child = record
        .intent
        .child
        .as_ref()
        .context("shared Stop has no original child")?;
    let parent = &binding.scope;
    ensure!(
        record.prepared == *prepared
            && record.receipt_binding() == *binding
            && record.intent.task.key.id == *task_id
            && child.parent_agent_id == parent.agent_id
            && child.creation_task_id == *task_id
            && child.scope != *parent
            && prepared.scopes().len() == 2
            && prepared.scopes().contains(parent)
            && prepared.scopes().contains(&child.scope)
            && (purge_agent_id == child.scope.agent_id || purge_agent_id == parent.agent_id)
            && receipt.outcome.delivery_receipt.is_none(),
        "shared Stop differs from its exact original parent/child group"
    );
    let observed = receipt
        .outcome
        .scopes
        .iter()
        .find(|s| s.scope == child.scope)
        .context("original Stop readonly child ACK absent")?;
    ensure!(
        observed.install_keys == Default::default()
            && observed.audit_events.is_empty()
            && !observed.notify_memory_index
            && !observed.notify_scheduler,
        "original Stop child scope has mutation effects"
    );
    task_stop::validate_task_stop_readonly_child_tx(tx, child)?;
    let relations = super::super::agent_relations::canonical_relations_from_connection(
        tx,
        &child.scope.agent_id,
    )?
    .context("original Stop child canonical relations absent")?;
    ensure!(
        relations.resolution == crate::types::AgentCanonicalResolution::Resolved
            && relations
                .lineage
                .as_ref()
                .is_some_and(|l| l.parent_agent_id == parent.agent_id
                    && l.child_agent_id == child.scope.agent_id)
            && relations
                .supervision
                .as_ref()
                .is_some_and(|s| s.supervisor_agent_id == parent.agent_id
                    && s.delegated_from_task_id.as_deref() == Some(task_id.as_str())),
        "original Stop child canonical lineage differs"
    );
    let identity = identity_tx(tx, &parent.agent_id)?;
    ensure!(
        identity.incarnation == parent.identity_incarnation,
        "original Stop parent incarnation changed"
    );
    let (owner, linked_child, kind, status, raw): (String, Option<String>, String, String, String) = tx.query_row(
        "SELECT owner_agent_id,child_agent_id,kind,status,payload_json FROM tasks WHERE task_id=?1",
        [task_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )?;
    let task: TaskRecord = serde_json::from_str(&raw)?;
    record.intent.validate_original(&task)?;
    ensure!(
        owner == parent.agent_id
            && task.agent_id == owner
            && linked_child.as_deref() == Some(child.scope.agent_id.as_str())
            && task
                .detail
                .as_ref()
                .and_then(|d| d.get("child_agent_id"))
                .and_then(serde_json::Value::as_str)
                == Some(child.scope.agent_id.as_str())
            && task.kind.as_str() == kind
            && task.status == crate::types::TaskStatus::Cancelled
            && status == "cancelled",
        "original Stop parent Task is not canonically cancelled"
    );
    if task.detail.as_ref().and_then(|d| d.get("target")).is_some() {
        ensure!(
            task.target()
                .is_some_and(|t| t.kind == crate::types::TaskTargetKind::ChildAgent
                    && t.agent_id == child.scope.agent_id
                    && t.incarnation == Some(child.scope.identity_incarnation)),
            "original Stop parent typed target differs or has unknown incarnation"
        );
    }
    ensure!(
        task.detail
            .as_ref()
            .and_then(|d| d.get("target_identity_incarnation"))
            .is_none_or(|v| v.as_u64() == Some(child.scope.identity_incarnation)),
        "original Stop parent legacy target incarnation differs"
    );
    if purge_agent_id == parent.agent_id {
        // Parent-first purge must not strand the child's physical cleanup.
        assert_private_history_cleared_tx(tx, &child.scope.agent_id)?;
    }
    if table_exists(tx, "dispatch_memory_auto_commands")? {
        ensure!(
            !exists(
                tx,
                "SELECT EXISTS(SELECT 1 FROM dispatch_memory_auto_commands WHERE command_nonce=?1)",
                prepared.command_nonce()
            )?,
            "readonly Stop unexpectedly owns automatic command history"
        );
    }
    Ok(())
}
