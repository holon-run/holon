// Tests share the existing fake-process registration and original monitor helpers.
async fn accepted_original_producer_fixture() -> Result<(
    TempDir,
    TempDir,
    RuntimeHandle,
    TaskRecord,
    FakeRunningProcess,
    Arc<super::super::control_cancellation::ControlJoinLease>,
    Arc<OriginalCommandTaskProducer>,
)> {
    let (home, workspace, runtime) = test_runtime();
    let process = FakeRunningProcess::pending();
    let spec = command_spec(false, false);
    let resolved = resolved_command(&runtime, &spec).await;
    let task = runtime
        .register_command_task(
            "original producer".into(),
            resolved,
            running_command(process.clone(), "original output", ""),
            AuthorityClass::OperatorInstruction,
            false,
            CapturedOutput::default(),
        )
        .await?;
    wait_for_latest_task(&runtime, &task.id, TaskStatus::Running).await;
    let lease = runtime
        .inner
        .task_handles
        .lock()
        .await
        .get(&task.id)
        .and_then(ManagedTaskHandle::control_lease)
        .context("registered Command lease missing")?;
    let source = lease
        .original_command()?
        .context("registered Command producer missing")?;
    Ok((home, workspace, runtime, task, process, lease, source))
}
fn original_terminal_receipt(
    runtime: &RuntimeHandle,
    source: &OriginalCommandTaskProducer,
) -> Result<Option<crate::runtime_db::input_mutation_receipts::InputMutationReceipt>> {
    use rusqlite::OptionalExtension;
    runtime.inner.runtime_db.transaction(|tx| {
        let row: Option<String> = tx
            .query_row(
                "SELECT prepared_json FROM input_mutation_receipts WHERE command_nonce=?1",
                [format!("command-terminal:{}", source.address().lease_id)],
                |r| r.get(0),
            )
            .optional()?;
        let Some(raw) = row else { return Ok(None) };
        let prepared = serde_json::from_str(&raw)?;
        crate::runtime_db::input_mutation_receipts::lookup_input_mutation_receipt_tx(tx, &prepared)
    })
}
async fn await_original_durable_terminal(source: &OriginalCommandTaskProducer) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !source.is_durable_finished() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("original terminal responsibility did not finish")?;
    Ok(())
}
#[tokio::test]
async fn original_command_task_none_named_terminal_keeps_actual_result_once_and_releases_capsule(
) -> Result<()> {
    let (_home, _workspace, runtime, task, process, lease, source) =
        accepted_original_producer_fixture().await?;
    let old = runtime
        .inner
        .mutation_owner
        .capture()?
        .context("published original root missing")?;
    let weak = Arc::downgrade(old.tasks.as_ref().context("original Task domain missing")?);
    let mut reservation = runtime.inner.mutation_owner.reserve().await?;
    reservation.begin("fixture-original-task-unprepared".into())?;
    reservation.committed_prepared_with_domains(
        old.state.clone(),
        old.episodes.clone(),
        old.input_evidence.clone(),
        old.work.clone(),
        None,
    )?;
    reservation.publish(old.state.clone())?;
    drop((reservation, old));
    assert!(runtime
        .inner
        .mutation_owner
        .capture()?
        .unwrap()
        .tasks
        .is_none());
    *process.status.lock().await = Some(RunningProcessExitStatus::new(Some(0), None));
    await_original_durable_terminal(&source).await?;
    lease.join().await?;
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .tasks()
            .latest(&task.id)?
            .unwrap()
            .status,
        TaskStatus::Completed
    );
    let receipt = original_terminal_receipt(&runtime, &source)?.unwrap();
    use crate::runtime_db::{
        command_task_terminal::CommandTaskTerminalResult,
        input_mutation_receipts::InputMutationResult,
    };
    let InputMutationResult::CommandTaskTerminal { ack } = &receipt.outcome.result else {
        panic!("wrong original terminal result")
    };
    let CommandTaskTerminalResult::Delivered { message_id } = &ack.result else {
        panic!("actual result was lost")
    };
    assert_eq!(
        runtime.inner.runtime_db.connection()?.query_row(
            "SELECT COUNT(*) FROM messages WHERE evidence_id=?1",
            [message_id],
            |r| r.get::<_, usize>(0)
        )?,
        1
    );
    let actual_source = runtime.inner.runtime_db.transaction(|tx| {
        crate::runtime_db::evidence::committed_message_tx(tx, &task.agent_id, message_id)
    })?;
    let actual_task = runtime.inner.runtime_db.tasks().latest(&task.id)?.unwrap();
    assert!(actual_task.rejoin_fence().is_err());
    assert!(actual_task
        .detail
        .as_ref()
        .unwrap()
        .get("parent_turn_id")
        .is_none());
    assert!(
        actual_source.turn_id.is_some(),
        "Result retains its dispatch hint"
    );
    assert!(actual_source.metadata.as_ref().unwrap()["task_detail"]
        .get("parent_turn_id")
        .is_none());
    assert_eq!(
        runtime.inner.runtime_db.connection()?.query_row(
            "SELECT COUNT(*) FROM task_result_settlements WHERE task_id=?1",
            [&task.id],
            |r| r.get::<_, usize>(0)
        )?,
        0,
        "no rejoin must not invent an obligation"
    );
    assert!(runtime
        .inner
        .task_handles
        .lock()
        .await
        .get(&task.id)
        .is_none());
    assert!(source.frozen_terminal()?.is_none());
    assert!(
        weak.upgrade().is_none(),
        "capsule must not retain an old root"
    );
    runtime
        .continue_original_command_terminal(source.clone())
        .await?;
    assert_eq!(
        original_terminal_receipt(&runtime, &source)?.unwrap(),
        receipt
    );
    Ok(())
}
#[tokio::test]
async fn original_command_no_state_updates_existing_task_but_returns_unavailable_without_ready(
) -> Result<()> {
    let (_home, _workspace, runtime, task, process, lease, source) =
        accepted_original_producer_fixture().await?;
    runtime.inner.runtime_db.connection()?.execute(
        "DELETE FROM agent_states WHERE agent_id=?1",
        [&task.agent_id],
    )?;
    *process.status.lock().await = Some(RunningProcessExitStatus::new(Some(0), None));
    await_original_durable_terminal(&source).await?;
    lease.join().await?;
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .tasks()
            .latest(&task.id)?
            .unwrap()
            .status,
        TaskStatus::Completed
    );
    let receipt = original_terminal_receipt(&runtime, &source)?.unwrap();
    use crate::runtime_db::{
        command_task_terminal::{
            CommandTaskTerminalInstallMode, CommandTaskTerminalResult,
            CommandTaskTerminalUnavailable,
        },
        input_mutation_receipts::InputMutationResult,
    };
    let InputMutationResult::CommandTaskTerminal { ack } = &receipt.outcome.result else {
        panic!("wrong receipt")
    };
    assert_eq!(ack.install_mode, CommandTaskTerminalInstallMode::NoState);
    assert!(ack.task_changed);
    assert_eq!(
        ack.result,
        CommandTaskTerminalResult::Unavailable {
            reason: CommandTaskTerminalUnavailable::OwnerNoState
        }
    );
    assert!(runtime
        .inner
        .runtime_db
        .agent_states()
        .latest(&task.agent_id)?
        .is_none());
    assert_eq!(
        runtime.inner.runtime_db.connection()?.query_row(
            "SELECT COUNT(*) FROM messages WHERE task_id=?1",
            [&task.id],
            |r| r.get::<_, usize>(0)
        )?,
        0
    );
    assert!(
        runtime.inner.mutation_owner.capture()?.is_none(),
        "NoState must not publish fake State/TaskReady"
    );
    assert!(source.frozen_terminal()?.is_none());
    Ok(())
}
#[tokio::test]
async fn original_command_retired_disposition_does_not_read_successor_state_or_write_terminal_task(
) -> Result<()> {
    let (_home, _workspace, runtime, task, process, lease, source) =
        accepted_original_producer_fixture().await?;
    let mut identity = runtime
        .inner
        .runtime_db
        .agent_identities()
        .latest(&task.agent_id)?
        .unwrap();
    identity.incarnation += 1;
    runtime
        .inner
        .runtime_db
        .agent_identities()
        .upsert(&identity)?;
    runtime.inner.runtime_db.connection()?.execute(
        "UPDATE agent_states SET payload_json='poisoned successor State' WHERE agent_id=?1",
        [&task.agent_id],
    )?;
    *process.status.lock().await = Some(RunningProcessExitStatus::new(Some(0), None));
    await_original_durable_terminal(&source).await?;
    lease.join().await?;
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .tasks()
            .latest(&task.id)?
            .unwrap()
            .status,
        TaskStatus::Running
    );
    let receipt = original_terminal_receipt(&runtime, &source)?.unwrap();
    use crate::runtime_db::{
        command_task_terminal::{CommandTaskTerminalResult, CommandTaskTerminalUnavailable},
        input_mutation_receipts::InputMutationResult,
    };
    let InputMutationResult::CommandTaskTerminal { ack } = &receipt.outcome.result else {
        panic!("wrong receipt")
    };
    assert!(!ack.task_changed);
    assert_eq!(
        ack.result,
        CommandTaskTerminalResult::Unavailable {
            reason: CommandTaskTerminalUnavailable::ScopeRetired
        }
    );
    assert_eq!(
        runtime.inner.runtime_db.connection()?.query_row(
            "SELECT payload_json FROM agent_states WHERE agent_id=?1",
            [&task.agent_id],
            |r| r.get::<_, String>(0)
        )?,
        "poisoned successor State"
    );
    assert!(receipt.outcome.scopes[0].install_keys.tasks.is_empty());
    Ok(())
}
#[tokio::test]
async fn original_command_receipt_failure_keeps_frozen_nonce_body_and_exact_lease_until_retry(
) -> Result<()> {
    let (_home, _workspace, runtime, task, process, lease, source) =
        accepted_original_producer_fixture().await?;
    runtime.inner.runtime_db.connection()?.execute_batch("CREATE TRIGGER reject_original_command_receipt BEFORE INSERT ON input_mutation_receipts WHEN NEW.command_nonce LIKE 'command-terminal:%' BEGIN SELECT RAISE(ABORT,'original Command receipt rollback'); END;")?;
    *process.status.lock().await = Some(RunningProcessExitStatus::new(Some(0), None));
    lease.join().await?;
    assert!(!source.is_durable_finished());
    let frozen = source
        .frozen_terminal()?
        .context("original terminal body was lost")?;
    let original_id = frozen.message.as_ref().unwrap().id.clone();
    assert_eq!(
        frozen.task.status,
        TaskStatus::Completed,
        "fallback must not replace original outcome by Failed"
    );
    assert!(original_terminal_receipt(&runtime, &source)?.is_none());
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .tasks()
            .latest(&task.id)?
            .unwrap()
            .status,
        TaskStatus::Running
    );
    assert!(runtime
        .inner
        .task_handles
        .lock()
        .await
        .get(&task.id)
        .is_some());
    assert!(
        runtime.inner.control_cancellations.drain().await.is_err(),
        "generic drain must retain pending durable responsibility"
    );
    runtime
        .inner
        .runtime_db
        .connection()?
        .execute_batch("DROP TRIGGER reject_original_command_receipt")?;
    runtime
        .continue_original_command_terminal(source.clone())
        .await?;
    let receipt = original_terminal_receipt(&runtime, &source)?.unwrap();
    use crate::runtime_db::{
        command_task_terminal::CommandTaskTerminalResult,
        input_mutation_receipts::InputMutationResult,
    };
    let InputMutationResult::CommandTaskTerminal { ack } = &receipt.outcome.result else {
        panic!("wrong receipt")
    };
    assert_eq!(
        ack.result,
        CommandTaskTerminalResult::Delivered {
            message_id: original_id
        }
    );
    assert!(source.is_durable_finished());
    assert!(source.frozen_terminal()?.is_none());
    runtime
        .inner
        .control_cancellations
        .drain_for_original_runtime(&runtime, false)
        .await?;
    Ok(())
}

