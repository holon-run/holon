//! Bounded canonical SQL readback for an original TaskStop transaction.
use super::*;
use crate::runtime::input_evidence_budget::{
    BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory, OwnedBytesEstimate,
};

pub(crate) struct PreparedTaskSqlReadback {
    pub task: crate::types::TaskRecord,
    pub content_digest: String,
    pub projection_digest: String,
    pub budget: Arc<BudgetViewGuard>,
}

pub(crate) fn prepare_task_sql_readback_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    id: &str,
    request: &TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<PreparedTaskSqlReadback> {
    let bytes: usize = tx.query_row(
        &format!("SELECT length(CAST(payload_json AS BLOB))+length(CAST({} AS BLOB)) FROM tasks WHERE task_id=?1", projection_sql(tx, TaskFamily::Task)?),
        [id], |row| row.get(0),
    )?;
    let material = bytes
        .checked_add(id.len())
        .and_then(|n| n.checked_add(256))
        .and_then(|n| n.checked_mul(8))
        .context("TaskStop SQL readback material overflow")?;
    bound(8, material, Some(request))?;
    let budget = ledger.register_owned_bytes(
        scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: u64::try_from(material)?,
            metadata_estimate_bytes: 0,
        },
    )?;
    let stored = stored_on(tx, &scope.agent_id, &TaskKey::new(TaskFamily::Task, id))?
        .context("TaskStop actual canonical Task missing")?;
    let content_digest = stored.digest();
    let CanonicalTaskFact::Task(task) = stored.fact else {
        anyhow::bail!("TaskStop SQL readback family differs")
    };
    Ok(PreparedTaskSqlReadback {
        task,
        content_digest,
        projection_digest: stored.projection_digest,
        budget,
    })
}
