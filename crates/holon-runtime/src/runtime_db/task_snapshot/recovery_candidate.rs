//! Receipt/current recovery adds finite cross-domain observations to the normal
//! Task candidate. The original writer never rebuilds the Task/Stop domain.
use super::*;
use crate::runtime_db::episode_snapshot::task_checkpoint::TaskContinuityCheckpoint;

pub(in crate::runtime_db) struct TaskRecoveryQualification {
    fixed: Vec<TaskContinuityCheckpoint>,
    source_complete: bool,
    _guards: Vec<Arc<BudgetViewGuard>>,
}

pub(in crate::runtime_db) fn prepare_recovery_task_qualification_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    candidate: &TaskPreparationCandidate,
    request: &TaskDomainLoadRequest,
    budget: &InputEvidenceBudgetLedger,
) -> Result<TaskRecoveryQualification> {
    // Receipt install IDs and live queries never assert scheduler recovery
    // sources. Such a request still requires the scheduler's original frozen
    // recovery time and its Wait/replay graph, not receipt-current authority.
    ensure!(
        request.capture.recovery_source_ids.is_empty(),
        "Task receipt recovery reader gate: scheduler recovery sources require frozen Wait/replay qualification"
    );
    let mut material = read_preparation::TaskReadMaterial::new(scope, request, Some(budget));
    material.charge_checkpoint_metadata(tx, candidate.binding.as_deref())?;
    let fixed = qualification::manifests(tx, candidate.binding.as_deref())?;
    Ok(TaskRecoveryQualification {
        fixed,
        source_complete: query_sources::source_inventory_complete_tx(tx, scope)?,
        _guards: material.into_guards(),
    })
}

/// Heads, finite query relations and the live pending frontier may change
/// outside the Task writer. Compare metadata, retaining reader-owned bodies.
pub(in crate::runtime_db) fn commit_recovery_task_candidate_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    candidate: TaskPreparationCandidate,
    qualification: TaskRecoveryQualification,
    request: &TaskDomainLoadRequest,
    budget: &InputEvidenceBudgetLedger,
) -> Result<TaskCurrentSnapshot> {
    let mut material = read_preparation::TaskReadMaterial::new(scope, request, Some(budget));
    let selected = &candidate.qualification.records;
    let coverage = candidate.snapshot.resident.coverage();
    material.charge_ids(
        selected
            .len()
            .checked_mul(std::mem::size_of::<&TaskFactLocator>())
            .context("Task recovery locator slots overflow")?,
        selected.len(),
    )?;
    let tasks = selected
        .iter()
        .filter(|record| record.locator.key.family == TaskFamily::Task)
        .map(|record| &record.locator)
        .collect::<Vec<_>>();
    ensure!(
        query_sources::source_inventory_complete_tx(tx, scope)? == qualification.source_complete,
        TaskPreparationCandidateStale("recovery output inventory")
    );
    if qualification.source_complete {
        let output = query_sources::observe_output_heads_tx(tx, scope, &tasks, |bytes, count| {
            material.charge_ids(bytes, count)
        })?;
        ensure!(
            output == coverage.output,
            TaskPreparationCandidateStale("recovery output heads")
        );
    } else {
        ensure!(
            coverage.output == TaskOutputCoverage::default(),
            TaskPreparationCandidateStale("recovery unavailable historical output")
        );
    }
    let status = status_observation::observe_tx(tx, scope, selected, |bytes, count| {
        material.charge_ids(bytes, count)
    })?;
    ensure!(
        status == coverage.status,
        TaskPreparationCandidateStale("recovery status heads")
    );
    let running = running_sources::observe_tx(
        tx,
        scope,
        candidate.binding.as_deref(),
        &qualification.fixed,
        |bytes, count| material.charge_ids(bytes, count),
    )?;
    ensure!(
        running == coverage.running,
        TaskPreparationCandidateStale("recovery legacy Running sources")
    );
    let query = query_relations::observe_tx(tx, scope, &request.query, &mut material)?;
    ensure!(
        query == coverage.query,
        TaskPreparationCandidateStale("recovery query relations")
    );
    validate_stops_tx(tx, scope, selected, &coverage.stop, &mut material)?;
    commit_task_candidate_snapshot_tx(tx, scope, candidate)
}