fn set_original_command_deleting(
    runtime: &RuntimeHandle,
    task: &TaskRecord,
    mode: crate::types::AgentDeletionMode,
) -> Result<()> {
    runtime.inner.runtime_db.transaction(|tx| {
        let payload: String = tx.query_row(
            "SELECT payload_json FROM agent_identities WHERE agent_id=?1",
            [&task.agent_id],
            |r| r.get(0),
        )?;
        let mut identity: crate::types::AgentIdentityRecord = serde_json::from_str(&payload)?;
        let revision = identity.revision;
        identity.status = crate::types::AgentRegistryStatus::Deleting;
        identity.revision += 1;
        identity.updated_at = chrono::Utc::now();
        crate::runtime_db::evidence::upsert_agent_identity_tx(tx, &identity)?;
        crate::runtime_db::evidence::insert_agent_deletion_job_tx(
            tx,
            &crate::types::AgentDeletionJob {
                deletion_id: format!("original-command-delete-{}", task.id),
                agent_id: task.agent_id.clone(),
                mode,
                status: crate::types::AgentDeletionStatus::Pending,
                phase: crate::types::AgentDeletionPhase::Fence,
                requested_by: "original-command-fixture".into(),
                expected_identity_revision: revision,
                cascade_private_children: false,
                attempts: 0,
                last_error: None,
                next_attempt_at: None,
                created_at: identity.updated_at,
                updated_at: identity.updated_at,
                completed_at: None,
            },
        )
    })
}

