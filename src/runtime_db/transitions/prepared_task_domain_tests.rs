//! Real original Task/Queue transactions; all SQL assertions keep qualification strict.
use super::*;
use crate::{
    config::StateMemoryLimits,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_task::{CanonicalTaskFact, TaskCaptureRequest, TaskFamily, TaskKey},
    },
    runtime_db::{task_result_settlement as results, task_snapshot as tasks},
    types::{TaskKind, TaskStatus},
};

fn load_request() -> tasks::TaskDomainLoadRequest {
    tasks::TaskDomainLoadRequest {
        recovery_at: None,
        query: Default::default(),
        capture: TaskCaptureRequest::current(),
        record_limit: 4096,
        byte_limit: 4 * 1024 * 1024,
    }
}
fn ledger() -> Result<InputEvidenceBudgetLedger> {
    Ok(InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?)
}
fn initialized_task_fixture() -> Result<Fixture> {
    let f = Fixture::new()?;
    // A real owner publishes Ready only after the cold Task namespace exists.
    f.db.transaction(|tx| {
        tasks::prepare_task_domain_snapshot_with_budget_tx(
            tx,
            &f.scope,
            &load_request(),
            &ledger()?,
        )?
        .require_current()?;
        Ok(())
    })?;
    Ok(f)
}
fn command(f: &Fixture, id: &str, status: TaskStatus) -> TaskTransitionCommand {
    TaskTransitionCommand {
        agent_id: f.scope.agent_id.clone(),
        expected_owner_incarnation: f.scope.identity_incarnation,
        task: TaskRecord {
            id: id.into(),
            agent_id: f.scope.agent_id.clone(),
            kind: TaskKind::CommandTask,
            status,
            created_at: f.at,
            updated_at: f.at,
            parent_message_id: None,
            work_item_id: None,
            summary: Some(id.into()),
            detail: None,
            recovery: None,
        },
        agent_deletion: None,
        task_result_settlement: None,
        queue_entry: None,
        work_items: vec![],
        expected_wait_conditions: vec![],
        wait_conditions: vec![],
        agent_state: None,
        message_evidence: vec![],
        audit_events: vec![],
        index_changes: vec![],
        notify_scheduler: true,
        commit_on_idempotent: false,
        fault: None,
    }
}
fn freeze(
    f: &Fixture,
    nonce: &str,
    command: TaskTransitionCommand,
) -> Result<PreparedInputTransition> {
    PreparedInputTransition::task(
        f.scope.clone(),
        nonce.into(),
        f.at,
        PreparedTaskTransition::new(command, Default::default(), true, None, None)?,
    )
}
fn execute(
    f: &Fixture,
    p: &PreparedInputTransition,
    request: &tasks::TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<InputMutationExecution<TransitionCommit>> {
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition_with_work_request_and_memory_budget(
            p,
            &crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
                capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
                record_limit: 4096,
                byte_limit: 4 * 1024 * 1024,
            },
            ledger,
            request,
        )
}
fn aliases(f: &Fixture, id: &str) -> Result<usize> {
    Ok(f.db.connection()?.query_row("SELECT COUNT(*) FROM task_domain_records WHERE agent_id=?1 AND identity_incarnation=?2 AND record_id=?3",
        params![f.scope.agent_id,f.scope.identity_incarnation,id],|r|r.get(0))?)
}

fn original_command_creation(f: &Fixture, id: &str) -> Result<PreparedInputTransition> {
    let mut command = command(f, id, TaskStatus::Queued);
    command.task.detail = Some(serde_json::json!({
        "rejoin_obligation_id":id,"rejoin_generation":1,"cmd":"true",
    }));
    let creation = crate::runtime_db::command_task_creation::OriginalCommandTaskCreation::new(
        f.scope.clone(),
        command.task.clone(),
    )?;
    command.task = creation.task().clone();
    PreparedInputTransition::task(
        f.scope.clone(),
        format!("create:{id}"),
        f.at,
        PreparedTaskTransition::new(command, Default::default(), true, None, None)?
            .with_original_command_creation(std::sync::Arc::new(creation))?,
    )
}

