//! Functional gates and isolated, paired attribution for live recovery.
//!
//! Mounted below prepared_live_oracle_tests.rs. Settled Result selection and
//! actual CallerAdmitted Result recovery are separate dependency rows.
use super::*;
use crate::runtime::resident_task::{ResidentTaskDomain, TaskAvailability, TaskFamily, TaskKey};
use crate::runtime_db::input_mutation_receipts::InputMutationReceipt;
use crate::runtime_db::task_result_settlement::{
    self, TaskResultSettlementDisposition, TaskResultSettlementRecord, TaskResultSettlementState,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dependency {
    Warm,
    Cold,
    Stale,
    NamedTask,
    NamedResult,
    CallerAdmittedResult,
}

impl Dependency {
    const ALL: [Self; 6] = [
        Self::Warm,
        Self::Cold,
        Self::Stale,
        Self::NamedTask,
        Self::NamedResult,
        Self::CallerAdmittedResult,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Warm => "warm_unrelated",
            Self::Cold => "cold",
            Self::Stale => "stale_after_accepted_task_write",
            Self::NamedTask => "existing_named_task",
            Self::NamedResult => "existing_settled_result",
            Self::CallerAdmittedResult => "caller_admitted_result_recovery_write",
        }
    }

    fn needs_fresh_base(self) -> bool {
        matches!(
            self,
            Self::Cold | Self::Stale | Self::NamedTask | Self::NamedResult
        )
    }
}

#[derive(Clone, Copy, Debug)]
struct Shape {
    selected: usize,
    history: usize,
}

fn matrix_request() -> tasks::TaskDomainLoadRequest {
    let mut request = task_request();
    // Vary terminal history without changing the selected-body denominator.
    request.capture.terminal_limit = 0;
    request
}

fn matrix_task(f: &RecoveryFixture, id: String, active: bool) -> TaskRecord {
    TaskRecord {
        id,
        agent_id: f.f.scope.agent_id.clone(),
        kind: crate::types::TaskKind::CommandTask,
        status: if active {
            crate::types::TaskStatus::Running
        } else {
            crate::types::TaskStatus::Completed
        },
        created_at: f.f.at,
        updated_at: f.f.at,
        parent_message_id: None,
        work_item_id: None,
        summary: Some("paired live-input matrix".into()),
        detail: Some(serde_json::json!({"body": "x".repeat(1024)})),
        recovery: None,
    }
}