#[tokio::test]
async fn original_command_operator_deleting_no_rejoin_delivers_actual_message_and_queue_once(
) -> Result<()> {
    let (_home, _workspace, runtime, task, process, lease, source) =
        accepted_original_producer_fixture().await?;
    set_original_command_deleting(&runtime, &task, crate::types::AgentDeletionMode::Delete)?;
    *process.status.lock().await = Some(RunningProcessExitStatus::new(Some(0), None));
    await_original_durable_terminal(&source).await?;
    lease.join().await?;
    let receipt = original_terminal_receipt(&runtime, &source)?.unwrap();
    use crate::runtime_db::{
        command_task_terminal::CommandTaskTerminalResult,
        input_mutation_receipts::InputMutationResult,
    };
    let InputMutationResult::CommandTaskTerminal { ack } = &receipt.outcome.result else {
        panic!("wrong original producer outcome")
    };
    let CommandTaskTerminalResult::Delivered { message_id } = &ack.result else {
        panic!("accepted terminal Message was lost")
    };
    runtime.inner.runtime_db.transaction(|tx| {
        assert_eq!(tx.query_row("SELECT COUNT(*) FROM messages WHERE evidence_id=?1",[message_id],|r|r.get::<_,usize>(0))?,1);
        assert_eq!(tx.query_row("SELECT status FROM queue_entries WHERE message_id=?1",[message_id],|r|r.get::<_,String>(0))?,"queued");
        assert_eq!(tx.query_row("SELECT COUNT(*) FROM task_result_settlements WHERE task_id=?1",[&task.id],|r|r.get::<_,usize>(0))?,0);
        assert!(tx.query_row("SELECT EXISTS(SELECT 1 FROM input_evidence_current WHERE agent_id=?1 AND identity_incarnation=?2 AND family='message' AND record_id=?3)",rusqlite::params![task.agent_id,source.address().scope.identity_incarnation,message_id],|r|r.get::<_,bool>(0))?);
        Ok(())
    })?;
    runtime
        .continue_original_command_terminal(source.clone())
        .await?;
    assert_eq!(
        original_terminal_receipt(&runtime, &source)?.unwrap(),
        receipt
    );
    Ok(())
}