#[test]
fn original_task_writer_uses_prepared_baseline_without_reloading_active_bodies() -> Result<()> {
    let f = initialized_task_fixture()?;
    let ledger = ledger()?;
    let mut large = command(&f, "command:large-baseline", TaskStatus::Queued);
    large.task.detail = Some(serde_json::json!({
        "rejoin_obligation_id":large.task.id,
        "rejoin_generation":1,
        "cmd":"true",
        "padding":"x".repeat(64 * 1024),
    }));
    let creation = std::sync::Arc::new(
        crate::runtime_db::command_task_creation::OriginalCommandTaskCreation::new(
            f.scope.clone(),
            large.task.clone(),
        )?,
    );
    large.task = creation.task().clone();
    let seed = PreparedInputTransition::task(
        f.scope.clone(),
        "create:large-baseline".into(),
        f.at,
        PreparedTaskTransition::new(large, Default::default(), true, None, None)?
            .with_original_command_creation(creation)?,
    )?;
    drop(execute(&f, &seed, &load_request(), &ledger)?);
    let baseline = f.db.transaction(|tx| {
        tasks::prepare_task_domain_snapshot_with_budget_tx(tx, &f.scope, &load_request(), &ledger)?
            .require_current()
    })?;
    let large_before_cold = baseline
        .resident
        .selected_install_records()
        .into_iter()
        .find(|record| record.locator.key.id == "command:large-baseline")
        .context("large baseline fact missing")?;
    let mut request = load_request();
    request.byte_limit = 32 * 1024;
    let cold = original_command_creation(&f, "command:cold-baseline-control")?;
    let InputMutationExecution::Committed { value, .. } = execute(&f, &cold, &request, &ledger)?
    else {
        panic!("cold control must execute its original command")
    };
    let cold_delta = value.effects.task_delta.context("cold Task ACK missing")?;
    let cold_large = cold_delta
        .records
        .iter()
        .find(|record| record.locator.key.id == "command:large-baseline")
        .context("cold ACK large fact missing")?;
    assert!(!std::sync::Arc::ptr_eq(&large_before_cold, cold_large));

    let baseline = f.db.transaction(|tx| {
        tasks::prepare_task_domain_snapshot_with_budget_tx(tx, &f.scope, &load_request(), &ledger)?
            .require_current()
    })?;
    let large_before_hot = baseline
        .resident
        .selected_install_records()
        .into_iter()
        .find(|record| record.locator.key.id == "command:large-baseline")
        .context("large prepared fact missing")?;
    let prepared = original_command_creation(&f, "command:small-after-baseline")?;
    let receipts = f.receipt_count()?;
    let prepared =
        prepared.with_task_bases(vec![std::sync::Arc::new(baseline.resident)], &ledger)?;
    let InputMutationExecution::Committed { value, .. } =
        execute(&f, &prepared, &request, &ledger)?
    else {
        panic!("prepared Task baseline must execute its original command")
    };
    assert!(value.applied);
    let delta = value.effects.task_delta.context("Task ACK missing")?;
    let hot_large = delta
        .records
        .iter()
        .find(|record| record.locator.key.id == "command:large-baseline")
        .context("prepared ACK large fact missing")?;
    // Ledger views can account for the same Arc more than once. Pointer identity
    // directly proves that the original writer retained the qualified body.
    assert!(std::sync::Arc::ptr_eq(&large_before_hot, hot_large));
    assert!(delta
        .coverage
        .active_task_ids
        .contains("command:large-baseline"));
    assert!(delta
        .coverage
        .active_task_ids
        .contains("command:small-after-baseline"));
    assert_eq!(f.receipt_count()?, receipts + 1);
    Ok(())
}

#[test]
fn deferred_task_roots_preserve_exact_replay_without_encoding() -> Result<()> {
    let f = initialized_task_fixture()?;
    let prepared = original_command_creation(&f, "command:deferred-replay")?;
    assert_deferred_task_roots_preserve_replay(&f, &prepared)
}

#[test]
fn deferred_task_roots_preserve_non_task_replay_without_encoding() -> Result<()> {
    let f = initialized_task_fixture()?;
    let prepared = PreparedInputTransition::queue(
        f.scope.clone(),
        "queue:deferred-replay".into(),
        f.at,
        f.queue()?,
    )?;
    assert_deferred_task_roots_preserve_replay(&f, &prepared)
}

fn assert_deferred_task_roots_preserve_replay(
    f: &Fixture,
    prepared: &PreparedInputTransition,
) -> Result<()> {
    use crate::diagnostics::resource_io::ResourceReadMaterial;
    use std::sync::Arc;

    let ledger = ledger()?;
    assert!(matches!(
        execute(f, prepared, &load_request(), &ledger)?,
        InputMutationExecution::Committed { .. }
    ));
    let root = Arc::new(f.db.transaction(|tx| {
        Ok(tasks::prepare_task_domain_snapshot_with_budget_tx(
            tx,
            &f.scope,
            &load_request(),
            &ledger,
        )?
        .require_current()?
        .resident)
    })?);
    let header = ResourceReadMaterial::new(&ledger, &f.scope)?.finish()?;
    let eager = ResourceReadMaterial::with_test_limits(&ledger, &f.scope, header.0, header.1)?;
    {
        let _entry = eager.enter();
        assert!(PreparedTaskBaselines::new(vec![Arc::clone(&root)], &ledger).is_err());
    }
    let deferred = ResourceReadMaterial::with_test_limits(&ledger, &f.scope, header.0, header.1)?;
    let mut bases = PreparedTaskBaselines::default();
    {
        let _entry = deferred.enter();
        bases.defer_roots(vec![Arc::clone(&root)]);
    }
    assert_eq!(deferred.finish()?, header);
    assert!(bases.task_base(&f.scope).is_none());
    let replay = |bases: &PreparedTaskBaselines| {
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition_with_input_query_and_task_bases(
                prepared,
                &crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
                    capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
                    record_limit: 4096,
                    byte_limit: 4 * 1024 * 1024,
                },
                &ledger,
                &load_request(),
                None,
                None,
                bases,
            )
    };
    // Measure the actual writer's exact replay without any baseline. Borrowing
    // roots must also fit that exact allowance, not merely a generous phase.
    let measured = ResourceReadMaterial::new(&ledger, &f.scope)?;
    {
        let _entry = measured.enter();
        assert!(matches!(
            replay(&PreparedTaskBaselines::default())?,
            InputMutationExecution::Replayed { .. }
        ));
    }
    let allowance = measured.finish()?;
    let tight =
        ResourceReadMaterial::with_test_limits(&ledger, &f.scope, allowance.0, allowance.1)?;
    let replay = {
        let _entry = tight.enter();
        bases.defer_roots(vec![root]);
        replay(&bases)?
    };
    assert!(matches!(replay, InputMutationExecution::Replayed { .. }));
    assert_eq!(tight.finish()?, allowance);
    assert!(bases.task_base(&f.scope).is_none());
    Ok(())
}

