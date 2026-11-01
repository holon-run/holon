//! Original child association checks; a Task id never selects a successor identity.
use super::*;
use crate::types::{
    AgentIdentityRecord, AgentKind, AgentOwnership, AgentRegistryStatus, AgentSupervisionState,
    AgentVisibility,
};

pub(crate) struct PreparedTaskStopChildIdentity {
    pub identity: AgentIdentityRecord,
    original_teardown_revision: Option<u64>,
    _original: PreparedTaskStops,
    _budget: Arc<BudgetViewGuard>,
    _parent_budget: Arc<BudgetViewGuard>,
    _associated_budget: Arc<BudgetViewGuard>,
}

impl PreparedTaskStopChildIdentity {
    /// A deleted identity readback alone never proves who deleted it.
    pub(crate) fn proves_original_teardown(
        &self,
        original: &TaskStopRecord,
        identity_revision: u64,
    ) -> bool {
        self.original_teardown_revision == Some(identity_revision)
            && self.identity.revision == identity_revision
            && self.identity.status == AgentRegistryStatus::Deleted
            && self._original.records.first().is_some_and(|record| {
                record.prepared == original.prepared
                    && record.receipt_binding() == original.receipt_binding()
                    && record.intent.child == original.intent.child
            })
            && original.intent.child.as_ref().is_some_and(|child| {
                self.identity.agent_id == child.scope.agent_id
                    && self.identity.incarnation == child.scope.identity_incarnation
            })
    }
}

pub(crate) fn prepare_task_stop_child_identity_tx(
    tx: &Transaction<'_>,
    original: &TaskStopRecord,
    require_deleted: bool,
    byte_limit: usize,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<PreparedTaskStopChildIdentity> {
    let observed = load_task_stop_by_sequence_tx(
        tx,
        &original.intent.task.scope,
        original.effect_seq,
        byte_limit,
        ledger,
    )?;
    let actual = observed
        .records
        .first()
        .context("TaskStop original child intent missing")?;
    ensure!(
        actual.prepared == original.prepared
            && actual.intent == original.intent
            && actual.intent_digest == original.intent_digest
            && actual.ack_task_digest == original.ack_task_digest,
        "TaskStop original child intent changed"
    );
    ensure!(
        matches!(
            actual.progress.child,
            TaskStopChildState::Pending | TaskStopChildState::RetiredJoined
        ),
        "TaskStop has no original child retirement responsibility"
    );
    let parent_budget = validate_current_parent_generation_tx(tx, actual, byte_limit, ledger)?;
    let target = actual
        .intent
        .child
        .as_ref()
        .context("TaskStop original child identity unavailable")?;
    let (identity, budget) =
        read_original_child_identity_tx(tx, target, require_deleted, byte_limit, ledger)?;
    let associated_budget =
        prepare_child_associated_material_tx(tx, target, !require_deleted, byte_limit, ledger)?;
    let supervision_matches: bool = tx.query_row(
        "SELECT COALESCE((SELECT supervisor_agent_id=?2 AND delegated_from_task_id=?3 AND (state IN ('active','cleanup_required') OR (?4 AND state='closed')) FROM agent_supervisions WHERE child_agent_id=?1 ORDER BY revision DESC,updated_at DESC,supervision_id DESC LIMIT 1),1)",
        params![target.scope.agent_id,target.parent_agent_id,target.creation_task_id,identity.status == AgentRegistryStatus::Deleted], |row|row.get(0))?;
    ensure!(
        supervision_matches,
        "TaskStop original child supervision changed or was revoked"
    );
    Ok(PreparedTaskStopChildIdentity {
        identity,
        original_teardown_revision: None,
        _original: observed,
        _budget: budget,
        _parent_budget: parent_budget,
        _associated_budget: associated_budget,
    })
}

/// This identity-only gate precedes parent Task body acquisition, including when
/// the parent is retired. It never reads a successor's State.
fn validate_current_parent_generation_tx(
    tx: &Transaction<'_>,
    original: &TaskStopRecord,
    byte_limit: usize,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<Arc<BudgetViewGuard>> {
    let scope = &original.intent.task.scope;
    let same_incarnation: bool=tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_identities WHERE agent_id=?1 AND json_extract(payload_json,'$.agent_id')=?1 AND COALESCE(json_extract(payload_json,'$.incarnation'),1)=?2)",
        params![scope.agent_id,scope.identity_incarnation],|row|row.get(0))?;
    ensure!(
        same_incarnation,
        "TaskStop parent identity generation changed"
    );
    let request = crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
        recovery_at: None,
        query: Default::default(),
        capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
        record_limit: usize::try_from(ledger.limits().record_limit)?,
        byte_limit,
    };
    let actual = crate::runtime_db::task_snapshot::prepare_task_sql_readback_tx(
        tx,
        scope,
        &original.intent.task.key.id,
        &request,
        ledger,
    )?;
    original.intent.validate_original(&actual.task)?;
    let qualified:bool=tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM task_domain_records WHERE agent_id=?1 AND identity_incarnation=?2 AND family='task' AND record_id=?3 AND availability='present' AND content_digest=?4 AND projection_digest=?5)",
        params![scope.agent_id,scope.identity_incarnation,original.intent.task.key.id,actual.content_digest,actual.projection_digest],|row|row.get(0))?;
    ensure!(
        qualified,
        "TaskStop current parent Task is not qualified in the original generation"
    );
    Ok(actual.budget)
}