#[tokio::test]
async fn original_command_parent_cleanup_keeps_original_terminal_responsibility_after_rollback(
) -> Result<()> {
    let (_home, _workspace, runtime, task, process, lease, source) =
        accepted_original_producer_fixture().await?;
    set_original_command_deleting(
        &runtime,
        &task,
        crate::types::AgentDeletionMode::ParentCleanup,
    )?;
    *process.status.lock().await = Some(RunningProcessExitStatus::new(Some(0), None));
    lease.join().await?;
    assert!(!source.is_durable_finished());
    let frozen = source
        .frozen_terminal()?
        .context("fenced original terminal was discarded")?;
    assert_eq!(frozen.task.status, TaskStatus::Completed);
    assert!(original_terminal_receipt(&runtime, &source)?.is_none());
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .tasks()
            .latest(&task.id)?
            .unwrap()
            .status,
        TaskStatus::Running
    );
    assert_eq!(
        runtime.inner.runtime_db.connection()?.query_row(
            "SELECT COUNT(*) FROM messages WHERE task_id=?1",
            [&task.id],
            |r| r.get::<_, usize>(0)
        )?,
        0
    );
    let error = runtime
        .continue_original_command_terminal(source.clone())
        .await
        .err()
        .context("ParentCleanup unexpectedly admitted terminal Message")?;
    assert!(
        format!("{error:#}").contains("agent_cleanup_fenced"),
        "{error:#}"
    );
    assert!(runtime
        .inner
        .task_handles
        .lock()
        .await
        .get(&task.id)
        .is_some());
    // The fixture ends its deletion fence; retry still uses the original body.
    runtime.inner.runtime_db.transaction(|tx| {
        tx.execute(
            "DELETE FROM agent_deletion_jobs WHERE agent_id=?1",
            [&task.agent_id],
        )?;
        Ok(())
    })?;
    runtime
        .continue_original_command_terminal(source.clone())
        .await?;
    assert!(source.is_durable_finished());
    Ok(())
}

