//! Focused original Stop progress; process capabilities never come from SQL.
use super::*;
use std::fmt;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct TaskStopPhaseAddress {
    original: PreparedInputMutation,
    binding: TaskStopReceiptBinding,
    expected_revision: u64,
    expected_progress: TaskStopProgress,
}
impl TaskStopPhaseAddress {
    pub(crate) fn from_record(record: &TaskStopRecord) -> Result<Self> {
        // The selected source owns its body guard. Only this small immutable
        // address is copied here; SQL preparation validates the full record and
        // original receipt after actual material admission, never in a factory.
        ensure!(
            record.effect_seq > 0
                && record.revision > 0
                && !record.prepared.command_nonce().is_empty()
                && record.prepared.scopes().contains(&record.intent.task.scope)
                && valid_digest(&record.intent_digest)
                && valid_digest(&record.generation_digest)
                && valid_digest(&record.ack_task_digest),
            "TaskStop phase source header invalid"
        );
        Ok(Self {
            original: record.prepared.clone(),
            binding: record.receipt_binding(),
            expected_revision: record.revision,
            expected_progress: record.progress.clone(),
        })
    }
    pub(in crate::runtime_db) fn immutable_request(
        &self,
    ) -> (&PreparedInputMutation, &TaskStopReceiptBinding) {
        (&self.original, &self.binding)
    }
    pub(crate) fn scope(&self) -> &InputEvidenceScope {
        &self.binding.scope
    }
    pub(crate) fn matches(&self, record: &TaskStopRecord) -> bool {
        self.original == record.prepared && self.binding == record.receipt_binding()
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub(crate) enum TaskStopProgressAction {
    MonitorJoined,
    ChildRetired,
    WorktreeCleanupObserved,
    WorktreeRetained { reason: String },
    PhysicalAttention { reason: String },
    ChildAttention { reason: String },
}
impl TaskStopProgressAction {
    pub(crate) fn validate(&self) -> Result<()> {
        let reason = match self {
            Self::WorktreeRetained { reason }
            | Self::PhysicalAttention { reason }
            | Self::ChildAttention { reason } => Some(reason),
            _ => None,
        };
        ensure!(
            reason.is_none_or(|s| !s.is_empty() && s.len() <= 4096),
            "TaskStop phase reason invalid"
        );
        Ok(())
    }
}

pub(crate) enum TaskStopProgressAuthorization {
    MonitorJoined(Arc<crate::runtime::task_stop_join::TaskStopMonitorJoin>),
    ChildRetired(Arc<crate::host::TaskStopChildRetirement>),
    WorktreeCleanup(Option<Arc<crate::host::TaskStopWorktreeCleanupObserved>>),
    SafeObservation,
}
impl fmt::Debug for TaskStopProgressAuthorization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::MonitorJoined(_) => "OriginalMonitorJoin",
            Self::ChildRetired(_) => "OriginalChildRetirement",
            Self::WorktreeCleanup(_) => "OriginalWorktreeCleanup",
            Self::SafeObservation => "SafeObservation",
        })
    }
}
impl TaskStopProgressAuthorization {
    pub(in crate::runtime_db) fn worktree_observation(
        &self,
    ) -> Option<&Arc<crate::host::TaskStopWorktreeCleanupObserved>> {
        match self {
            Self::WorktreeCleanup(Some(value)) => Some(value),
            _ => None,
        }
    }
    fn validate(&self, action: &TaskStopProgressAction, record: &TaskStopRecord) -> Result<()> {
        let valid = match (self, action) {
            (Self::MonitorJoined(proof), TaskStopProgressAction::MonitorJoined) => {
                proof.matches(record)
            }
            (Self::ChildRetired(proof), TaskStopProgressAction::ChildRetired) => {
                proof.matches(record)
            }
            (
                Self::WorktreeCleanup(Some(proof)),
                TaskStopProgressAction::WorktreeCleanupObserved,
            ) => proof.matches(record),
            (
                Self::SafeObservation,
                TaskStopProgressAction::WorktreeRetained { .. }
                | TaskStopProgressAction::PhysicalAttention { .. }
                | TaskStopProgressAction::ChildAttention { .. },
            ) => true,
            _ => false,
        };
        ensure!(valid, "TaskStop phase has no original physical proof");
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskStopDomainInvalidation {
    pub base_revision: u64,
    pub revision: u64,
}
#[derive(Debug, thiserror::Error)]
pub(crate) enum TaskStopPhaseError {
    #[error("TaskStop sparse/retired phase requires the original Host metadata actor")]
    RequiresMetadataActor,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TaskStopPhaseInstallMode {
    Current,
    NoState,
    Incomplete,
    Retired,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskStopPhaseAck {
    pub original_nonce: String,
    pub binding: TaskStopReceiptBinding,
    pub revision: u64,
    pub updated_at: DateTime<Utc>,
    pub progress: TaskStopProgress,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<super::continuation::TaskStopResultAck>,
    pub install_mode: TaskStopPhaseInstallMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalidation: Option<TaskStopDomainInvalidation>,
}
impl TaskStopPhaseAck {
    pub(crate) fn validate(&self, prepared: &PreparedInputMutation) -> Result<()> {
        ensure!(
            prepared.family() == InputMutationFamily::Task
                && prepared.scopes() == std::slice::from_ref(&self.binding.scope)
                && !self.original_nonce.is_empty()
                && self.original_nonce != prepared.command_nonce()
                && self.binding.effect_seq > 0
                && self.revision > 0
                && valid_digest(&self.binding.intent_digest)
                && valid_digest(&self.binding.generation_digest)
                && valid_digest(&self.binding.task_ack_digest),
            "TaskStop phase ACK address invalid"
        );
        ensure!(
            self.progress
                .reason
                .as_ref()
                .is_none_or(|s| s.len() <= 4096),
            "TaskStop phase ACK reason exceeds bound"
        );
        if let Some(result) = &self.result {
            result.validate(prepared, &self.progress)?;
        }
        if let Some(change) = &self.invalidation {
            ensure!(
                change.base_revision > 0 && change.revision > change.base_revision,
                "TaskStop phase invalidation revision invalid"
            );
        }
        Ok(())
    }
}

/// Only minted after actual original-row CAS/readback. No Task/State permission.
pub(crate) struct ActualTaskStopProgress<'tx, 'connection> {
    tx: &'tx Transaction<'connection>,
    record: Arc<TaskStopRecord>,
    changed: bool,
    material: Arc<BudgetViewGuard>,
}
impl ActualTaskStopProgress<'_, '_> {
    pub(crate) fn record(&self) -> &TaskStopRecord {
        &self.record
    }
    pub(crate) fn changed(&self) -> bool {
        self.changed
    }
    pub(crate) fn budget_view(&self) -> Arc<BudgetViewGuard> {
        self.material.clone()
    }
    pub(crate) fn validate_presence_tx(&self, tx: &Transaction<'_>) -> Result<()> {
        ensure!(
            std::ptr::eq(tx, self.tx),
            "TaskStop progress belongs to another transaction"
        );
        validate_task_stop_phase_ack_tx(tx, &self.ack(None, TaskStopPhaseInstallMode::Current))
    }
    pub(crate) fn ack(
        &self,
        invalidation: Option<TaskStopDomainInvalidation>,
        install_mode: TaskStopPhaseInstallMode,
    ) -> TaskStopPhaseAck {
        TaskStopPhaseAck {
            original_nonce: self.record.prepared.command_nonce().to_owned(),
            binding: self.record.receipt_binding(),
            revision: self.record.revision,
            updated_at: self.record.updated_at,
            progress: self.record.progress.clone(),
            result: None,
            install_mode,
            invalidation,
        }
    }
}

pub(crate) struct SelectedTaskStopPhase<'tx, 'connection> {
    tx: &'tx Transaction<'connection>,
    selected: PreparedTaskStops,
}
/// Frozen Audit source from the original whole Stop and physical observation.
/// It pins existing charged allocations, without a Root/Host/storage back edge.
pub(crate) struct OriginalTaskStopAuditView<'tx, 'connection> {
    tx: &'tx Transaction<'connection>,
    record: Arc<TaskStopRecord>,
    observation: Arc<crate::host::TaskStopWorktreeCleanupObserved>,
    _material: Arc<BudgetViewGuard>,
}
impl<'tx, 'connection> SelectedTaskStopPhase<'tx, 'connection> {
    pub(in crate::runtime_db) fn original_worktree_audit_view(
        &self,
        action: &TaskStopProgressAction,
        authorization: &TaskStopProgressAuthorization,
    ) -> Result<Option<OriginalTaskStopAuditView<'tx, 'connection>>> {
        let Some(observation) = authorization.worktree_observation() else {
            return Ok(None);
        };
        authorization.validate(action, &self.selected.records[0])?;
        if observation.audit_events().is_empty() {
            return Ok(None);
        }
        Ok(Some(OriginalTaskStopAuditView {
            tx: self.tx,
            record: self.selected.records[0].clone(),
            observation: observation.clone(),
            _material: self.selected._budget.clone(),
        }))
    }
}
impl OriginalTaskStopAuditView<'_, '_> {
    pub(in crate::runtime_db) fn scope(&self) -> &InputEvidenceScope {
        &self.record.intent.task.scope
    }
    pub(in crate::runtime_db) fn events(&self) -> &[crate::types::AuditEvent] {
        self.observation.audit_events()
    }
    pub(in crate::runtime_db) fn validate_tx(&self, tx: &Transaction<'_>) -> Result<()> {
        ensure!(
            std::ptr::eq(tx, self.tx) && self.observation.matches(&self.record),
            "Stop Audit source crossed its original transaction/observation"
        );
        Ok(())
    }
    pub(in crate::runtime_db) fn validate_actual_tx(
        &self,
        tx: &Transaction<'_>,
        actual: &ActualTaskStopProgress<'_, '_>,
    ) -> Result<()> {
        self.validate_tx(tx)?;
        actual.validate_presence_tx(tx)?;
        let after = actual.record();
        ensure!(
            self.record.prepared == after.prepared
                && self.record.effect_seq == after.effect_seq
                && self.record.intent_digest == after.intent_digest
                && self.record.generation_digest == after.generation_digest
                && self.record.ack_task_digest == after.ack_task_digest
                && self.scope() == &after.intent.task.scope,
            "Stop Audit ACK is not the original immutable Stop"
        );
        Ok(())
    }
}
pub(crate) fn prepare_task_stop_phase_tx<'tx, 'connection>(
    tx: &'tx Transaction<'connection>,
    address: &TaskStopPhaseAddress,
    charge: impl FnMut(usize, usize) -> Result<Arc<BudgetViewGuard>>,
) -> Result<SelectedTaskStopPhase<'tx, 'connection>> {
    let selected = observe_task_stops_tx(
        tx,
        address.scope(),
        Some(address.binding.effect_seq),
        false,
        charge,
        None,
    )?;
    ensure!(
        selected.records.len() == 1,
        "TaskStop original phase target missing"
    );
    ensure!(
        address.matches(&selected.records[0]),
        "TaskStop phase original address differs"
    );
    Ok(SelectedTaskStopPhase { tx, selected })
}
pub(crate) fn apply_task_stop_progress_tx<'tx, 'connection>(
    selected: SelectedTaskStopPhase<'tx, 'connection>,
    address: &TaskStopPhaseAddress,
    action: &TaskStopProgressAction,
    authorization: &TaskStopProgressAuthorization,
    at: DateTime<Utc>,
) -> Result<ActualTaskStopProgress<'tx, 'connection>> {
    action.validate()?;
    let tx = selected.tx;
    let selected = selected.selected;
    let current = &selected.records[0];
    ensure!(
        address.matches(current),
        "TaskStop phase original address differs"
    );
    authorization.validate(action, current)?;
    let terminal = match action {
        TaskStopProgressAction::MonitorJoined => {
            current.progress.physical == TaskStopPhysicalState::MonitorJoined
        }
        TaskStopProgressAction::ChildRetired => {
            current.progress.child == TaskStopChildState::RetiredJoined
        }
        TaskStopProgressAction::WorktreeCleanupObserved => {
            current.progress.worktree != TaskStopWorktreeState::Pending
        }
        TaskStopProgressAction::WorktreeRetained { .. } => {
            current.progress.worktree == TaskStopWorktreeState::Retained
        }
        TaskStopProgressAction::PhysicalAttention { .. } => {
            current.progress.physical == TaskStopPhysicalState::NeedsAttention
        }
        TaskStopProgressAction::ChildAttention { .. } => {
            current.progress.child == TaskStopChildState::NeedsAttention
        }
    };
    if terminal {
        return Ok(ActualTaskStopProgress {
            tx,
            record: current.clone(),
            changed: false,
            material: selected._budget,
        });
    }
    ensure!(
        matches!(action, TaskStopProgressAction::WorktreeCleanupObserved)
            || (current.revision == address.expected_revision
                && current.progress == address.expected_progress),
        "TaskStop progress revision conflict"
    );
    ensure!(
        at >= current.updated_at,
        "TaskStop progress clock moved backwards"
    );
    let mut next = current.as_ref().clone();
    match action {
        TaskStopProgressAction::MonitorJoined => {
            next.progress.physical = TaskStopPhysicalState::MonitorJoined
        }
        TaskStopProgressAction::ChildRetired => {
            next.progress.child = TaskStopChildState::RetiredJoined
        }
        TaskStopProgressAction::WorktreeCleanupObserved => {
            next.progress.worktree = authorization
                .worktree_observation()
                .context("worktree observation missing")?
                .status();
        }
        TaskStopProgressAction::WorktreeRetained { reason } => {
            next.progress.worktree = TaskStopWorktreeState::Retained;
            next.progress.reason = Some(reason.clone());
        }
        TaskStopProgressAction::PhysicalAttention { reason } => {
            next.progress.physical = TaskStopPhysicalState::NeedsAttention;
            next.progress.reason = Some(reason.clone());
        }
        TaskStopProgressAction::ChildAttention { reason } => {
            next.progress.child = TaskStopChildState::NeedsAttention;
            next.progress.reason = Some(reason.clone());
        }
    }
    current
        .progress
        .validate_successor(&next.progress, &current.intent)?;
    next.revision = next
        .revision
        .checked_add(1)
        .context("TaskStop progress revision overflow")?;
    next.updated_at =
        DateTime::parse_from_rfc3339(&super::super::migrations::timestamp(at))?.with_timezone(&Utc);
    next.validate()?;
    let changed = tx.execute("UPDATE task_stop_effects SET revision=?1,physical_state=?2,child_state=?3,worktree_state=?4,result_state=?5,progress_json=?6,updated_at=?7 WHERE effect_seq=?8 AND agent_id=?9 AND identity_incarnation=?10 AND command_nonce=?11 AND intent_digest=?12 AND generation_digest=?13 AND ack_task_digest=?14 AND revision=?15 AND progress_json=?16",
        params![next.revision,state_name(next.progress.physical)?,state_name(next.progress.child)?,
            state_name(next.progress.worktree)?,state_name(next.progress.result)?,serde_json::to_string(&next.progress)?,
            super::super::migrations::timestamp(at),next.effect_seq,next.intent.task.scope.agent_id,
            next.intent.task.scope.identity_incarnation,next.prepared.command_nonce(),next.intent_digest,
            next.generation_digest,next.ack_task_digest,current.revision,serde_json::to_string(&current.progress)?])?;
    ensure!(changed == 1, "TaskStop progress revision conflict");
    let actual = ActualTaskStopProgress {
        tx,
        record: Arc::new(next),
        changed: true,
        material: selected._budget,
    };
    actual.validate_presence_tx(tx)?;
    Ok(actual)
}

/// First insertion validates actual metadata. Later receipt replay checks its
/// immutable checksum, never a newer mutable progress revision.
pub(crate) fn validate_task_stop_phase_ack_tx(
    tx: &Transaction<'_>,
    ack: &TaskStopPhaseAck,
) -> Result<()> {
    let exact: bool = tx.query_row("SELECT COALESCE((SELECT command_nonce=?1 AND agent_id=?2 AND identity_incarnation=?3 AND intent_digest=?4 AND generation_digest=?5 AND ack_task_digest=?6 AND revision=?7 AND progress_json=?8 AND updated_at=?10 FROM task_stop_effects WHERE effect_seq=?9),0)",
        params![ack.original_nonce,ack.binding.scope.agent_id,ack.binding.scope.identity_incarnation,
            ack.binding.intent_digest,ack.binding.generation_digest,ack.binding.task_ack_digest,
            ack.revision,serde_json::to_string(&ack.progress)?,ack.binding.effect_seq,
            super::super::migrations::timestamp(ack.updated_at)],|r|r.get(0))?;
    ensure!(
        exact,
        "TaskStop phase ACK differs from actual original progress"
    );
    super::continuation::validate_result_phase_insert_tx(tx, ack)?;
    Ok(())
}

/// Integer-only material observation precedes JSON scalar validation/identity decode.
pub(in crate::runtime_db) fn task_stop_phase_identity_material_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
) -> Result<usize> {
    let bytes: usize = tx.query_row(
        "SELECT COALESCE((SELECT length(CAST(agent_id AS BLOB))+length(CAST(status AS BLOB))+length(CAST(payload_json AS BLOB)) FROM agent_identities WHERE agent_id=?1),0)",
        [&scope.agent_id], |row| row.get(0),
    )?;
    bytes
        .checked_add(scope.agent_id.len())
        .and_then(|value| value.checked_add(512))
        .context("Stop phase identity material overflow")
}

/// Exact identity and scalar readiness only; retired never queries State.
pub(super) fn observe_original_task_stop_state_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
) -> Result<Option<TaskStopPhaseInstallMode>> {
    let identity:Option<(bool,bool)>=tx.query_row(
        "SELECT COALESCE(agent_id=json_extract(payload_json,'$.agent_id') AND status=json_extract(payload_json,'$.status') AND COALESCE(json_extract(payload_json,'$.incarnation'),1)>0 AND (json_type(payload_json,'$.incarnation') IS NULL OR json_type(payload_json,'$.incarnation')='integer'),0),COALESCE(COALESCE(json_extract(payload_json,'$.incarnation'),1)=?2 AND status IN ('active','deleting'),0) FROM agent_identities WHERE agent_id=?1",
        params![scope.agent_id,scope.identity_incarnation],|r|Ok((r.get(0)?,r.get(1)?)),
    ).optional()?;
    let current = match identity {
        Some((consistent, current)) => {
            ensure!(consistent, "Stop phase identity projection corrupt");
            current
        }
        None => false,
    };
    if !current {
        return Ok(Some(TaskStopPhaseInstallMode::Retired));
    }
    let has_state: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_states WHERE agent_id=?1)",
        [&scope.agent_id],
        |r| r.get(0),
    )?;
    if !has_state {
        return Ok(Some(TaskStopPhaseInstallMode::NoState));
    }
    Ok(None)
}