fn seed_matrix(f: &RecoveryFixture, shape: Shape) -> Result<(Arc<ResidentTaskDomain>, TaskKey)> {
    let request = matrix_request();
    let mut records: Vec<_> = (0..shape.selected)
        .map(|index| matrix_task(f, format!("matrix:active:{index:04}"), true))
        .chain(
            (0..shape.history)
                .map(|index| matrix_task(f, format!("matrix:history:{index:04}"), false)),
        )
        .collect();
    let mut named = matrix_task(f, "matrix:named".into(), false);
    // A settled Result with a missing original owner is an existing supported
    // fixture shape; it grants no live recovery or admission eligibility.
    named.detail = Some(serde_json::json!({
        "rejoin_obligation_id": named.id,
        "rejoin_generation": 1,
        "parent_turn_id": "matrix:missing-result-owner"
    }));
    let mut source = f.f.queue()?.command.message_evidence.remove(0);
    source.id = "matrix:result-source".into();
    source.kind = MessageKind::TaskResult;
    source.origin = MessageOrigin::Task {
        task_id: named.id.clone(),
    };
    source.authority_class = AuthorityClass::RuntimeInstruction;
    source.task_id = Some(named.id.clone());
    source.metadata = Some(serde_json::json!({
        "task_id": named.id,
        "task_status": "completed"
    }));
    let source =
        f.f.db
            .messages()
            .append_with_index_changes_in_scope(&f.f.scope, &source, &[])?
            .record;
    let mut result = TaskResultSettlementRecord::pending(&named, &source, f.f.at)?
        .context("matrix Result requires an original rejoin fence")?;
    result.state = TaskResultSettlementState::Settled;
    result.disposition = Some(TaskResultSettlementDisposition::OwnerMissing);
    result.settled_at = Some(f.f.at);
    result.next_recheck_at = None;
    let result_key = TaskKey::new(TaskFamily::ResultSettlement, &result.result_identity);
    records.push(named);
    let mut keys: BTreeSet<_> = records
        .iter()
        .map(|record| TaskKey::new(TaskFamily::Task, &record.id))
        .collect();
    keys.insert(result_key.clone());
    let state = f.f.state()?;
    f.f.db.transaction_once(|tx| {
        let proof = tasks::prepare_task_write_tx(tx, &f.f.scope, &keys)?;
        for record in &records {
            upsert_task_tx(tx, record)?;
        }
        assert!(task_result_settlement::upsert_pending_tx(tx, &result)?);
        tasks::register_task_writes_tx(tx, &proof, &keys, &state, &request)?;
        Ok(())
    })?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let snapshot = f.f.db.transaction_once(|tx| {
        tasks::prepare_task_domain_snapshot_with_budget_tx(tx, &f.f.scope, &request, &ledger)?
            .require_current()
    })?;
    let resident = Arc::new(snapshot.resident);
    assert_eq!(resident.coverage().active_task_ids.len(), shape.selected);
    assert!(resident.coverage().unsettled_result_ids.is_empty());
    assert!(resident.coverage().terminal.keys.is_empty());
    assert_eq!(resident.selected_install_records().len(), shape.selected);
    assert!(!resident.coverage().observations.contains_key(&result_key));
    assert!(!resident
        .coverage()
        .observations
        .contains_key(&TaskKey::new(TaskFamily::Task, "matrix:named")));
    let counts: (i64, i64) = f.f.db.connection()?.query_row(
        "SELECT
           (SELECT COUNT(*) FROM tasks WHERE owner_agent_id=?1),
           (SELECT COUNT(*) FROM task_result_settlements WHERE agent_id=?1)",
        [&f.f.scope.agent_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(counts, ((shape.selected + shape.history + 1) as i64, 1));
    Ok((resident, result_key))
}

fn seed_caller_admitted_result(
    f: &mut RecoveryFixture,
) -> Result<(Arc<ResidentTaskDomain>, TaskResultSettlementRecord)> {
    let mut task = matrix_task(f, "matrix:caller-admitted".into(), false);
    task.parent_message_id = Some("matrix:caller-admitted-source".into());
    task.detail = Some(serde_json::json!({
        "rejoin_obligation_id": task.id,
        "rejoin_generation": 1,
        "parent_turn_id": f.predecessor.turn_id,
    }));
    let mut source = f.f.queue()?.command.message_evidence.remove(0);
    source.id = task.parent_message_id.clone().unwrap();
    source.kind = MessageKind::TaskResult;
    source.origin = MessageOrigin::Task {
        task_id: task.id.clone(),
    };
    source.authority_class = AuthorityClass::RuntimeInstruction;
    source.delivery_surface = Some(crate::types::MessageDeliverySurface::TaskRejoin);
    source.admission_context = Some(crate::types::AdmissionContext::RuntimeOwned);
    source.task_id = Some(task.id.clone());
    source.work_item_id = None;
    source.metadata = Some(serde_json::json!({
        "task_id": task.id,
        "task_kind": task.kind,
        "task_status": task.status,
        "task_detail": task.detail,
    }));
    let source =
        f.f.db
            .messages()
            .append_with_index_changes_in_scope(&f.f.scope, &source, &[])?
            .record;
    let mut result = TaskResultSettlementRecord::pending(&task, &source, f.f.at)?
        .context("CallerAdmitted matrix Result requires its same-parent source fence")?;
    result.state = TaskResultSettlementState::CallerAdmitted;
    result.activation_id = f.attempt.recovery_of_attempt_id.clone();
    result.admitted_at = Some(f.f.at);
    assert!(result.activation_id.is_some());
    assert!(result.disposition.is_none() && result.settled_at.is_none());
    f.predecessor.input_message_ids.push(source.id.clone());
    let batch = &mut f.predecessor.live_input_batches[0];
    batch.result_identities.push(result.result_identity.clone());
    batch.items.push(crate::types::IdleInputBatchItem {
        message_id: source.id.clone(),
        source_ref: format!("message:{}", source.id),
        rendered: "matrix same-parent Task Result".into(),
    });
    batch.rendered_bytes = batch.rendered_text().len();
    let keys = BTreeSet::from([
        TaskKey::new(TaskFamily::Task, &task.id),
        TaskKey::new(TaskFamily::ResultSettlement, &result.result_identity),
    ]);
    let state = f.f.state()?;
    f.f.db.transaction_once(|tx| {
        let proof = tasks::prepare_task_write_tx(tx, &f.f.scope, &keys)?;
        upsert_task_tx(tx, &task)?;
        assert!(task_result_settlement::upsert_pending_tx(tx, &result)?);
        upsert_queue_entry_tx(
            tx,
            &QueueEntryRecord {
                message_id: source.id.clone(),
                agent_id: f.f.scope.agent_id.clone(),
                priority: source.priority.clone(),
                status: QueueEntryStatus::Interjected,
                created_at: f.f.at,
                updated_at: f.f.at,
            },
        )?;
        upsert_turn_record_tx(tx, &f.predecessor)?;
        use crate::runtime_db::input_evidence_snapshot as input;
        input::register_committed_input_evidence_tx(
            tx,
            &f.f.scope,
            &[input::CanonicalInputEvidence::Turn(f.predecessor.clone())],
        )?;
        tasks::register_task_writes_tx(tx, &proof, &keys, &state, &matrix_request())?;
        Ok(())
    })?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let snapshot = f.f.db.transaction_once(|tx| {
        tasks::prepare_task_domain_snapshot_with_budget_tx(
            tx,
            &f.f.scope,
            &matrix_request(),
            &ledger,
        )?
        .require_current()
    })?;
    assert_eq!(
        snapshot.resident.coverage().unsettled_result_ids,
        BTreeSet::from([result.result_identity.clone()])
    );
    assert_eq!(
        f.f.db
            .task_result_settlements()
            .latest_for_message(&source.id)?,
        Some(result.clone())
    );
    Ok((Arc::new(snapshot.resident), result))
}

fn advance_task_writer(f: &RecoveryFixture) -> Result<()> {
    let record = matrix_task(f, "matrix:accepted-stale-writer".into(), false);
    let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &record.id)]);
    let state = f.f.state()?;
    f.f.db.transaction_once(|tx| {
        let proof = tasks::prepare_task_write_tx(tx, &f.f.scope, &keys)?;
        upsert_task_tx(tx, &record)?;
        tasks::register_task_writes_tx(tx, &proof, &keys, &state, &matrix_request())?;
        Ok(())
    })
}