#[tokio::test]
async fn original_command_lost_ack_replays_exact_terminal_without_a_second_result() -> Result<()> {
    let (_home, _workspace, runtime, task, process, lease, source) =
        accepted_original_producer_fixture().await?;
    runtime.inject_next_transition_fault(
        crate::runtime_db::transitions::TransitionFaultPoint::BeforeCacheUpdate,
    );
    *process.status.lock().await = Some(RunningProcessExitStatus::new(Some(0), None));
    await_original_durable_terminal(&source).await?;
    lease.join().await?;
    let receipt = original_terminal_receipt(&runtime, &source)?.unwrap();
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .tasks()
            .latest(&task.id)?
            .unwrap()
            .status,
        TaskStatus::Completed
    );
    assert_eq!(
        runtime.inner.runtime_db.connection()?.query_row(
            "SELECT COUNT(*) FROM messages WHERE task_id=?1",
            [&task.id],
            |r| r.get::<_, usize>(0)
        )?,
        1
    );
    assert_eq!(
        runtime.inner.runtime_db.connection()?.query_row(
            "SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce=?1",
            [format!("command-terminal:{}", source.address().lease_id)],
            |r| r.get::<_, usize>(0)
        )?,
        1
    );
    assert!(source.frozen_terminal()?.is_none());
    runtime
        .continue_original_command_terminal(source.clone())
        .await?;
    assert_eq!(
        original_terminal_receipt(&runtime, &source)?.unwrap(),
        receipt
    );
    Ok(())
}

#[tokio::test]
async fn original_command_unknown_keeps_same_capsule_until_fresh_state_recovery_succeeds(
) -> Result<()> {
    let (_home, _workspace, runtime, task, process, lease, source) =
        accepted_original_producer_fixture().await?;
    runtime.inner.runtime_db.connection()?.execute_batch("CREATE TABLE original_command_state_backup(payload_json TEXT NOT NULL); CREATE TRIGGER poison_original_command_fresh_state AFTER INSERT ON input_mutation_receipts WHEN NEW.command_nonce LIKE 'command-terminal:%' BEGIN INSERT INTO original_command_state_backup SELECT payload_json FROM agent_states; UPDATE agent_states SET payload_json='{invalid fresh original State'; END;")?;
    *process.status.lock().await = Some(RunningProcessExitStatus::new(Some(0), None));
    lease.join().await?;
    let frozen = source
        .frozen_terminal()?
        .context("Unknown lost the original terminal capsule")?;
    let message_id = frozen.message.as_ref().unwrap().id.clone();
    let receipt = original_terminal_receipt(&runtime, &source)?
        .context("actual committed whole receipt missing")?;
    assert!(!source.is_durable_finished());
    assert!(
        runtime.inner.mutation_owner.capture().is_err(),
        "Unknown must keep ordinary capture closed"
    );
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .tasks()
            .latest(&task.id)?
            .unwrap()
            .status,
        TaskStatus::Completed
    );
    assert_eq!(
        runtime.inner.runtime_db.connection()?.query_row(
            "SELECT COUNT(*) FROM messages WHERE evidence_id=?1",
            [&message_id],
            |r| r.get::<_, usize>(0)
        )?,
        1
    );
    runtime.inner.runtime_db.connection()?.execute_batch("DROP TRIGGER poison_original_command_fresh_state; UPDATE agent_states SET payload_json=(SELECT payload_json FROM original_command_state_backup LIMIT 1);")?;
    runtime
        .continue_original_command_terminal(source.clone())
        .await?;
    assert!(source.is_durable_finished());
    assert!(source.frozen_terminal()?.is_none());
    assert_eq!(
        original_terminal_receipt(&runtime, &source)?.unwrap(),
        receipt
    );
    assert_eq!(
        runtime.inner.runtime_db.connection()?.query_row(
            "SELECT COUNT(*) FROM messages WHERE evidence_id=?1",
            [&message_id],
            |r| r.get::<_, usize>(0)
        )?,
        1
    );
    Ok(())
}

