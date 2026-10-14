//! Qualified original association and execution facts for the held FS interval.
use super::*;
use crate::{
    runtime_db::task_snapshot::{self, TaskDomainLoadRequest},
    system::{EffectiveExecution, ExecutionScopeKind},
};
use std::path::PathBuf;

pub(crate) struct OriginalTaskStopWorktreeRead {
    pub(crate) execution: EffectiveExecution,
    pub(crate) repo_root: PathBuf,
    pub(crate) metadata: serde_json::Value,
    pub(crate) guards: Vec<Arc<BudgetViewGuard>>,
    _child: PreparedTaskStopChildIdentity,
}
pub(crate) fn prepare_original_task_stop_worktree_tx(
    tx: &Transaction<'_>,
    original: &TaskStopRecord,
    request: &TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<Option<OriginalTaskStopWorktreeRead>> {
    let scope = &original.intent.task.scope;
    let identity_bytes = phase::task_stop_phase_identity_material_tx(tx, scope)?;
    let mut guards = vec![worktree_material(
        ledger,
        scope,
        request,
        identity_bytes,
        1,
    )?];
    // No successor State or Task body is read on retired/missing-State paths.
    if phase::observe_original_task_stop_state_tx(tx, scope)?.is_some() {
        return Ok(None);
    }
    let Some(target) = original.intent.worktree.as_ref() else {
        return Ok(None);
    };
    let same_owner: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM tasks WHERE task_id=?1 AND owner_agent_id=?2)",
        params![original.intent.task.key.id, scope.agent_id],
        |r| r.get(0),
    )?;
    if !same_owner {
        return Ok(None);
    }
    let Some(child_target) = original.intent.child.as_ref() else {
        return Ok(None);
    };
    let child_bytes = phase::task_stop_phase_identity_material_tx(tx, &child_target.scope)?;
    guards.push(worktree_material(ledger, scope, request, child_bytes, 1)?);
    let same_child:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_identities WHERE agent_id=?1 AND status='deleted' AND json_extract(payload_json,'$.agent_id')=?1 AND json_extract(payload_json,'$.status')=status AND COALESCE(json_extract(payload_json,'$.incarnation'),1)=?2 AND parent_agent_id=?3 AND delegated_from_task_id=?4)",params![child_target.scope.agent_id,child_target.scope.identity_incarnation,child_target.parent_agent_id,child_target.creation_task_id],|r|r.get(0))?;
    if !same_child {
        return Ok(None);
    }
    let task = task_snapshot::prepare_task_sql_readback_tx(
        tx,
        scope,
        &original.intent.task.key.id,
        request,
        ledger,
    )?;
    if original.intent.validate_original(&task.task).is_err() {
        return Ok(None);
    }
    let qualified:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM task_domain_records WHERE agent_id=?1 AND identity_incarnation=?2 AND family='task' AND record_id=?3 AND availability='present' AND content_digest=?4 AND projection_digest=?5)",
        params![scope.agent_id,scope.identity_incarnation,original.intent.task.key.id,task.content_digest,task.projection_digest],|r|r.get(0))?;
    ensure!(
        qualified,
        "TaskStop worktree current Task qualification differs"
    );
    let Some(metadata) = task.task.detail.as_ref().and_then(|v| v.get("worktree")) else {
        return Ok(None);
    };
    let original_metadata = original
        .ack_task
        .detail
        .as_ref()
        .and_then(|v| v.get("worktree"));
    let Some(root) = original_metadata
        .and_then(|v| v.get("original_cwd"))
        .and_then(|v| v.as_str())
    else {
        return Ok(None);
    };
    if metadata.get("worktree_path").and_then(|v| v.as_str()) != Some(target.path.as_str())
        || metadata.get("worktree_branch").and_then(|v| v.as_str()) != Some(target.branch.as_str())
        || metadata.get("original_cwd").and_then(|v| v.as_str()) != Some(root)
        || !std::path::Path::new(root).is_absolute()
        || !std::path::Path::new(&target.path).is_absolute()
    {
        return Ok(None);
    }
    let state_bytes: usize = tx.query_row(
        "SELECT length(CAST(payload_json AS BLOB)) FROM agent_states WHERE agent_id=?1",
        [&scope.agent_id],
        |r| r.get(0),
    )?;
    guards.push(worktree_material(ledger, scope, request, state_bytes, 1)?);
    let raw: String = tx.query_row(
        "SELECT payload_json FROM agent_states WHERE agent_id=?1",
        [&scope.agent_id],
        |r| r.get(0),
    )?;
    let state = crate::runtime_db::repositories::decode_agent_state_payload(&raw)?;
    ensure!(
        state.id == scope.agent_id,
        "TaskStop worktree State owner differs"
    );
    let repo_root = PathBuf::from(root);
    let workspace =
        crate::runtime::workspace::workspace_view_from_state(&state, repo_root.clone())?;
    if workspace.cwd() != repo_root.as_path() {
        return Ok(None);
    }
    // Explicit frozen local execution, no storage/State fallback during FS.
    let execution = EffectiveExecution {
        profile: state.execution_profile.clone(),
        workspace,
        scope: ExecutionScopeKind::AgentTurn,
        attached_workspaces: Vec::new(),
    };
    let child =
        prepare_task_stop_child_identity_tx(tx, original, true, request.byte_limit, ledger)?;
    guards.push(task.budget);
    Ok(Some(OriginalTaskStopWorktreeRead {
        execution,
        repo_root,
        metadata: metadata.clone(),
        guards,
        _child: child,
    }))
}
fn worktree_material(
    ledger: &InputEvidenceBudgetLedger,
    scope: &InputEvidenceScope,
    request: &TaskDomainLoadRequest,
    bytes: usize,
    records: usize,
) -> Result<Arc<BudgetViewGuard>> {
    let bytes = bytes
        .checked_mul(16)
        .context("worktree association material overflow")?;
    ensure!(
        bytes <= request.byte_limit.min(MAX_BYTES)
            && records
                .checked_mul(16)
                .is_some_and(|n| n <= request.record_limit.min(MAX_RECORDS)),
        crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
            "worktree association"
        )
    );
    Ok(ledger.register_owned_bytes(
        scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: u64::try_from(bytes)?,
            metadata_estimate_bytes: 0,
        },
    )?)
}
