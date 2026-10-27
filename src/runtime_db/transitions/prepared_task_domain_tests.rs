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

#[test]
fn original_task_whole_ack_installs_exact_fact_and_noop_replays_without_latest() -> Result<()> {
    let f = Fixture::new()?;
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
    let f = Fixture::new()?;
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
    let f = Fixture::new()?;
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
    let f = Fixture::new()?;
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
        matches!(
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
    let f = Fixture::new()?;
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
    let f = Fixture::new()?;
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
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[path = "prepared_task_cleanup_cap_tests.rs"]
mod cleanup_cap_tests;

#[path = "prepared_task_input_cleanup_tests.rs"]
mod input_cleanup_tests;

#[path = "prepared_task_message_budget_tests.rs"]
mod message_budget_tests;