#[test]
fn task_sidecar_replacement_cannot_shadow_the_reloaded_root() -> Result<()> {
    use std::sync::Arc;

    let f = initialized_task_fixture()?;
    let ledger = ledger()?;
    let read_root = || {
        f.db.transaction(|tx| {
            Ok(Arc::new(
                tasks::load_task_domain_snapshot_with_budget_tx(
                    tx,
                    &f.scope,
                    &load_request(),
                    &ledger,
                )?
                .require_current()?
                .resident,
            ))
        })
    };
    let first = read_root()?;
    let mut bases = PreparedTaskBaselines::default();
    bases.defer_roots(vec![Arc::clone(&first)]);
    let stale = tasks::TaskWriteBaselineStale::new(&f.scope, &load_request(), &ledger)?;
    bases.reprepare(&f.db, &stale, &ledger)?;
    bases.defer_roots(vec![Arc::clone(&first)]);
    assert!(Arc::ptr_eq(
        &bases.task_base(&f.scope).unwrap().resident,
        &first
    ));
    let replacement = read_root()?;
    assert!(!Arc::ptr_eq(&first, &replacement));
    bases.defer_roots(vec![Arc::clone(&replacement)]);
    assert!(bases.task_base(&f.scope).is_none());
    bases.reprepare(&f.db, &stale, &ledger)?;
    assert!(Arc::ptr_eq(
        &bases.task_base(&f.scope).unwrap().resident,
        &replacement
    ));
    Ok(())
}

#[test]
fn task_sidecar_preparation_rejects_successor_before_decoding_state() -> Result<()> {
    let f = initialized_task_fixture()?;
    let ledger = ledger()?;
    let stale = tasks::TaskWriteBaselineStale::new(&f.scope, &load_request(), &ledger)?;
    let mut identity = f.db.agent_identities().latest(&f.scope.agent_id)?.unwrap();
    identity.incarnation += 1;
    identity.updated_at += chrono::Duration::seconds(1);
    f.db.transaction(|tx| {
        upsert_agent_identity_tx(tx, &identity)?;
        tx.execute(
            "UPDATE agent_states SET payload_json='poisoned-successor-state' WHERE agent_id=?1",
            [&f.scope.agent_id],
        )?;
        Ok(())
    })?;
    let receipts = f.receipt_count()?;
    let mut bases = PreparedTaskBaselines::new(Vec::new(), &ledger)?;
    bases.reprepare(&f.db, &stale, &ledger)?;
    assert_eq!(f.receipt_count()?, receipts);
    assert_eq!(
        f.db.connection()?.query_row(
            "SELECT payload_json FROM agent_states WHERE agent_id=?1",
            [&f.scope.agent_id],
            |row| row.get::<_, String>(0),
        )?,
        "poisoned-successor-state",
    );
    Ok(())
}