const PENDING: &str = "(physical_state='pending' OR child_state='pending' OR worktree_state='pending' OR result_state='pending')";
// The older state-tuple index is covering, so SQLite can prefer its historical
// owner scan for COUNT even when a partial frontier index is available.
const PENDING_FRONTIER: &str = "FROM task_stop_effects INDEXED BY idx_task_stop_pending_frontier WHERE agent_id=?1 AND identity_incarnation=?2 AND (physical_state='pending' OR child_state='pending' OR worktree_state='pending' OR result_state='pending')";

fn validate_stops_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    selected: &[Arc<QualifiedTaskFact>],
    expected: &TaskStopCoverage,
    material: &mut read_preparation::TaskReadMaterial<'_>,
) -> Result<()> {
    // The partial pending-frontier index excludes completed historical Stops.
    let count: usize = tx.query_row(
        &format!("SELECT COUNT(*) {PENDING_FRONTIER}"),
        params![scope.agent_id, scope.identity_incarnation],
        |row| row.get(0),
    )?;
    ensure!(
        expected.pending_complete && count == expected.pending_effect_seqs.len(),
        TaskPreparationCandidateStale("recovery pending Stop frontier")
    );
    material.charge_ids(
        count
            .checked_mul(64)
            .context("Task recovery Stop frontier overflow")?,
        count,
    )?;
    let mut statement = tx.prepare_cached(&format!(
        "SELECT effect_seq {PENDING_FRONTIER} ORDER BY effect_seq"
    ))?;
    let mut rows = statement.query(params![scope.agent_id, scope.identity_incarnation])?;
    for seq in &expected.pending_effect_seqs {
        ensure!(
            rows.next()?.map(|row| row.get::<_, u64>(0)).transpose()? == Some(*seq),
            TaskPreparationCandidateStale("recovery pending Stop frontier")
        );
    }
    ensure!(
        rows.next()?.is_none(),
        TaskPreparationCandidateStale("recovery pending Stop frontier")
    );
    for record in selected {
        let CanonicalTaskFact::Task(task) = &record.fact else {
            continue;
        };
        let head = expected
            .heads
            .get(&task.id)
            .context("Task recovery Stop head missing")?;
        let current: Option<u64> = tx.query_row(
            "SELECT MAX(effect_seq) FROM task_stop_effects WHERE agent_id=?1 AND identity_incarnation=?2 AND task_id=?3 AND task_created_at=?4 AND generation_digest=?5",
            params![scope.agent_id, scope.identity_incarnation, task.id, super::super::super::migrations::timestamp(task.created_at), head.generation_digest],
            |row| row.get(0),
        )?;
        ensure!(
            current == head.head.as_ref().map(|head| head.effect_seq),
            TaskPreparationCandidateStale("recovery Stop generation head")
        );
    }
    for (seq, record) in &expected.records {
        // Stop revision is an incrementing generation, unlike Task timestamps.
        // Digests bind the immutable intent; no Stop JSON is read or decoded.
        let exact: bool = tx.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM task_stop_effects WHERE effect_seq=?1 AND agent_id=?2 AND identity_incarnation=?3 AND revision=?4 AND intent_digest=?5 AND generation_digest=?6 AND {PENDING}=?7)"),
            params![seq, scope.agent_id, scope.identity_incarnation, record.locator.revision, record.locator.intent_digest, record.locator.generation_digest, record.pending],
            |row| row.get(0),
        )?;
        ensure!(
            exact,
            TaskPreparationCandidateStale("recovery Stop revision")
        );
    }
    Ok(())
}

#[cfg(all(test, holon_lib_test_shard = "b"))]
#[path = "recovery_candidate_tests.rs"]
mod tests;