type Stages = BTreeMap<String, crate::diagnostics::attribution::StageSnapshot>;

fn matrix_stages() -> Stages {
    attribution::snapshot()
        .into_iter()
        .map(|stage| (stage.name.clone(), stage))
        .collect()
}

struct MatrixRun {
    commit: TransitionCommit,
    receipt: InputMutationReceipt,
    installed: Option<ResidentTaskDomain>,
    elapsed_ns: u128,
    selected_records_before: usize,
    budget: serde_json::Value,
    before: Stages,
    after: Stages,
}

fn run_matrix_path(
    db: &RuntimeDb,
    f: &RecoveryFixture,
    prepared: &PreparedInputTransition,
    resident: &Arc<ResidentTaskDomain>,
    request: &tasks::TaskDomainLoadRequest,
    dependency: Dependency,
    full: bool,
) -> Result<MatrixRun> {
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let mut bases = PreparedTaskBaselines::default();
    if dependency != Dependency::Cold {
        bases.defer_roots(vec![resident.clone()]);
    }
    let before = matrix_stages();
    let started = Instant::now();
    let mut run = || {
        db.transitions_in_scope(&f.f.scope)
            .commit_prepared_transition_with_task_preparation(
                prepared,
                &work_request(),
                &ledger,
                request,
                &mut bases,
            )
    };
    let execution = crate::runtime_db::transitions::with_live_input_oracle(
        f.f.at + chrono::Duration::seconds(1),
        || {
            if full {
                live_task::with_full_task_oracle(run)
            } else {
                run()
            }
        },
    )?;
    let elapsed_ns = started.elapsed().as_nanos();
    let after = matrix_stages();
    let InputMutationExecution::Committed { value, receipt } = execution else {
        anyhow::bail!("matrix {} did not commit, full={full}", dependency.name())
    };
    assert!(value.applied);
    let fallback = full || dependency != Dependency::Warm;
    assert_eq!(value.effects.task_delta.is_some(), fallback);
    assert_eq!(bases.has_prepared_scope(&f.f.scope), fallback);
    if !fallback {
        assert!(Arc::ptr_eq(bases.resident(&f.f.scope).unwrap(), resident));
        assert!(value.effects.task_write_keys.is_empty());
        assert!(value.effects.task_observation_keys.is_empty());
    } else if dependency.needs_fresh_base() {
        assert!(value
            .effects
            .task_delta
            .as_ref()
            .unwrap()
            .prepared_base
            .is_some());
    }
    let installed = value
        .effects
        .task_delta
        .as_ref()
        .map(|delta| resident.with_committed(delta))
        .transpose()?;
    Ok(MatrixRun {
        commit: value,
        receipt,
        installed,
        elapsed_ns,
        selected_records_before: resident.selected_install_records().len(),
        budget: serde_json::to_value(ledger.report()?)?,
        before,
        after,
    })
}