#[test]
fn original_creator_proof_reuses_holder_across_preparation_retries() -> Result<()> {
    let f = initialized_task_fixture()?;
    let ledger = ledger()?;
    let id = "command:creator-retry";
    let mut command = command(&f, id, TaskStatus::Queued);
    command.task.detail = Some(serde_json::json!({
        "rejoin_obligation_id":id,"rejoin_generation":1,"cmd":"true",
    }));
    let creation = std::sync::Arc::new(
        crate::runtime_db::command_task_creation::OriginalCommandTaskCreation::new(
            f.scope.clone(),
            command.task.clone(),
        )?,
    );
    command.task = creation.task().clone();
    let mut original_receipt = None;
    for attempt in 0..3 {
        let prepared = PreparedInputTransition::task(
            f.scope.clone(),
            format!("create:{id}"),
            f.at,
            PreparedTaskTransition::new(command.clone(), Default::default(), true, None, None)?
                .with_original_command_creation(std::sync::Arc::clone(&creation))?,
        )?;
        assert_eq!(std::sync::Arc::strong_count(&creation), 2);
        match execute(&f, &prepared, &load_request(), &ledger)? {
            InputMutationExecution::Committed { value, receipt } => {
                assert_eq!(attempt, 0);
                assert!(value.applied);
                original_receipt = Some(receipt);
            }
            InputMutationExecution::Replayed { receipt, .. } => {
                assert!(attempt > 0);
                assert_eq!(Some(&receipt), original_receipt.as_ref());
            }
        }
        drop(prepared);
        assert_eq!(std::sync::Arc::strong_count(&creation), 1);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn original_creator_transaction_persists_source_and_replays_without_reconstruction() -> Result<()> {
    let f = initialized_task_fixture()?;
    let ledger = ledger()?;
    let id = "command:creator-api";
    let prepared = original_command_creation(&f, id)?;
    let InputMutationExecution::Committed { value, receipt } =
        execute(&f, &prepared, &load_request(), &ledger)?
    else {
        panic!("original creator must commit")
    };
    assert!(value.applied);
    let task = f.db.tasks().latest(id)?.context("created Task missing")?;
    assert_eq!(
        task.rejoin_fence().unwrap().source.command_creation_id(),
        Some(id)
    );
    let original: String = f.db.connection()?.query_row(
        "SELECT original_task_json FROM command_task_creation_sources WHERE task_id=?1",
        [id],
        |r| r.get(0),
    )?;
    let original: TaskRecord = serde_json::from_str(&original)?;
    assert!(original
        .detail
        .as_ref()
        .unwrap()
        .get("command_creation_id")
        .is_none());
    assert!(original
        .detail
        .as_ref()
        .unwrap()
        .get("parent_turn_id")
        .is_none());
    drop(value);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let InputMutationExecution::Replayed {
        receipt: replayed, ..
    } = execute(&f, &prepared, &load_request(), &ledger)?
    else {
        panic!("exact creator replay must return its original receipt")
    };
    assert_eq!(replayed, receipt);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn creator_finalizer_failure_rolls_back_origin_task_and_receipt() -> Result<()> {
    let f = initialized_task_fixture()?;
    let ledger = ledger()?;
    let id = "command:creator-atomic-api";
    let prepared = original_command_creation(&f, id)?;
    f.db.connection()?.execute_batch(
        "CREATE TRIGGER fail_creator_task_alias BEFORE INSERT ON task_domain_records
         BEGIN SELECT RAISE(ABORT,'creator-finalizer-fault'); END;",
    )?;
    let before = f.receipt_count()?;
    let error = execute(&f, &prepared, &load_request(), &ledger).unwrap_err();
    assert!(
        format!("{error:#}").contains("creator-finalizer-fault"),
        "{error:#}"
    );
    assert!(f.db.tasks().latest(id)?.is_none());
    let count: u64 = f.db.connection()?.query_row(
        "SELECT count(*) FROM command_task_creation_sources WHERE task_id=?1",
        [id],
        |r| r.get(0),
    )?;
    assert_eq!(count, 0);
    assert_eq!(f.receipt_count()?, before);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    f.db.connection()?
        .execute_batch("DROP TRIGGER fail_creator_task_alias")?;
    assert!(matches!(
        execute(&f, &prepared, &load_request(), &ledger)?,
        InputMutationExecution::Committed { .. }
    ));
    Ok(())
}

#[test]
fn original_task_whole_ack_installs_exact_fact_and_noop_replays_without_latest() -> Result<()> {
    let f = initialized_task_fixture()?;
    let ledger = ledger()?;
    let command = command(&f, "task:whole-qualified", TaskStatus::Running);
    let original = freeze(&f, "task:whole-qualified:first", command.clone())?;
    let InputMutationExecution::Committed { value, receipt } =
        execute(&f, &original, &load_request(), &ledger)?
    else {
        panic!("initial Task must ACK")
    };
    let key = TaskKey::new(TaskFamily::Task, &command.task.id);
    assert_eq!(value.effects.task_write_keys, BTreeSet::from([key.clone()]));
    assert!(value.effects.task_observation_keys.is_empty());
    assert!(value.effects.task_delta_requires_prepared_domain);
    let delta = value.effects.task_delta.as_ref().unwrap();
    assert_eq!(delta.revision, delta.base_revision + 1);
    assert_eq!(
        delta
            .records
            .iter()
            .find(|v| v.locator.key == key)
            .unwrap()
            .fact,
        CanonicalTaskFact::Task(command.task.clone())
    );
    assert!(ledger.report()?.observed_bytes > 0);
    drop(value);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let noop = freeze(&f, "task:whole-qualified:noop", command.clone())?;
    let InputMutationExecution::Committed {
        value: noop_value, ..
    } = execute(&f, &noop, &load_request(), &ledger)?
    else {
        panic!("distinct noop still writes a receipt")
    };
    assert!(!noop_value.applied);
    assert!(noop_value.effects.task_write_keys.is_empty());
    assert!(noop_value.effects.task_observation_keys.contains(&key));
    let noop_delta = noop_value.effects.task_delta.as_ref().unwrap();
    assert_eq!(noop_delta.base_revision, noop_delta.revision);
    drop(noop_value);
    let mut later = command;
    later.task.summary = Some("later canonical summary".into());
    later.task.updated_at += chrono::Duration::milliseconds(1);
    execute(
        &f,
        &freeze(&f, "task:whole-qualified:later", later)?,
        &load_request(),
        &ledger,
    )?;
    let before = f.db.tasks().latest("task:whole-qualified")?;
    let InputMutationExecution::Replayed {
        receipt: replayed, ..
    } = execute(&f, &original, &load_request(), &ledger)?
    else {
        panic!("original receipt wins before stale Task read")
    };
    assert_eq!(replayed, receipt);
    assert_eq!(f.db.tasks().latest("task:whole-qualified")?, before);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn task_finalizer_fault_rolls_back_business_qualification_and_whole_receipt() -> Result<()> {
    let f = initialized_task_fixture()?;
    let ledger = ledger()?;
    let p = freeze(
        &f,
        "task:alias-fault",
        command(&f, "task:alias-fault", TaskStatus::Running),
    )?;
    f.db.connection()?.execute_batch("CREATE TRIGGER fail_task_alias BEFORE INSERT ON task_domain_records BEGIN SELECT RAISE(ABORT,'task-finalizer-fault'); END;")?;
    let before = f.receipt_count()?;
    let error = execute(&f, &p, &load_request(), &ledger).unwrap_err();
    assert!(
        format!("{error:#}").contains("task-finalizer-fault"),
        "{error:#}"
    );
    assert!(f.db.tasks().latest("task:alias-fault")?.is_none());
    assert_eq!(aliases(&f, "task:alias-fault")?, 0);
    assert_eq!(f.receipt_count()?, before);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    f.db.connection()?
        .execute_batch("DROP TRIGGER fail_task_alias")?;
    assert!(matches!(
        execute(&f, &p, &load_request(), &ledger)?,
        InputMutationExecution::Committed { .. }
    ));
    Ok(())
}

#[test]
fn original_task_raw_cleanup_and_new_incarnation_collision_never_adopt() -> Result<()> {
    let f = initialized_task_fixture()?;
    let ledger = ledger()?;
    let raw = command(&f, "task:raw-cleanup", TaskStatus::Running);
    f.db.transaction(|tx| {
        crate::runtime_db::repositories::upsert_task_tx(tx, &raw.task).map(|_| ())
    })?;
    let mut terminal = raw.clone();
    terminal.task.status = TaskStatus::Interrupted;
    terminal.task.updated_at += chrono::Duration::milliseconds(1);
    let InputMutationExecution::Committed { value, .. } = execute(
        &f,
        &freeze(&f, "raw-cleanup", terminal)?,
        &load_request(),
        &ledger,
    )?
    else {
        panic!("original cleanup must ACK")
    };
    assert!(value
        .effects
        .task_write_keys
        .contains(&TaskKey::new(TaskFamily::Task, &raw.task.id)));
    assert_eq!(aliases(&f, &raw.task.id)?, 0);
    assert!(!value
        .effects
        .task_delta
        .as_ref()
        .unwrap()
        .coverage
        .task_metadata
        .contains_key(&raw.task.id));
    drop(value);
    let first = command(&f, "task:old-inc", TaskStatus::Running);
    execute(
        &f,
        &freeze(&f, "old-inc-first", first.clone())?,
        &load_request(),
        &ledger,
    )?;
    let mut identity = f.db.agent_identities().latest(&f.scope.agent_id)?.unwrap();
    identity.incarnation = 2;
    identity.updated_at += chrono::Duration::seconds(1);
    f.db.transaction(|tx| upsert_agent_identity_tx(tx, &identity))?;
    let successor = InputEvidenceScope {
        agent_id: f.scope.agent_id.clone(),
        identity_incarnation: 2,
    };
    let stale = PreparedInputTransition::task(
        successor.clone(),
        "new-inc-stale-command".into(),
        f.at,
        PreparedTaskTransition::new(first.clone(), Default::default(), true, None, None)?,
    )?;
    let before = f.receipt_count()?;
    let namespace_before = namespace_rows(&f)?;
    assert!(f
        .db
        .transitions_in_scope(&successor)
        .commit_prepared_transition_with_work_request_and_memory_budget(
            &stale,
            &crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
                capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
                record_limit: 4096,
                byte_limit: 4 * 1024 * 1024,
            },
            &ledger,
            &load_request(),
        )
        .is_err());
    assert_eq!(f.receipt_count()?, before);
    assert_eq!(namespace_rows(&f)?, namespace_before);
    assert_eq!(
        f.db.connection()?.query_row(
            "SELECT COUNT(*) FROM task_domain_records WHERE agent_id=?1 AND identity_incarnation=2",
            [&f.scope.agent_id],
            |row| row.get::<_, usize>(0)
        )?,
        0
    );
    f.db.transaction(|tx| {
        tasks::prepare_task_domain_snapshot_with_budget_tx(
            tx,
            &successor,
            &load_request(),
            &ledger,
        )?
        .require_current()?;
        Ok(())
    })?;
    let mut first = first;
    first.expected_owner_incarnation = successor.identity_incarnation;
    let prepared = PreparedInputTransition::task(
        successor.clone(),
        "new-inc-duplicate".into(),
        f.at,
        PreparedTaskTransition::new(first, Default::default(), true, None, None)?,
    )?;
    let InputMutationExecution::Committed { value, .. } =
        f.db.transitions_in_scope(&successor)
            .commit_prepared_transition_with_work_request_and_memory_budget(
                &prepared,
                &crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
                    capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
                    record_limit: 4096,
                    byte_limit: 4 * 1024 * 1024,
                },
                &ledger,
                &load_request(),
            )?
    else {
        panic!("no-op still has original receipt")
    };
    assert!(value
        .effects
        .task_delta
        .as_ref()
        .unwrap()
        .coverage
        .task_metadata
        .is_empty());
    assert_eq!(f.db.connection()?.query_row("SELECT COUNT(*) FROM task_domain_records WHERE agent_id=?1 AND identity_incarnation=2 AND record_id='task:old-inc'",[&f.scope.agent_id],|r|r.get::<_,usize>(0))?,0);
    Ok(())
}

#[test]
fn task_proof_budget_precedes_decode_and_original_business_sql() -> Result<()> {
    let f = initialized_task_fixture()?;
    let ledger = ledger()?;
    let raw = command(&f, "task:oversize-before", TaskStatus::Running);
    f.db.transaction(|tx| {
        crate::runtime_db::repositories::upsert_task_tx(tx, &raw.task).map(|_| ())
    })?;
    f.db.connection()?.execute(
        "UPDATE tasks SET input_target=?1,payload_json='{' WHERE task_id=?2",
        params!["x".repeat(1024 * 1024), raw.task.id],
    )?;
    let mut request = load_request();
    request.byte_limit = 64 * 1024;
    let p = freeze(&f, "budget-before-task", raw)?;
    let before = f.receipt_count()?;
    let error = execute(&f, &p, &request, &ledger).unwrap_err();
    assert!(
        !matches!(
            error.downcast_ref::<tasks::TaskSnapshotError>(),
            Some(tasks::TaskSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(f.receipt_count()?, before);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let payload: String = f.db.connection()?.query_row(
        "SELECT payload_json FROM tasks WHERE task_id='task:oversize-before'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(payload, "{");
    Ok(())
}

#[test]
fn original_queue_result_settlement_qualifies_actual_ids_once_and_preserves_replay() -> Result<()> {
    let f = initialized_task_fixture()?;
    let ledger = ledger()?;
    let mut queue = f.queue()?;
    let source = &mut queue.command.message_evidence[0];
    source.kind = MessageKind::TaskResult;
    source.task_id = Some("task:queue-result".into());
    source.origin = MessageOrigin::Task {
        task_id: "task:queue-result".into(),
    };
    source.authority_class = AuthorityClass::RuntimeInstruction;
    let message = source.clone();
    let admission =
        PreparedInputTransition::queue(f.scope.clone(), "queue:result-source".into(), f.at, queue)?;
    execute(&f, &admission, &load_request(), &ledger)?;
    let mut task = command(&f, "task:queue-result", TaskStatus::Completed);
    task.task.parent_message_id = Some(message.id.clone());
    task.task.detail = Some(serde_json::json!({"rejoin_obligation_id":task.task.id,
        "rejoin_generation":1,"parent_turn_id":"turn:original-parent"}));
    let result = results::TaskResultSettlementRecord::pending(&task.task, &message, f.at)?.unwrap();
    task.task_result_settlement = Some(result.clone());
    execute(
        &f,
        &freeze(&f, "task:queue-result-pending", task)?,
        &load_request(),
        &ledger,
    )?;
    f.db.transaction(|tx| {
        let key = TaskKey::new(TaskFamily::ResultSettlement, &result.result_identity);
        let keys = BTreeSet::from([key]);
        let proof = tasks::prepare_task_write_tx(tx, &f.scope, &keys)?;
        let prepared = results::prepare_admit_message_tx(
            tx,
            &f.scope.agent_id,
            &message.id,
            "activation:queue-result",
            f.at,
            results::TaskResultWriteLimits::default(),
        )?;
        let ack = prepared.execute()?;
        let actual = ack
            .actual_result_ids
            .into_iter()
            .map(|id| TaskKey::new(TaskFamily::ResultSettlement, id))
            .collect();
        let state =
            crate::runtime_db::context_inputs::ContextDurableRead::new(tx, &f.scope.agent_id)
                .scope_facts()?
                .state
                .context("current fixture State missing")?;
        tasks::register_task_writes_tx(tx, &proof, &actual, &state, &load_request())?;
        Ok(())
    })?;
    let mut queue = f.queue()?;
    queue.command.message_evidence.clear();
    queue.command.agent_state = None;
    queue.task_result_settlement = Some(results::TaskResultActivationSettlement {
        activation_id: "activation:queue-result".into(),
        disposition: results::TaskResultSettlementDisposition::ModelDelivered,
        settled_at: f.at,
    });
    let p = PreparedInputTransition::queue(
        f.scope.clone(),
        "queue:result-settled".into(),
        f.at,
        queue,
    )?;
    let InputMutationExecution::Committed { value, receipt } =
        execute(&f, &p, &load_request(), &ledger)?
    else {
        panic!("actual Result write must ACK")
    };
    let key = TaskKey::new(TaskFamily::ResultSettlement, &result.result_identity);
    assert_eq!(value.effects.task_write_keys, BTreeSet::from([key.clone()]));
    assert!(value.effects.task_observation_keys.contains(&key));
    let delta = value.effects.task_delta.as_ref().unwrap();
    assert_eq!(delta.revision, delta.base_revision + 1);
    assert_eq!(
        delta.coverage.result_metadata[&result.result_identity].state,
        results::TaskResultSettlementState::Settled
    );
    assert!(!delta
        .coverage
        .unsettled_result_ids
        .contains(&result.result_identity));
    let InputMutationExecution::Replayed { receipt: again, .. } =
        execute(&f, &p, &load_request(), &ledger)?
    else {
        panic!("original Queue receipt must replay")
    };
    assert_eq!(again, receipt);
    drop(value);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn original_protocol_only_change_reobserves_result_activation_without_task_row_write() -> Result<()>
{
    use crate::domain::execution_protocol::{
        admit_execution, interrupt_execution, AdmitExecution, AdmittedFences, ExecutionAttempt,
        ExecutionAttemptState, ExecutionBinding, ExecutionOrigin, ExecutionPriority,
        ExecutionProtocolCommand, ExecutionProtocolState, ExecutionProvenance, ExecutionSource,
        ExecutionSourceIdentity, ExecutionTrust, InterruptExecution,
    };
    let f = initialized_task_fixture()?;
    let ledger = ledger()?;
    let mut queue = f.queue()?;
    let source = &mut queue.command.message_evidence[0];
    source.kind = MessageKind::TaskResult;
    source.task_id = Some("task:activation-only".into());
    source.origin = MessageOrigin::Task {
        task_id: "task:activation-only".into(),
    };
    source.authority_class = AuthorityClass::RuntimeInstruction;
    let source_id = queue.command.message_evidence[0].id.clone();
    execute(
        &f,
        &PreparedInputTransition::queue(f.scope.clone(), "activation-source".into(), f.at, queue)?,
        &load_request(),
        &ledger,
    )?;
    let source: MessageEnvelope = serde_json::from_str(&f.db.connection()?.query_row(
        "SELECT payload_json FROM messages WHERE message_id=?1",
        [&source_id],
        |r| r.get::<_, String>(0),
    )?)?;
    let mut task = command(&f, "task:activation-only", TaskStatus::Completed);
    task.task.parent_message_id = Some(source_id.clone());
    task.task.detail = Some(serde_json::json!({"rejoin_obligation_id":task.task.id,
        "rejoin_generation":1,"parent_turn_id":"turn:activation-parent"}));
    let result = results::TaskResultSettlementRecord::pending(&task.task, &source, f.at)?.unwrap();
    task.task_result_settlement = Some(result.clone());
    execute(
        &f,
        &freeze(&f, "activation-pending", task.clone())?,
        &load_request(),
        &ledger,
    )?;
    let sequence = source.message_seq.context("actual sequence missing")?;
    let admitted = admit_execution(
        &ExecutionProtocolState::empty(&f.scope.agent_id),
        &AdmitExecution {
            attempt: ExecutionAttempt {
                input_projection: None,
                attempt_id: "activation:metadata-only".into(),
                agent_id: f.scope.agent_id.clone(),
                source_message_id: Some(source_id.clone()),
                source: ExecutionSource {
                    identity: ExecutionSourceIdentity::QueueMessage {
                        message_id: source_id.clone(),
                    },
                    generation: sequence,
                },
                binding: ExecutionBinding::Conversation {
                    interaction_id: source_id.clone(),
                },
                provenance: ExecutionProvenance {
                    origin: ExecutionOrigin::Task,
                    trust: ExecutionTrust::RuntimeInstruction,
                    priority: ExecutionPriority::Normal,
                    correlation_id: None,
                    causation_id: None,
                },
                admitted_fences: AdmittedFences {
                    source_revision: sequence,
                    work_item_source_revision: None,
                    work_item_generation: None,
                    rejoin: None,
                    agent_control_revision: 1,
                    host_registry_revision: 1,
                },
                state: ExecutionAttemptState::Open,
                run_id: None,
                turn_id: None,
                recovery_of_attempt_id: None,
                terminal_outcome_id: None,
                admitted_at: f.at.to_rfc3339(),
                terminal_at: None,
            },
        },
    )
    .map_err(anyhow::Error::msg)?
    .state;
    f.db.transaction(|tx| {
        // The original typed protocol fixture writer provides the lifecycle fact.
        // This focused test exercises Task's metadata finalizer, not D3 admission.
        persist_state_tx(tx, &admitted)?;
        let key = TaskKey::new(TaskFamily::ResultSettlement, &result.result_identity);
        let keys = BTreeSet::from([key]);
        let proof = tasks::prepare_task_write_tx(tx, &f.scope, &keys)?;
        let ack = results::prepare_admit_message_tx(
            tx,
            &f.scope.agent_id,
            &source_id,
            "activation:metadata-only",
            f.at,
            results::TaskResultWriteLimits::default(),
        )?
        .execute()?;
        let actual = ack
            .actual_result_ids
            .into_iter()
            .map(|id| TaskKey::new(TaskFamily::ResultSettlement, id))
            .collect();
        let state =
            crate::runtime_db::context_inputs::ContextDurableRead::new(tx, &f.scope.agent_id)
                .scope_facts()?
                .state
                .unwrap();
        let facts = tasks::register_task_writes_tx(tx, &proof, &actual, &state, &load_request())?;
        assert!(facts.delta.coverage.result_metadata[&result.result_identity].activation_open);
        Ok(())
    })?;
    let interrupted = InterruptExecution {
        attempt_id: "activation:metadata-only".into(),
        outcome_id: "outcome:metadata-only".into(),
        reason: "original interruption".into(),
        interrupted_at: f.at.to_rfc3339(),
    };
    task.task_result_settlement = None;
    let p = PreparedInputTransition::task(
        f.scope.clone(),
        "activation-only-observer".into(),
        f.at,
        PreparedTaskTransition::new(
            task,
            ExecutionProtocolTransition {
                bootstrap: None,
                commands: vec![ExecutionProtocolCommand::Interrupt(interrupted.clone())],
            },
            true,
            None,
            None,
        )?,
    )?;
    let baseline = f.db.transaction(|tx| {
        tasks::prepare_task_domain_snapshot_with_budget_tx(tx, &f.scope, &load_request(), &ledger)?
            .require_current()
    })?;
    let p = p.with_task_bases(vec![std::sync::Arc::new(baseline.resident)], &ledger)?;
    let before: String = f.db.connection()?.query_row(
        "SELECT payload_json FROM task_result_settlements WHERE result_identity=?1",
        [&result.result_identity],
        |r| r.get(0),
    )?;
    let value = f.db.transaction(|tx| {
        let prepared = super::super::task_writes::prepare_task_transaction_tx(
            tx,
            &p,
            &f.scope,
            false,
            &load_request(),
            &ledger,
            None,
            None,
        )?
        .context("activation omitted Task finalizer")?;
        let final_protocol = interrupt_execution(&admitted, &interrupted)
            .map_err(anyhow::Error::msg)?
            .state;
        persist_state_tx(tx, &final_protocol)?;
        let mut value = TransitionCommit::default();
        prepared.finish(tx, &mut value, &ledger, None)?;
        let delta = value.effects.task_delta.as_ref().unwrap();
        assert_eq!(delta.revision, delta.base_revision + 1);
        assert!(!delta.coverage.result_metadata[&result.result_identity].activation_open);
        assert!(value.effects.task_write_keys.is_empty());
        let after: String = tx.query_row(
            "SELECT payload_json FROM task_result_settlements WHERE result_identity=?1",
            [&result.result_identity],
            |r| r.get(0),
        )?;
        assert_eq!(
            after, before,
            "activation observation must not rewrite Result body"
        );
        Ok(value)
    })?;
    assert!(ledger.report()?.observed_bytes > 0);
    drop(value);
    drop(p);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn bounded_task_preparation_exhaustion_alerts_once_without_failure_warns() -> Result<()> {
    use crate::runtime_db::write_log_probe::WriteLogProbe;

    // A reader that keeps resolving preparation without refreshing the
    // baseline leaves every attempt stale, so the bounded budget must
    // exhaust and alert exactly once.
    let f = initialized_task_fixture()?;
    let prepared = freeze(
        &f,
        "reprepare-exhausted",
        command(&f, "exhausted", TaskStatus::Queued),
    )?;
    let ledger = ledger()?;
    let request = load_request();
    let work = crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
        capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
        record_limit: 4096,
        byte_limit: 4 * 1024 * 1024,
    };
    let mut bases = PreparedTaskBaselines::new(Vec::new(), &ledger)?;
    bases.silent_reprepare = true;
    let probe = WriteLogProbe::default();
    let error = probe
        .capture(|| {
            f.db.transitions_in_scope(&f.scope)
                .commit_prepared_transition_with_task_preparation(
                    &prepared, &work, &ledger, &request, &mut bases,
                )
        })
        .unwrap_err();
    assert!(
        error
            .downcast_ref::<tasks::TaskWriteBaselineStale>()
            .is_some(),
        "{error:#}"
    );
    assert_eq!(
        probe.write_failure_warns(),
        0,
        "intermediate reprepare attempts must not surface as failure WARNs"
    );
    assert!(
        probe.deferred_reprepare_events() >= 1,
        "at least one intermediate attempt must leave a reader_reprepare event"
    );
    assert_eq!(
        probe.terminal_alerts("reprepare attempts exhausted"),
        1,
        "budget exhaustion must alert exactly once"
    );
    Ok(())
}

#[path = "prepared_task_cleanup_cap_tests.rs"]
mod cleanup_cap_tests;

#[path = "prepared_task_input_cleanup_tests.rs"]
mod input_cleanup_tests;

#[path = "prepared_task_message_budget_tests.rs"]
mod message_budget_tests;