fn prepare_child_associated_material_tx(
    tx: &Transaction<'_>,
    target: &TaskStopChildTarget,
    include_state: bool,
    byte_limit: usize,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<Arc<BudgetViewGuard>> {
    let bytes:usize=tx.query_row(
        "SELECT COALESCE((SELECT length(CAST(json_array(supervision_id,supervisor_agent_id,delegated_from_work_item_id,delegated_from_task_id,state,revision,created_at,updated_at) AS BLOB)) FROM agent_supervisions WHERE child_agent_id=?1 ORDER BY revision DESC,updated_at DESC,supervision_id DESC LIMIT 1),0)+CASE WHEN ?2 THEN COALESCE((SELECT length(CAST(payload_json AS BLOB)) FROM agent_states WHERE agent_id=?1),0) ELSE 0 END",
        params![target.scope.agent_id,include_state],|row|row.get(0))?;
    let material = bytes
        .checked_mul(8)
        .and_then(|n| n.checked_add(2048))
        .context("TaskStop child associated material overflow")?;
    ensure!(
        8 <= ledger.limits().record_limit && material <= byte_limit.min(MAX_BYTES),
        "TaskStop child associated ResourceExhausted before decode"
    );
    ledger
        .register_owned_bytes(
            &target.scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: u64::try_from(material)?,
                metadata_estimate_bytes: 0,
            },
        )
        .map_err(Into::into)
}

/// Covers the existing archive's active Wait/linked Work filter and pending
/// Queue observations, including cancellation clones/IDs until cleanup returns.
pub(crate) fn prepare_task_stop_archive_material_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    byte_limit: usize,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<Arc<BudgetViewGuard>> {
    let (count,bytes):(usize,usize)=tx.query_row(
        "SELECT SUM(n),SUM(bytes) FROM (SELECT COUNT(*) n,COALESCE(SUM(length(CAST(payload_json AS BLOB))+length(CAST(json_array(wait_condition_id,agent_id,work_item_id,status,kind,source,subject_ref,waiting_for,created_at,updated_at,expires_at,resolved_at,cancelled_at,last_turn_id,trigger_message_id,triggered_at,wake_sources_json,continuation_json) AS BLOB))),0) bytes FROM wait_conditions WHERE agent_id=?1 AND status='active' UNION ALL SELECT COUNT(*),COALESCE(SUM(length(CAST(payload_json AS BLOB))),0) FROM work_items WHERE work_item_id IN (SELECT work_item_id FROM wait_conditions WHERE agent_id=?1 AND status='active' AND work_item_id IS NOT NULL) UNION ALL SELECT COUNT(*),COALESCE(SUM(length(CAST(payload_json AS BLOB))),0) FROM queue_entries WHERE agent_id=?1 AND status IN ('queued','interrupted'))",
        [&scope.agent_id],|row|Ok((row.get(0)?,row.get(1)?)))?;
    let records = count
        .checked_mul(16)
        .context("TaskStop archive record copies overflow")?;
    let material = count
        .checked_mul(512)
        .and_then(|n| n.checked_add(bytes))
        .and_then(|n| n.checked_mul(16))
        .context("TaskStop archive material overflow")?;
    ensure!(
        u64::try_from(records)? <= ledger.limits().record_limit
            && material <= byte_limit.min(MAX_BYTES),
        "TaskStop archive ResourceExhausted before decode"
    );
    ledger
        .register_owned_bytes(
            scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: u64::try_from(material)?,
                metadata_estimate_bytes: 0,
            },
        )
        .map_err(Into::into)
}