fn installed<'a>(run: &'a MatrixRun, resident: &'a ResidentTaskDomain) -> &'a ResidentTaskDomain {
    run.installed.as_ref().unwrap_or(resident)
}

fn assert_recovered_result(
    db: &RuntimeDb,
    f: &RecoveryFixture,
    original: &TaskResultSettlementRecord,
    run: &MatrixRun,
    resident: &ResidentTaskDomain,
) -> Result<()> {
    let key = TaskKey::new(TaskFamily::ResultSettlement, &original.result_identity);
    assert!(run.commit.effects.task_write_keys.contains(&key));
    assert!(run.commit.effects.task_delta.is_some());
    let coverage = installed(run, resident).coverage();
    assert!(coverage
        .unsettled_result_ids
        .contains(&original.result_identity));
    assert_eq!(
        coverage.observations.get(&key),
        Some(&TaskAvailability::Present)
    );
    let mut expected = original.clone();
    expected.activation_id = Some(f.attempt.attempt_id.clone());
    expected.updated_at = f.f.at + chrono::Duration::seconds(1);
    assert_ne!(expected.activation_id, original.activation_id);
    assert!(expected.updated_at > original.updated_at);
    // Compare every canonical Result field, including all admission/recovery
    // metadata and updated_at; no timestamp or ACK-field normalization.
    assert_eq!(
        db.task_result_settlements()
            .latest_for_message(&original.message_id)?,
        Some(expected)
    );
    let turn = &run.commit.effects.turns[0];
    assert!(turn.input_message_ids.contains(&original.message_id));
    assert_eq!(
        turn.live_input_batches.len(),
        f.predecessor.live_input_batches.len()
    );
    let batch = &turn.live_input_batches[0];
    assert_eq!(batch.activation_id, f.attempt.attempt_id);
    assert!(batch.result_identities.contains(&original.result_identity));
    assert!(batch
        .items
        .iter()
        .any(|item| item.message_id == original.message_id));
    Ok(())
}

fn assert_task_equivalent(left: &ResidentTaskDomain, right: &ResidentTaskDomain) {
    assert_eq!(left.scope(), right.scope());
    assert_eq!(left.coverage(), right.coverage());
    assert_eq!(
        left.selected_install_records(),
        right.selected_install_records()
    );
    assert_eq!(
        left.selected_install_stop_records(),
        right.selected_install_stop_records()
    );
}

fn assert_matrix_database(
    db: &RuntimeDb,
    f: &RecoveryFixture,
    prepared: &PreparedInputTransition,
    request: &tasks::TaskDomainLoadRequest,
    run: &MatrixRun,
    resident: &ResidentTaskDomain,
) -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let oracle = db.transaction_once(|tx| {
        tasks::prepare_task_domain_snapshot_with_budget_tx(tx, &f.f.scope, request, &ledger)?
            .require_current()
    })?;
    assert_task_equivalent(installed(run, resident), &oracle.resident);
    assert_eq!(
        db.agent_states().latest(&f.f.scope.agent_id)?,
        Some(f.f.state()?)
    );
    for expected in &run.commit.effects.turns {
        assert_eq!(
            db.turn_records()
                .by_id(Some(&f.f.scope.agent_id), &expected.turn_id)?,
            Some(expected.clone())
        );
    }
    let receipts: i64 =
        db.connection()?
            .query_row("SELECT COUNT(*) FROM input_mutation_receipts", [], |row| {
                row.get(0)
            })?;
    assert_eq!(receipts, 1);
    assert!(matches!(
        db.transitions_in_scope(&f.f.scope).commit_prepared_transition(prepared)?,
        InputMutationExecution::Replayed { receipt, .. } if receipt == run.receipt
    ));
    let receipts: i64 =
        db.connection()?
            .query_row("SELECT COUNT(*) FROM input_mutation_receipts", [], |row| {
                row.get(0)
            })?;
    assert_eq!(receipts, 1, "replay created another receipt");
    Ok(())
}

