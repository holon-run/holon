//! A named reader candidate carries observations, never admission authority.
use super::*;
use crate::runtime::input_evidence_budget::{BudgetViewGuard, InputEvidenceBudgetLedger};

#[derive(Debug, thiserror::Error)]
#[error("Task preparation dependency changed: {0}")]
pub(crate) struct TaskPreparationCandidateStale(pub(crate) &'static str);

/// A selected row retains its family-specific canonical observation. The
/// original writer checks it without decoding the body; the recorded digests
/// must still match alias or eligibility.
pub(super) struct CandidateRecordDependency {
    pub(super) key: TaskKey,
    pub(super) canonical_observation: CanonicalRecordObservation,
    pub(super) content_digest: String,
    pub(super) projection_digest: String,
}

/// A negative observation is re-observed by exact indexed lookups only. When
/// the reader saw an alias it must still be byte-identical; otherwise the key
/// must still be unknown to canonical, alias, eligibility and fixed inventory.
pub(super) struct CandidateAbsenceDependency {
    pub(super) key: TaskKey,
    pub(super) alias: Option<Alias>,
}

/// Invalid Result targets need not be selected Task members. Their exact
/// predicate authority must still survive the reader-to-writer handoff.
#[derive(PartialEq, Eq)]
struct CandidateResultTargetDependency {
    key: TaskKey,
    predicate_digest: Option<String>,
    alias: Option<Alias>,
    eligibility: Option<(String, String)>,
    unavailable: bool,
    known: bool,
}

fn result_target_dependency_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    seed: Option<&str>,
    key: TaskKey,
    material: Option<&mut read_preparation::TaskReadMaterial<'_>>,
    fixed: &qualification::CandidateFixedQualification,
) -> Result<CandidateResultTargetDependency> {
    // Task revisions are millisecond timestamps, not unique generations.
    // Re-observe only the exact predicate projection, never the Task body.
    let predicate = result_relation::TASK_PREDICATE_SQL;
    let bytes: Option<usize> = tx
        .query_row(
            &format!("SELECT length(CAST({predicate} AS BLOB)) FROM tasks WHERE task_id=?1"),
            [&key.id],
            |row| row.get(0),
        )
        .optional()?;
    let metadata_bytes = bytes
        .unwrap_or_default()
        .checked_add(key.id.len())
        .and_then(|bytes| bytes.checked_add(256))
        .context("Task target predicate bytes overflow")?;
    observe_material(1, metadata_bytes, None)?;
    if let Some(material) = material {
        material.charge_relation(metadata_bytes)?;
    }
    let raw: Option<String> = tx
        .query_row(
            &format!("SELECT {predicate} FROM tasks WHERE task_id=?1"),
            [&key.id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(CandidateResultTargetDependency {
        predicate_digest: raw.map(|raw| digest(raw.as_bytes())),
        alias: alias(tx, scope, &key)?,
        eligibility: fixed.eligible_digest(tx, seed, &key)?,
        unavailable: tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM task_domain_eligibility WHERE seed_id=?1 AND family='task' AND record_id=?2 AND availability='unavailable')",
            params![seed, key.id],
            |row| row.get(0),
        )?,
        known: qualification::known_anywhere(tx, &key)?,
        key,
    })
}

#[derive(Default)]
pub(super) struct CandidateQualification {
    pub(super) fixed: qualification::CandidateFixedQualification,
    pub(super) aliases: Vec<(TaskKey, Alias)>,
    pub(super) records: Vec<Arc<QualifiedTaskFact>>,
    pub(super) present: Vec<CandidateRecordDependency>,
    pub(super) absent: Vec<CandidateAbsenceDependency>,
}

pub(crate) struct TaskPreparationCandidate {
    snapshot: TaskCurrentSnapshot,
    qualification: CandidateQualification,
    coverage_json: String,
    base_revision: u64,
    base_coverage_json: String,
    binding: Option<String>,
    invalid_targets: Vec<CandidateResultTargetDependency>,
    _guards: Vec<Arc<BudgetViewGuard>>,
}

pub(in crate::runtime_db) fn qualified_recovery_selectors_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    keys: &BTreeSet<TaskKey>,
    candidate: &TaskPreparationCandidate,
) -> Result<BTreeSet<TaskKey>> {
    candidate
        .qualification
        .fixed
        .validate(tx, candidate.binding.as_deref())?;
    read_preparation::qualified_task_recovery_selectors_with_fixed_tx(
        tx,
        scope,
        keys,
        Some(&candidate.qualification.fixed),
    )
}

/// Failed dependency checks and load errors never open this initialization gate.
pub(crate) fn task_preparation_initialization_required_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
) -> Result<bool> {
    let initialized: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM task_domain_current WHERE agent_id=?1 AND identity_incarnation=?2)",
        params![scope.agent_id, scope.identity_incarnation],
        |row| row.get(0),
    )?;
    Ok(!initialized || !query_sources::source_inventory_initialized_tx(tx, scope)?)
}