fn read_original_child_identity_tx(
    tx: &Transaction<'_>,
    target: &TaskStopChildTarget,
    require_deleted: bool,
    byte_limit: usize,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<(AgentIdentityRecord, Arc<BudgetViewGuard>)> {
    const PROJECTION: &str = "json_array(agent_id,kind,visibility,ownership,profile_preset,status,parent_agent_id,lineage_parent_agent_id,delegated_from_task_id,created_at,updated_at,archived_at)";
    let bytes: usize = tx.query_row(&format!("SELECT length(CAST(payload_json AS BLOB))+length(CAST({PROJECTION} AS BLOB)) FROM agent_identities WHERE agent_id=?1"),
        [&target.scope.agent_id], |row| row.get(0)).optional()?.context("TaskStop original child identity is absent")?;
    let material = u64::try_from(bytes)?
        .checked_add(serialized_bytes(target)?)
        .and_then(|n| n.checked_mul(8))
        .and_then(|n| n.checked_add(2048))
        .context("TaskStop child material overflow")?;
    ensure!(
        8 <= ledger.limits().record_limit && material <= u64::try_from(byte_limit.min(MAX_BYTES))?,
        "TaskStop child ResourceExhausted before decode"
    );
    let budget = ledger.register_owned_bytes(
        &target.scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: material,
            metadata_estimate_bytes: 0,
        },
    )?;
    let (payload, projection): (String, String) = tx.query_row(
        &format!("SELECT payload_json,{PROJECTION} FROM agent_identities WHERE agent_id=?1"),
        [&target.scope.agent_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let identity = crate::runtime_db::repositories::decode_agent_identity_payload(&payload)?;
    let expected = serde_json::json!([
        identity.agent_id,
        identity.kind,
        identity.visibility,
        identity.ownership,
        identity.profile_preset,
        identity.status,
        identity.parent_agent_id,
        identity.lineage_parent_agent_id,
        identity.delegated_from_task_id,
        crate::runtime_db::migrations::timestamp(identity.created_at),
        crate::runtime_db::migrations::timestamp(identity.updated_at),
        identity
            .deleted_at
            .map(crate::runtime_db::migrations::timestamp)
    ]);
    ensure!(
        serde_json::from_str::<serde_json::Value>(&projection)? == expected,
        "TaskStop child normalized identity differs from payload"
    );
    ensure!(
        identity.agent_id == target.scope.agent_id
            && identity.incarnation == target.scope.identity_incarnation
            && identity.kind == AgentKind::Child
            && identity.visibility == AgentVisibility::Private
            && identity.ownership == Some(AgentOwnership::ParentSupervised)
            && identity.parent_agent_id.as_deref() == Some(target.parent_agent_id.as_str())
            && identity.delegated_from_task_id.as_deref() == Some(target.creation_task_id.as_str()),
        "TaskStop child is not the original identity/parent/creation Task"
    );
    ensure!(
        !require_deleted || identity.status == AgentRegistryStatus::Deleted,
        "TaskStop original child was not retired before cleanup"
    );
    Ok((identity, budget))
}

/// Recheck the original receipt and identity in the actual tombstone transaction.
pub(crate) fn tombstone_task_stop_child_tx(
    tx: &Transaction<'_>,
    original: &TaskStopRecord,
    byte_limit: usize,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<PreparedTaskStopChildIdentity> {
    let mut evidence =
        prepare_task_stop_child_identity_tx(tx, original, false, byte_limit, ledger)?;
    let identity = &mut evidence.identity;
    let original_teardown = identity.status == AgentRegistryStatus::Active;
    if identity.status != AgentRegistryStatus::Deleted {
        let now = std::cmp::max(
            Utc::now(),
            identity.updated_at + chrono::Duration::nanoseconds(1),
        );
        identity.status = AgentRegistryStatus::Deleted;
        identity.deleted_at = Some(now);
        identity.updated_at = now;
        identity.revision = identity
            .revision
            .checked_add(1)
            .context("TaskStop child identity revision overflow")?;
        crate::runtime_db::evidence::upsert_agent_identity_tx(tx, identity)?;
        if original_teardown {
            evidence.original_teardown_revision = Some(identity.revision);
        }
    }
    crate::runtime_db::agent_relations::transition_supervision_state_tx(
        tx,
        &identity.agent_id,
        AgentSupervisionState::Closed,
        identity.revision,
        identity.updated_at,
    )?;
    Ok(evidence)
}

#[cfg(test)]
#[path = "task_stop/child_tests.rs"]
mod tests;
