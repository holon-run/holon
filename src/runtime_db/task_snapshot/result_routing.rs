//! The accepted TaskResult route reads one parent and its produced Briefs.
use super::*;
use crate::runtime::{
    input_evidence_budget::{BudgetViewGuard, InputEvidenceBudgetLedger},
    resident_input_evidence::{
        InputEvidenceFamily as Family, InputEvidenceKey as Key, PreparedInputEvidence,
        QualifiedInputEvidence,
    },
    AcceptedTaskResultSource,
};
use crate::types::{BriefKind, MessageEnvelope};

/// Selected Arcs and actual material only; no root, connection or storage backref.
pub(crate) struct TaskResultParentRead {
    source: Arc<QualifiedInputEvidence>,
    parent: Option<Arc<QualifiedInputEvidence>>,
    briefs: Vec<Arc<QualifiedInputEvidence>>,
    _material: Vec<Arc<BudgetViewGuard>>,
}
impl TaskResultParentRead {
    pub(crate) fn parent_delivered(&self) -> Result<bool> {
        ensure!(
            matches!(&self.source.record, PreparedInputEvidence::Message(_)),
            "TaskResult source family differs"
        );
        let Some(parent) = &self.parent else {
            return Ok(false);
        };
        let PreparedInputEvidence::Turn(turn) = &parent.record else {
            anyhow::bail!("TaskResult parent family differs")
        };
        for brief in &self.briefs {
            let PreparedInputEvidence::Brief(brief) = &brief.record else {
                anyhow::bail!("TaskResult produced Brief family differs")
            };
            ensure!(
                brief.agent_id == turn.agent_id
                    && brief.turn_id.as_deref().is_none_or(|id| id == turn.turn_id)
                    && turn.produced_brief_ids.contains(&brief.id),
                "TaskResult produced Brief owning relation differs"
            );
            if brief.kind == BriefKind::Result {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
fn parent_id(task: &crate::types::TaskRecord) -> Option<&str> {
    task.detail
        .as_ref()
        .and_then(|detail| detail.get("parent_turn_id"))
        .and_then(serde_json::Value::as_str)
}

/// Called only after the original EnsurePending/Admit ACK, while the same owner
/// remains held. Presence and accepted-source authority are separate branches.
pub(crate) fn prepare_task_result_parent_read_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    expected_state: &AgentState,
    task_revision: u64,
    work_revision: u64,
    current_task: Option<&QualifiedTaskFact>,
    message: &MessageEnvelope,
    candidate: &crate::types::TaskRecord,
    source_proof: Option<&AcceptedTaskResultSource<'_>>,
    selected_material: (usize, usize),
    request: &TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<TaskResultParentRead> {
    ensure!(
        candidate.agent_id == scope.agent_id && message.agent_id == scope.agent_id,
        "TaskResult route scope differs"
    );
    let (record_limit, byte_limit) =
        TaskDomainLoadRequest::bounded_material_limits(request.record_limit, request.byte_limit);
    let remaining = TaskDomainLoadRequest {
        query: Default::default(),
        capture: TaskCaptureRequest {
            include_active: false,
            include_unsettled: false,
            terminal_limit: 0,
            required_keys: BTreeSet::new(),
        },
        record_limit: record_limit
            .checked_sub(selected_material.1)
            .filter(|n| *n > 0)
            .ok_or(TaskSnapshotError::ResourceExhausted(
                "TaskResult selected record overlap",
            ))?,
        byte_limit: byte_limit
            .checked_sub(selected_material.0)
            .filter(|n| *n > 0)
            .ok_or(TaskSnapshotError::ResourceExhausted(
                "TaskResult selected byte overlap",
            ))?,
    };
    let key = TaskKey::new(TaskFamily::Task, &candidate.id);
    let mut read = super::qualified_input_read::prepare_task_input_read_base_tx(
        tx,
        scope,
        expected_state,
        task_revision,
        &key,
        current_task,
        None,
        &remaining,
        ledger,
    )?;
    // Existing selected Arcs have their original Weak/Pins guards. Subtract
    // their overlap; do not register the shared allocation as another copy.
    let actual:Option<u64>=tx.query_row("SELECT revision FROM work_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",params![scope.agent_id,scope.identity_incarnation],|row|row.get(0)).optional()?;
    ensure!(
        actual == Some(work_revision),
        "TaskResult Work revision changed from original publication"
    );
    // Existing Task authority is the exact qualified locator checked above.
    // Incoming parent ownership is separately proved by this original accepted
    // Source/parser; terminal replays may legitimately carry newer parent detail.
    source_proof
        .context("TaskResult parent requires original accepted source proof")?
        .validate_candidate(message, candidate)?;
    read.material.charge_ids(
        read_preparation::encoded_metadata_bytes(&(message.id.as_str(), parent_id(candidate)))?
            + 512,
        4,
    )?;
    let source = super::super::input_evidence_snapshot::read_authorized_task_input_tx(
        tx,
        &mut read,
        &Key::new(Family::Message, &message.id),
        false,
    )?
    .context("accepted TaskResult source is absent")?;
    ensure!(
        matches!(&source.record,PreparedInputEvidence::Message(actual) if actual==message),
        "TaskResult source differs from accepted canonical Message"
    );
    let mut parent = None;
    let mut briefs = Vec::new();
    if let Some(id) = parent_id(candidate) {
        parent = super::super::input_evidence_snapshot::read_authorized_task_input_tx(
            tx,
            &mut read,
            &Key::new(Family::Turn, id),
            true,
        )?;
        if let Some(parent) = &parent {
            let PreparedInputEvidence::Turn(turn) = &parent.record else {
                anyhow::bail!("TaskResult parent family differs")
            };
            ensure!(
                turn.agent_id == scope.agent_id && turn.turn_id == id,
                "TaskResult parent normalized identity differs"
            );
            read.material.charge_ids(
                read_preparation::encoded_metadata_bytes(&turn.produced_brief_ids)?,
                turn.produced_brief_ids.len(),
            )?;
            for id in &turn.produced_brief_ids {
                let brief = super::super::input_evidence_snapshot::read_authorized_task_input_tx(
                    tx,
                    &mut read,
                    &Key::new(Family::Brief, id),
                    true,
                )?;
                if let Some(brief) = brief {
                    let PreparedInputEvidence::Brief(value) = &brief.record else {
                        anyhow::bail!("TaskResult produced Brief family differs")
                    };
                    ensure!(
                        value.agent_id == scope.agent_id && value.id == *id,
                        "TaskResult produced Brief identity differs"
                    );
                    briefs.push(brief);
                }
            }
        }
    }
    let result = TaskResultParentRead {
        source,
        parent,
        briefs,
        _material: read.material.into_guards(),
    };
    result.parent_delivered()?;
    Ok(result)
}