/// Bootstrap initializes the domain before Ready publication. No inventory is
/// created, repaired or inferred by a named preparation reader or its writer.
pub(crate) fn prepare_task_candidate_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    expected_state: &AgentState,
    request: &TaskDomainLoadRequest,
    budget: &InputEvidenceBudgetLedger,
) -> Result<Option<TaskPreparationCandidate>> {
    let mut read_stage = crate::diagnostics::attribution::TASK_CANDIDATE_READ.start();
    if task_preparation_initialization_required_tx(tx, scope)? {
        return Ok(None);
    }
    let facts = ContextDurableRead::new(tx, &scope.agent_id).scope_facts()?;
    ensure!(
        facts.state.as_deref() == Some(expected_state) && facts.control_revision.is_some(),
        "Task preparation canonical State differs from published root"
    );
    let identity = facts
        .identity
        .as_ref()
        .context("Task preparation identity missing")?;
    ensure!(
        identity.agent_id == scope.agent_id && identity.incarnation == scope.identity_incarnation,
        "Task preparation incarnation differs"
    );
    let mut base_material = read_preparation::TaskReadMaterial::new(scope, request, Some(budget));
    let base_coverage_json = {
        let mut statement = tx.prepare_cached(
            "SELECT coverage_json FROM task_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",
        )?;
        let mut rows = statement.query(params![scope.agent_id, scope.identity_incarnation])?;
        let row = rows
            .next()?
            .context("Task preparation current coverage missing")?;
        let json = repositories::borrowed_payload(row)?;
        base_material.charge_ids(json.len(), 1)?;
        json.to_owned()
    };
    let mut qualification = CandidateQualification::default();
    let (snapshot, mut guards) = loader::load_preparation_candidate_tx(
        tx,
        scope,
        request,
        identity.status == AgentRegistryStatus::Deleting,
        budget,
        &mut qualification,
    )?;
    ensure!(
        snapshot.facts.identity == facts.identity
            && snapshot.facts.state == facts.state
            && snapshot.facts.control_revision == facts.control_revision,
        "Task preparation domains differ from the same read view"
    );
    let coverage_json = serde_json::to_string(snapshot.resident.coverage())?;
    let base_revision = snapshot.resident.coverage().revision;
    let binding = binding(tx, scope)?;
    let mut invalid_targets = Vec::new();
    let mut target_keys = BTreeSet::new();
    for observation in snapshot.resident.coverage().result_dependencies.values() {
        if let TaskResultDependencyRelation::Invalid { target, .. } = &observation.relation {
            let task_id = match target {
                TaskDependencyTarget::GlobalAbsent { task_id }
                | TaskDependencyTarget::Pruned { task_id }
                | TaskDependencyTarget::FixedUnavailable { task_id }
                | TaskDependencyTarget::CanonicalObserved { task_id, .. } => task_id,
            };
            if target_keys.insert(task_id.as_str()) {
                // The key, tuple and four validated digest strings are bounded
                // metadata; no Task body is retained or re-read by the writer.
                base_material.charge_ids(
                    task_id
                        .len()
                        .checked_add(512)
                        .context("Task target bytes overflow")?,
                    1,
                )?;
                invalid_targets.push(result_target_dependency_tx(
                    tx,
                    scope,
                    binding.as_deref(),
                    TaskKey::new(TaskFamily::Task, task_id),
                    Some(&mut base_material),
                    &qualification.fixed,
                )?);
            }
        }
    }
    guards.extend(base_material.into_guards());
    read_stage.rows(qualification.records.len());
    Ok(Some(TaskPreparationCandidate {
        snapshot,
        qualification,
        coverage_json,
        base_revision,
        base_coverage_json,
        binding,
        invalid_targets,
        _guards: guards,
    }))
}

/// The original writer re-observes the semantic dependencies of the candidate
/// before saving qualification. It never re-runs a loader or recovery selector,
/// never decodes a Task body and never rebuilds Resident. Every check is an
/// exact-key lookup: scope facts, lane revision, continuity binding, selected
/// canonical revisions/lifecycle metadata with alias/eligibility digests, the
/// exact previous coverage bytes, selected child delegation heads, and indexed
/// negative observations. A changed dependency is
/// reported as stale so the named caller
/// re-reads on the preparation pool instead of failing the operation.
pub(crate) fn commit_task_candidate_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    candidate: TaskPreparationCandidate,
) -> Result<ResidentTaskDomain> {
    commit_task_candidate_snapshot_tx(tx, scope, candidate).map(|snapshot| snapshot.resident)
}

#[path = "recovery_candidate.rs"]
mod recovery;
pub(in crate::runtime_db) use recovery::{
    commit_recovery_task_candidate_tx, prepare_recovery_task_qualification_tx,
    TaskRecoveryQualification,
};