fn stage_count(run: &MatrixRun, name: &str) -> u64 {
    run.after[name].count - run.before[name].count
}

fn assert_cost_counters(run: &MatrixRun, unchanged: bool) {
    if unchanged {
        for name in [
            "db.task.domain_prepare",
            "db.task.candidate.read",
            "db.task.candidate.validate",
            "db.task.candidate.apply",
            "db.task.write.delta_finalize",
        ] {
            assert_eq!(stage_count(run, name), 0, "retained path entered {name}");
        }
        assert!(stage_count(run, "db.task.live.unchanged") > 0);
    } else {
        assert!(stage_count(run, "db.task.write.delta_finalize") > 0);
    }
}

fn report_matrix_path(
    sample: usize,
    shape: Shape,
    dependency: Dependency,
    full: bool,
    full_first: bool,
    run: &MatrixRun,
) {
    let mut stages = BTreeMap::new();
    for (name, after) in &run.after {
        if name.starts_with("db.task.")
            || name.starts_with("db.input.")
            || name.starts_with("db.current.")
            || name.starts_with("db.reader.")
            || name.starts_with("db.connection.")
        {
            let before = &run.before[name];
            stages.insert(
                name,
                serde_json::json!({
                    "count": after.count - before.count,
                    "rows": after.rows - before.rows,
                    "total_ns": after.total_ns - before.total_ns,
                }),
            );
        }
    }
    // Stages are inclusive and overlap. Do not sum them or treat a reader
    // lease timer as an exclusive writer-transaction/held measurement.
    eprintln!(
        "live_cost_matrix {}",
        serde_json::json!({
            "sample": sample,
            "dependency": dependency.name(),
            "path": if full { "full_task_oracle" } else { "dependency_based" },
            "full_first": full_first,
            "selected_active_tasks": shape.selected,
            "selected_install_records_before": run.selected_records_before,
            "selected_install_records_after": run.installed.as_ref()
                .map(|domain| domain.selected_install_records().len())
                .unwrap_or(run.selected_records_before),
            "unselected_terminal_history": shape.history,
            "extra_named_terminal_tasks": 1,
            "extra_settled_results": 1,
            "extra_caller_admitted_terminal_tasks": usize::from(
                dependency == Dependency::CallerAdmittedResult
            ),
            "extra_caller_admitted_results": usize::from(
                dependency == Dependency::CallerAdmittedResult
            ),
            "accepted_stale_writer_tasks": usize::from(dependency == Dependency::Stale),
            "terminal_limit": 0,
            "stages": stages,
            "commit_api_end_to_end_ns": run.elapsed_ns,
            "budget": run.budget,
            "unavailable": [
                "live_preflight_total_ns",
                "business_sql_ns",
                "writer_queue_wait_ns",
                "writer_held_ns",
                "live_receipt_total_ns",
                "writer_transaction_total_ns",
                "runtime_activation_end_to_end_ns"
            ],
            "measurement_scope": "commit API including reprepare/retry; excludes fixture, VACUUM clones, freeze, ACK consumption and verification",
            "stage_semantics": "inclusive overlapping intervals; not additive"
        })
    );
}

