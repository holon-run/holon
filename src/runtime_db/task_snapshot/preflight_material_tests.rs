use super::*;
use crate::runtime_db::task_snapshot::tests::{fresh, request, scope, task};
use crate::runtime_db::task_snapshot::{loader::current_coverage, prepare_task_write_tx};
use crate::types::TaskStatus;

fn actual_task(tx: &Transaction<'_>) -> Result<TaskKey> {
    let value = task("actual-preflight-task", TaskStatus::Running);
    let key = TaskKey::new(TaskFamily::Task, &value.id);
    let keys = BTreeSet::from([key.clone()]);
    let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
    repositories::upsert_task_tx(tx, &value)?;
    writes::register_task_writes_tx(
        tx,
        &proof,
        &keys,
        &AgentState::new("task-agent"),
        &request(),
    )?;
    Ok(key)
}

#[test]
fn discarded_original_task_preflight_releases_only_its_own_actual_material() -> Result<()> {
    let (_dir, db) = fresh()?;
    let key = db.transaction(actual_task)?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    db.transaction(|tx| {
        let scope = scope();
        let mut policy = request();
        policy.byte_limit = 64 * 1024 * 1024;
        policy.record_limit = 65_536;
        let keys = BTreeSet::from([key.clone()]);
        let mut phase = TaskReadMaterial::new(&scope, &policy, Some(&ledger));
        phase.charge_writer_material(1024, 4, 1)?;
        let proof = writes::prepare_original_task_write_using_material_tx(
            tx, &scope, &keys, false, &policy, &ledger, &mut phase,
        )?;
        let usage = phase.material_estimate();
        let bytes = ledger.report()?.observed_bytes;
        writes::preflight_original_task_using_material_tx(
            tx, &proof, &policy, &ledger, &mut phase,
        )?;
        let after = phase.material_estimate();
        assert_eq!((after.bytes, after.records), (usage.bytes, usage.records));
        assert_eq!(
            ledger.report()?.observed_bytes,
            bytes,
            "discarded preflight cannot retain a second full window"
        );
        assert_eq!(
            proof.task_before(&key.id)?.id,
            key.id,
            "the original before remains held"
        );
        let guards = phase.into_guards();
        let last = guards.clone();
        drop(guards);
        assert_eq!(ledger.report()?.observed_bytes, bytes);
        drop((proof, last));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    })
}

#[test]
fn original_task_preflight_checks_its_actual_peak_before_business_sql() -> Result<()> {
    let (_dir, db) = fresh()?;
    let key = db.transaction(actual_task)?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let (peak, before) = db.transaction(|tx| {
        let scope = scope();
        let mut policy = request();
        policy.byte_limit = 64 * 1024 * 1024;
        policy.record_limit = 65_536;
        let keys = BTreeSet::from([key.clone()]);
        let mut phase = TaskReadMaterial::new(&scope, &policy, Some(&ledger));
        phase.charge_writer_material(1024, 4, 1)?;
        let proof = writes::prepare_original_task_write_using_material_tx(
            tx, &scope, &keys, false, &policy, &ledger, &mut phase,
        )?;
        phase.read_current_metadata(tx)?;
        let snapshot = loader::load_selected_using_material(
            tx,
            &scope,
            &policy,
            false,
            false,
            true,
            Some(&ledger),
            None,
            None,
            &mut phase,
        )?
        .require_current()?;
        let peak = phase.material_estimate().bytes;
        assert_eq!(
            snapshot.resident.coverage().active_task_ids,
            BTreeSet::from([key.id.clone()])
        );
        let before = current_coverage(tx, &scope)?;
        drop((snapshot, proof, phase));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok((peak, before))
    })?;
    let error = db
        .transaction(|tx| {
            let scope = scope();
            let mut policy = request();
            policy.byte_limit = peak - 1;
            policy.record_limit = 65_536;
            let keys = BTreeSet::from([key.clone()]);
            let mut phase = TaskReadMaterial::new(&scope, &policy, Some(&ledger));
            phase.charge_writer_material(1024, 4, 1)?;
            let proof = writes::prepare_original_task_write_using_material_tx(
                tx, &scope, &keys, false, &policy, &ledger, &mut phase,
            )?;
            writes::preflight_original_task_using_material_tx(
                tx, &proof, &policy, &ledger, &mut phase,
            )
        })
        .unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<TaskSnapshotError>(),
            Some(TaskSnapshotError::ResourceExhausted("bytes"))
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert_eq!(
        db.transaction(|tx| Ok(current_coverage(tx, &scope())?))?,
        before
    );
    db.transaction(|tx| {
        assert_eq!(
            stored_on(tx, &scope().agent_id, &key)?
                .context("original Task")?
                .fact,
            CanonicalTaskFact::Task(task(&key.id, TaskStatus::Running))
        );
        Ok(())
    })
}