#[tokio::test]
async fn original_command_foreign_generation_and_lease_cannot_mint_a_terminal_source() -> Result<()>
{
    let (_home, _workspace, runtime, task, process, lease, source) =
        accepted_original_producer_fixture().await?;
    let mut terminal = task.clone();
    terminal.status = TaskStatus::Completed;
    terminal.updated_at = chrono::Utc::now();
    let mut foreign = terminal.clone();
    foreign.agent_id = "foreign-command-owner".into();
    assert!(source
        .freeze_terminal(&foreign, None, &runtime.inner.input_evidence_budget, None)
        .is_err());
    let mut generation = terminal.clone();
    generation.created_at += chrono::Duration::milliseconds(1);
    assert!(source
        .freeze_terminal(
            &generation,
            None,
            &runtime.inner.input_evidence_budget,
            None
        )
        .is_err());
    let mut registration = runtime
        .admit_control_task_handle_registration(&task)
        .await?;
    let mut request = crate::runtime::resident_task::TaskCaptureRequest::current();
    let key = crate::runtime::resident_task::TaskKey::new(
        crate::runtime::resident_task::TaskFamily::Task,
        &task.id,
    );
    request.required_keys.insert(key.clone());
    registration.prepare_owner_task_request(&request).await?;
    let capture = registration.capture_tasks(&request)?;
    let selected = capture.lookup(&key)?.unwrap();
    let (cancel, _) = oneshot::channel();
    let (force, _) = oneshot::channel();
    let mut other = task.clone();
    other.id = "another-original-command-lease".into();
    let wrong = runtime.inner.control_cancellations.command(
        &other,
        super::super::control_cancellation::SharedControlSignal::new(cancel),
        super::super::control_cancellation::SharedControlSignal::new(force),
    );
    assert!(OriginalCommandTaskProducer::admitted(
        &registration,
        selected,
        &task,
        &wrong,
        runtime.inner.mutation_owner.owner_epoch(),
        &runtime.inner.input_evidence_budget
    )
    .is_err());
    runtime
        .inner
        .control_cancellations
        .abandon_unattached_command(&wrong)?;
    drop((capture, registration));
    *process.status.lock().await = Some(RunningProcessExitStatus::new(Some(0), None));
    await_original_durable_terminal(&source).await?;
    lease.join().await?;
    Ok(())
}