fn paired_matrix(dependency: Dependency, shape: Shape, sample: usize, report: bool) -> Result<()> {
    let mut f = RecoveryFixture::new()?;
    let (mut resident, result_key) = seed_matrix(&f, shape)?;
    let recovered = if dependency == Dependency::CallerAdmittedResult {
        let (with_result, result) = seed_caller_admitted_result(&mut f)?;
        resident = with_result;
        Some(result)
    } else {
        None
    };
    let mut request = matrix_request();
    let required = match dependency {
        Dependency::NamedTask => Some(TaskKey::new(TaskFamily::Task, "matrix:named")),
        Dependency::NamedResult => Some(result_key),
        _ => None,
    };
    if let Some(key) = &required {
        request.capture.required_keys.insert(key.clone());
    }
    if dependency == Dependency::Stale {
        advance_task_writer(&f)?;
        let revision: u64 = f.f.db.connection()?.query_row(
            "SELECT revision FROM task_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",
            params![f.f.scope.agent_id, f.f.scope.identity_incarnation],
            |row| row.get(0),
        )?;
        assert!(revision > resident.coverage().revision);
    }
    let current = f.f.db.transaction_once(|tx| {
        tasks::unchanged::resident_is_current_tx(tx, &f.f.scope, &resident, &request)
    })?;
    assert_eq!(
        current,
        matches!(
            dependency,
            Dependency::Warm | Dependency::Cold | Dependency::CallerAdmittedResult
        )
    );
    let prepared = f.freeze(f.command(None))?;
    f.f.db.transaction_once(|tx| {
        assert_eq!(
            live_input::has_result_writes_tx(tx, &prepared)?,
            dependency == Dependency::CallerAdmittedResult
        );
        Ok(())
    })?;
    let fast_directory = tempfile::tempdir()?;
    let full_directory = tempfile::tempdir()?;
    // Neither execution changes the source fixture. Both start from VACUUM
    // clones made before either execution, with exactly one frozen command.
    let fast_db = fork_db(&f.f.db, fast_directory.path())?;
    let full_db = fork_db(&f.f.db, full_directory.path())?;
    let full_first = sample % 2 == 1;
    let execute = |db: &RuntimeDb, full| {
        run_matrix_path(db, &f, &prepared, &resident, &request, dependency, full)
    };
    let (fast, full) = if full_first {
        let full = execute(&full_db, true)?;
        (execute(&fast_db, false)?, full)
    } else {
        let fast = execute(&fast_db, false)?;
        (fast, execute(&full_db, true)?)
    };
    compare_acks(&fast.commit, &full.commit)?;
    assert_eq!(fast.receipt.prepared, full.receipt.prepared);
    assert_eq!(fast.receipt.outcome, full.receipt.outcome);
    assert_task_equivalent(installed(&fast, &resident), installed(&full, &resident));
    if let Some(original) = &recovered {
        assert_recovered_result(&fast_db, &f, original, &fast, &resident)?;
        assert_recovered_result(&full_db, &f, original, &full, &resident)?;
        assert_eq!(
            f.f.db
                .task_result_settlements()
                .latest_for_message(&original.message_id)?,
            Some(original.clone()),
            "paired recovery mutated the original CallerAdmitted fixture"
        );
    }
    if let Some(key) = &required {
        for run in [&fast, &full] {
            let coverage = installed(run, &resident).coverage();
            assert!(coverage.required_keys.contains(key));
            assert_eq!(
                coverage.observations.get(key),
                Some(&TaskAvailability::Present)
            );
        }
    }
    assert_matrix_database(&fast_db, &f, &prepared, &request, &fast, &resident)?;
    assert_matrix_database(&full_db, &f, &prepared, &request, &full, &resident)?;
    assert_eq!(f.f.receipt_count()?, 0, "oracle mutated the source fixture");
    if report {
        // These are process-global deltas; this ignored test must run alone.
        assert_cost_counters(&fast, dependency == Dependency::Warm);
        assert_cost_counters(&full, false);
        // A warm CallerAdmitted write may adopt its complete installed base;
        // it needs a Task ACK, not an artificial candidate rebuild.
        if dependency.needs_fresh_base() {
            for run in [&fast, &full] {
                assert!(stage_count(run, "db.task.candidate.read") > 0);
                assert!(stage_count(run, "db.task.candidate.validate") > 0);
            }
        }
        report_matrix_path(sample, shape, dependency, false, full_first, &fast);
        report_matrix_path(sample, shape, dependency, true, full_first, &full);
    }
    Ok(())
}

#[test]
fn prepared_live_input_dependency_functional_matrix_matches_full_task_oracle() -> Result<()> {
    for dependency in Dependency::ALL {
        paired_matrix(
            dependency,
            Shape {
                selected: 2,
                history: 3,
            },
            0,
            false,
        )?;
    }
    Ok(())
}

#[test]
#[ignore = "process-global paired attribution; run this test alone with --ignored --nocapture --test-threads=1"]
fn prepared_live_input_dependency_paired_cost_matrix() -> Result<()> {
    // Factorial axes: 1/32 selected bodies and 0/256 unselected terminal rows.
    // No time threshold: ACK, receipt, selection and stage counters are gates.
    for selected in [1, 32] {
        for history in [0, 256] {
            for dependency in Dependency::ALL {
                for sample in 0..5 {
                    paired_matrix(dependency, Shape { selected, history }, sample, true)?;
                }
            }
        }
    }
    Ok(())
}