pub(in crate::runtime_db) fn observe_task_stop_phase_mode_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
) -> Result<TaskStopPhaseInstallMode> {
    if let Some(unavailable) = observe_original_task_stop_state_tx(tx, scope)? {
        return Ok(unavailable);
    }
    let coverage:Option<(u64,Option<u64>,bool,bool)>=tx.query_row(
        "SELECT revision,json_extract(coverage_json,'$.revision'),COALESCE(json_type(coverage_json,'$.active_complete') IN ('true','false') AND json_type(coverage_json,'$.unsettled_complete') IN ('true','false') AND (json_type(coverage_json,'$.stop') IS NULL OR (json_type(coverage_json,'$.stop')='object' AND json_type(coverage_json,'$.stop.pending_complete') IN ('true','false'))),0),COALESCE(json_extract(coverage_json,'$.active_complete')=1 AND json_extract(coverage_json,'$.unsettled_complete')=1 AND json_extract(coverage_json,'$.stop.pending_complete')=1,0) FROM task_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",
        params![scope.agent_id,scope.identity_incarnation],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
    ).optional()?;
    match coverage {
        Some((revision, payload_revision, valid, complete)) => {
            ensure!(
                revision > 0 && payload_revision == Some(revision) && valid,
                "Stop phase Task coverage scalar projection corrupt"
            );
            Ok(if complete {
                TaskStopPhaseInstallMode::Current
            } else {
                TaskStopPhaseInstallMode::Incomplete
            })
        }
        None => Ok(TaskStopPhaseInstallMode::Incomplete),
    }
}

// Only the sibling original Result writer can create this actual-TX progress
// carrier; the Result-specific observation separately validates actual delivery.
pub(super) fn actual_result_progress_tx<'tx, 'connection>(
    tx: &'tx Transaction<'connection>,
    record: Arc<TaskStopRecord>,
    material: Arc<BudgetViewGuard>,
) -> Result<ActualTaskStopProgress<'tx, 'connection>> {
    let actual = ActualTaskStopProgress {
        tx,
        record,
        changed: true,
        material,
    };
    actual.validate_presence_tx(tx)?;
    Ok(actual)
}