#[test]
fn task_preflight_marks_cannot_release_another_phase_or_a_live_guard_holder() -> Result<()> {
    let scope = scope();
    let mut policy = request();
    policy.byte_limit = 64 * 1024 * 1024;
    policy.record_limit = 65_536;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let mut phase = TaskReadMaterial::new(&scope, &policy, Some(&ledger));
    phase.charge_writer_material(1024, 4, 1)?;
    let foreign = phase.mark_preflight()?;
    let held_mark = phase.mark_preflight()?;
    let retry = phase.mark_preflight()?;
    let original = phase.material_estimate();
    let original_bytes = ledger.report()?.observed_bytes;
    phase.charge_writer_material(2048, 8, 1)?;
    let mut other = TaskReadMaterial::new(&scope, &policy, Some(&ledger));
    other.charge_writer_material(128, 1, 1)?;
    let error = other.retire_preflight(foreign).unwrap_err();
    assert!(error.to_string().contains("another phase"));
    drop(other);
    let usage = phase.material_estimate();
    let bytes = ledger.report()?.observed_bytes;
    let held = phase
        .guards
        .last()
        .context("actual preflight guard")?
        .clone();
    let error = phase.retire_preflight(held_mark).unwrap_err();
    assert!(error.to_string().contains("another live holder"));
    assert_eq!(ledger.report()?.observed_bytes, bytes);
    assert_eq!((phase.bytes, phase.records), (usage.bytes, usage.records));
    drop(held);
    phase.retire_preflight(retry)?;
    assert_eq!(
        (phase.bytes, phase.records),
        (original.bytes, original.records)
    );
    assert_eq!(ledger.report()?.observed_bytes, original_bytes);
    let guards = phase.into_guards();
    let last = guards.clone();
    drop(guards);
    assert_eq!(ledger.report()?.observed_bytes, original_bytes);
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn ordinary_task_actual_phase_survives_whole_ack_clones_and_fault_rolls_back() -> Result<()> {
    use crate::runtime_db::transitions::{
        ExecutionProtocolTransition, InputMutationExecution, PreparedInputTransition,
        PreparedTaskTransition, TaskTransitionCommand, TransitionFaultPoint,
    };
    let (_dir, db) = fresh()?;
    let scope = scope();
    let value = task("actual-ordinary-task-phase", TaskStatus::Running);
    let command = |fault| TaskTransitionCommand {
        agent_id: scope.agent_id.clone(),
        task: value.clone(),
        agent_deletion: None,
        task_result_settlement: None,
        queue_entry: None,
        work_items: Vec::new(),
        expected_wait_conditions: Vec::new(),
        wait_conditions: Vec::new(),
        agent_state: None,
        message_evidence: Vec::new(),
        audit_events: Vec::new(),
        index_changes: Vec::new(),
        notify_scheduler: false,
        commit_on_idempotent: false,
        fault,
    };
    let prepare = |fault| {
        PreparedInputTransition::task(
            scope.clone(),
            "ordinary-task-phase-whole-ack".into(),
            value.created_at,
            PreparedTaskTransition::new(
                command(fault),
                ExecutionProtocolTransition::default(),
                false,
                None,
                None,
            )?,
        )
    };
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let policy = TaskDomainLoadRequest {
        query: Default::default(),
        capture: TaskCaptureRequest::current(),
        byte_limit: 64 * 1024 * 1024,
        record_limit: 65_536,
    };
    let work = crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
        capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
        byte_limit: 64 * 1024 * 1024,
        record_limit: 65_536,
    };
    let before = db.transaction(|tx| Ok(current_coverage(tx, &scope)?))?;
    let error = db
        .transitions_in_scope(&scope)
        .commit_prepared_transition_with_work_request_and_memory_budget(
            &prepare(Some(TransitionFaultPoint::BeforeCommit))?,
            &work,
            &ledger,
            &policy,
        )
        .err()
        .context("actual fault was omitted")?;
    assert!(format!("{error:#}").contains("BeforeCommit"), "{error:#}");
    assert_eq!(
        db.connection()?.query_row(
            "SELECT COUNT(*) FROM tasks WHERE task_id=?1",
            [&value.id],
            |row| row.get::<_, usize>(0)
        )?,
        0
    );
    assert_eq!(db.connection()?.query_row("SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce='ordinary-task-phase-whole-ack'",[],|row|row.get::<_,usize>(0))?,0);
    assert_eq!(
        db.transaction(|tx| Ok(current_coverage(tx, &scope)?))?,
        before
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let prepared = prepare(None)?;
    let execution = db
        .transitions_in_scope(&scope)
        .commit_prepared_transition_with_work_request_and_memory_budget(
            &prepared, &work, &ledger, &policy,
        )?;
    let InputMutationExecution::Committed {
        value: actual,
        receipt,
    } = execution
    else {
        anyhow::bail!("original Task ACK missing")
    };
    assert!(actual.applied);
    assert!(!actual.effects.task_material_budget_views.is_empty());
    let delta = actual
        .effects
        .task_delta
        .as_ref()
        .context("actual Task domain delta")?;
    assert!(delta.coverage.active_task_ids.contains(&value.id));
    assert_eq!(delta.revision, delta.base_revision + 1);
    let last = actual.effects.clone();
    drop(actual);
    assert!(ledger.report()?.observed_bytes > 0);
    let replay = db
        .transitions_in_scope(&scope)
        .commit_prepared_transition_with_work_request_and_memory_budget(
            &prepared, &work, &ledger, &policy,
        )?;
    let InputMutationExecution::Replayed {
        receipt: replayed,
        _material_budget_views: replay_material,
    } = replay
    else {
        anyhow::bail!("whole Task receipt did not replay")
    };
    assert_eq!(replayed, receipt);
    assert_eq!(db.connection()?.query_row("SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce='ordinary-task-phase-whole-ack'",[],|row|row.get::<_,usize>(0))?,1);
    assert!(!replay_material.is_empty(), "original replay retains its own actual receipt guard");
    drop(replay_material);
    assert!(ledger.report()?.observed_bytes > 0, "original ACK last holder still owns its material");
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