#[tokio::test]
async fn original_command_identity_material_precedes_bad_json_and_owns_only_its_view() -> Result<()>
{
    let (_home, _workspace, runtime, task, process, lease, source) =
        accepted_original_producer_fixture().await?;
    let ledger =
        crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(Default::default())?;
    let request = crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
        query: Default::default(),
        capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
        record_limit: 65536,
        byte_limit: 4096,
    };
    let raw: String = runtime.inner.runtime_db.connection()?.query_row(
        "SELECT payload_json FROM agent_identities WHERE agent_id=?1",
        [&task.agent_id],
        |r| r.get(0),
    )?;
    // The identity decoder's malformed JSON would otherwise run first.
    runtime.inner.runtime_db.connection()?.execute(
        "UPDATE agent_identities SET payload_json=?1 WHERE agent_id=?2",
        rusqlite::params!["{".repeat(8192), task.agent_id],
    )?;
    let error = runtime
        .inner
        .runtime_db
        .transaction(|tx| {
            crate::runtime_db::command_task_terminal::observe_original_owner_tx(
                tx,
                &source.address().scope,
                &request,
                &ledger,
            )
        })
        .err()
        .context("oversize bad identity accepted")?;
    assert!(
        matches!(
            error.downcast_ref::<crate::runtime_db::task_snapshot::TaskSnapshotError>(),
            Some(crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    runtime.inner.runtime_db.connection()?.execute(
        "UPDATE agent_identities SET payload_json=?1 WHERE agent_id=?2",
        rusqlite::params![raw, task.agent_id],
    )?;
    let mut request = request;
    request.byte_limit = 1024 * 1024;
    let observation = runtime.inner.runtime_db.transaction(|tx| {
        crate::runtime_db::command_task_terminal::observe_original_owner_tx(
            tx,
            &source.address().scope,
            &request,
            &ledger,
        )
    })?;
    assert!(ledger.report()?.observed_bytes > 0);
    drop(observation);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    *process.status.lock().await = Some(RunningProcessExitStatus::new(Some(0), None));
    await_original_durable_terminal(&source).await?;
    lease.join().await?;
    Ok(())
}

#[tokio::test]
async fn original_command_started_parent_fence_survives_a_new_result_turn_hint() -> Result<()> {
    let (_home, _workspace, runtime) = test_runtime();
    let source = crate::types::MessageEnvelope::new(
        "default",
        crate::types::MessageKind::OperatorPrompt,
        crate::types::MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        crate::types::Priority::Normal,
        crate::types::MessageBody::Text {
            text: "original parent dispatch".into(),
        },
    );
    let admitted = runtime.enqueue(source).await?;
    let super::super::scheduler_executor::RunLoopPoll::Message(scheduled) =
        super::super::scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
            .poll()
            .await?
    else {
        anyhow::bail!("original parent source must be actually claimed")
    };
    assert_eq!(scheduled.message.id, admitted.id);
    runtime
        .begin_dispatch_turn(
            &scheduled.message,
            None,
            None,
            scheduled
                .dispatch_plan
                .execution_admission_provenance
                .clone(),
            &scheduled.dispatch_plan.prior_closure,
        )
        .await?;
    let original_parent = runtime.agent_state().await?.current_turn_id.unwrap();
    let process = FakeRunningProcess::pending();
    let resolved = resolved_command(&runtime, &command_spec(false, false)).await;
    let task = runtime
        .register_command_task(
            "original parent command".into(),
            resolved,
            running_command(process.clone(), "original result", ""),
            AuthorityClass::OperatorInstruction,
            false,
            CapturedOutput::default(),
        )
        .await?;
    let fence = task.rejoin_fence().map_err(anyhow::Error::msg)?;
    assert_eq!(fence.parent_turn_id, original_parent);
    wait_for_latest_task(&runtime, &task.id, TaskStatus::Running).await;
    let lease = runtime
        .inner
        .task_handles
        .lock()
        .await
        .get(&task.id)
        .and_then(ManagedTaskHandle::control_lease)
        .unwrap();
    let producer = lease.original_command()?.unwrap();
    *process.status.lock().await = Some(RunningProcessExitStatus::new(Some(0), None));
    await_original_durable_terminal(&producer).await?;
    lease.join().await?;
    let actual = runtime.inner.runtime_db.tasks().latest(&task.id)?.unwrap();
    assert_eq!(actual.status, TaskStatus::Completed);
    assert_eq!(actual.rejoin_fence().map_err(anyhow::Error::msg)?, fence);
    let receipt = original_terminal_receipt(&runtime, &producer)?.unwrap();
    let crate::runtime_db::input_mutation_receipts::InputMutationResult::CommandTaskTerminal {
        ack,
    } = &receipt.outcome.result
    else {
        panic!("expected original Command terminal ACK")
    };
    let crate::runtime_db::command_task_terminal::CommandTaskTerminalResult::Delivered {
        message_id,
    } = &ack.result
    else {
        panic!("original terminal Result must be delivered")
    };
    let result = runtime.inner.runtime_db.transaction(|tx| {
        crate::runtime_db::evidence::committed_message_tx(tx, &task.agent_id, message_id)
    })?;
    assert_ne!(result.turn_id.as_deref(), Some(original_parent.as_str()));
    assert_eq!(
        result.metadata.as_ref().unwrap()["task_detail"]["parent_turn_id"],
        original_parent
    );
    assert_eq!(
        runtime.inner.runtime_db.connection()?.query_row(
            "SELECT COUNT(*) FROM task_result_settlements WHERE task_id=?1 AND parent_turn_id=?2",
            rusqlite::params![task.id, original_parent],
            |row| row.get::<_, usize>(0),
        )?,
        1
    );
    crate::runtime::tests::support::finish_claimed_test_run(&runtime).await;
    let terminal = crate::runtime::tests::support::terminal_transition_from_started_turn(
        &runtime,
        &scheduled.message,
    );
    runtime
        .commit_queue_terminal_settlement(
            crate::types::QueueEntryRecord {
                message_id: scheduled.message.id.clone(),
                agent_id: scheduled.message.agent_id.clone(),
                priority: scheduled.message.priority.clone(),
                status: crate::types::QueueEntryStatus::Processed,
                created_at: scheduled.message.created_at,
                updated_at: chrono::Utc::now(),
            },
            Vec::new(),
            true,
            Some(&terminal),
        )
        .await?;
    drop(scheduled);
    Ok(())
}