fn commit_task_candidate_snapshot_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    candidate: TaskPreparationCandidate,
) -> Result<TaskCurrentSnapshot> {
    let mut validate_stage = crate::diagnostics::attribution::TASK_CANDIDATE_VALIDATE.start();
    ensure!(
        candidate.snapshot.resident.scope() == scope,
        "Task preparation candidate scope differs"
    );
    let facts = ContextDurableRead::new(tx, &scope.agent_id).scope_facts()?;
    let expected = &candidate.snapshot.facts;
    ensure!(
        facts.control_revision.is_some()
            && facts.identity == expected.identity
            && facts.state == expected.state
            && facts.control_revision == expected.control_revision,
        TaskPreparationCandidateStale("scope facts")
    );
    ensure!(
        loader::current_revision(tx, scope)? == Some(candidate.base_revision),
        TaskPreparationCandidateStale("base revision")
    );
    let same_coverage: bool = tx.query_row(
        "SELECT coverage_json=?3 FROM task_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",
        params![scope.agent_id, scope.identity_incarnation, candidate.base_coverage_json],
        |row| row.get(0),
    )?;
    ensure!(
        same_coverage,
        TaskPreparationCandidateStale("base coverage")
    );
    let seed = binding(tx, scope)?;
    ensure!(
        seed == candidate.binding,
        TaskPreparationCandidateStale("continuity binding")
    );
    candidate
        .qualification
        .fixed
        .validate(tx, seed.as_deref())?;
    for dependency in &candidate.qualification.present {
        ensure!(
            canonical_observation_matches_on(
                tx,
                &dependency.key,
                &dependency.canonical_observation,
            )?,
            TaskPreparationCandidateStale("selected canonical row")
        );
        match alias(tx, scope, &dependency.key)? {
            Some(current) => ensure!(
                current.availability == TaskAvailability::Present
                    && current.digest.as_deref() == Some(dependency.content_digest.as_str())
                    && current.projection_digest.as_deref()
                        == Some(dependency.projection_digest.as_str()),
                TaskPreparationCandidateStale("selected alias")
            ),
            None => ensure!(
                candidate.qualification.fixed.eligible_digest(
                    tx,
                    seed.as_deref(),
                    &dependency.key
                )? == Some((
                    dependency.content_digest.clone(),
                    dependency.projection_digest.clone(),
                )),
                TaskPreparationCandidateStale("selected eligibility")
            ),
        }
    }
    for dependency in &candidate.qualification.absent {
        let (table, id, _) = family_table(dependency.key.family);
        let exists: bool = tx.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE {id}=?1)"),
            [&dependency.key.id],
            |row| row.get(0),
        )?;
        ensure!(
            !exists,
            TaskPreparationCandidateStale("negative canonical row")
        );
        match &dependency.alias {
            Some(value) => ensure!(
                alias(tx, scope, &dependency.key)?.as_ref() == Some(value),
                TaskPreparationCandidateStale("negative alias")
            ),
            None => ensure!(
                alias(tx, scope, &dependency.key)?.is_none()
                    && !qualification::known_anywhere(tx, &dependency.key)?
                    && candidate
                        .qualification
                        .fixed
                        .eligible_digest(tx, seed.as_deref(), &dependency.key)?
                        .is_none(),
                TaskPreparationCandidateStale("negative registration")
            ),
        }
    }
    for dependency in &candidate.invalid_targets {
        ensure!(
            result_target_dependency_tx(
                tx,
                scope,
                seed.as_deref(),
                dependency.key.clone(),
                None,
                &candidate.qualification.fixed
            )? == *dependency,
            TaskPreparationCandidateStale("Result target predicate")
        );
    }
    ensure!(
        status_observation::delegation_heads_match_tx(
            tx,
            &candidate.qualification.records,
            &candidate.snapshot.resident.coverage().status,
        )?,
        TaskPreparationCandidateStale("TaskStatus delegation head")
    );
    validate_stage.rows(candidate.qualification.records.len());
    drop(validate_stage);
    let mut apply_stage = crate::diagnostics::attribution::TASK_CANDIDATE_APPLY.start();
    apply_stage.rows(candidate.qualification.aliases.len());
    for (key, value) in &candidate.qualification.aliases {
        upsert_alias(tx, scope, key, value)?;
    }
    sync_child_observers_tx(tx, scope, &candidate.qualification.records)?;
    let coverage = candidate.snapshot.resident.coverage();
    tx.execute(
        "INSERT INTO task_domain_current(agent_id,identity_incarnation,revision,coverage_json) VALUES(?1,?2,?3,?4) ON CONFLICT(agent_id,identity_incarnation) DO UPDATE SET revision=excluded.revision,coverage_json=excluded.coverage_json",
        params![scope.agent_id, scope.identity_incarnation, coverage.revision, candidate.coverage_json],
    )?;
    Ok(candidate.snapshot)
}
