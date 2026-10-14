use super::*;
use crate::runtime::{resident_task::TaskCaptureRequest, RuntimeHandle};
use crate::types::{
    AuthorityClass, ChildAgentWorkspaceMode, MessageBody, Priority, QueueEntryStatus, TaskStatus,
};
use anyhow::Context;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

fn queued_running(runtime: &RuntimeHandle, task: &str) -> Result<Vec<MessageEnvelope>> {
    let connection = runtime.runtime_db().connection()?;
    let mut statement = connection.prepare(
        "SELECT m.payload_json FROM messages m
        JOIN queue_entries q ON q.message_id=m.evidence_id
        WHERE m.kind='task_status' AND json_extract(m.payload_json,'$.task_id')=?1
        AND q.status='queued' ORDER BY m.message_seq,m.created_at,m.message_id",
    )?;
    let rows = statement.query_map([task], |row| row.get::<_, String>(0))?;
    rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
}
async fn wait_queued_running(
    runtime: &RuntimeHandle,
    task: &str,
    count: usize,
) -> Result<Vec<MessageEnvelope>> {
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let rows = queued_running(runtime, task)?;
            if rows.len() == count {
                return Ok(rows);
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    match result {
        Ok(result) => result,
        Err(error) => Err(error).with_context(|| {
            format!(
                "actual monitor Running QueueAdmit did not settle: {}",
                clock_monitor_diagnostic(runtime, task)
            )
        }),
    }
}
fn clock_monitor_diagnostic(runtime: &RuntimeHandle, task: &str) -> String {
    let source = runtime
        .inner
        .task_handles
        .try_lock()
        .ok()
        .and_then(|handles| {
            handles
                .get(task)
                .and_then(crate::runtime::command_task::ManagedTaskHandle::control_lease)
        })
        .and_then(|lease| lease.original_child().ok().flatten());
    let source=source.map(|source|serde_json::json!({
        "failure":source.clock_monitor_failure_for_test(),
        "binding":source.monitor_binding_confirmed(),
        "finished":source.is_durable_finished(),
        "stage":source.monitor_stage().map(|stage|serde_json::json!({"task":stage.task,"child":stage.child,"detail":stage.detail})),
        "observed":source.observed_terminal().is_some(),
    }));
    format!(
        "bootstrap={:?}; source={source:?}; task={:?}; events={:?}",
        runtime.inner.bootstrap_result.lock().unwrap(),
        runtime.runtime_db().tasks().latest(task),
        runtime.inner.storage.read_recent_events(12)
    )
}
fn clock_input_projection(path: &std::path::Path, id: &str) -> String {
    let read = || -> Result<serde_json::Value> {
        let db = rusqlite::Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let source:(Option<String>,Option<i64>,String,i64,Option<String>,String)=db.query_row("SELECT m.content_hash,m.message_seq,m.created_at,length(CAST(m.payload_json AS BLOB)),(SELECT a.turn_id FROM conversation_input_assignments a WHERE a.agent_id=m.agent_id AND a.message_id=m.evidence_id),m.payload_json FROM messages m WHERE m.evidence_id=?1",[id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)))?;
        let mut query=db.prepare("SELECT capture_phase,fingerprint_json FROM input_evidence_seed_membership WHERE family='message' AND record_id=?1 ORDER BY seed_id,capture_phase")?;
        let seed = query
            .query_map([id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut query=db.prepare("SELECT qualification,fingerprint_json FROM input_evidence_current WHERE family='message' AND record_id=?1")?;
        let current = query
            .query_map([id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut query=db.prepare("SELECT domain,import_status,canonical_source FROM storage_domains WHERE domain IN ('turn_records','evidence','messages','context_episode_anchors') ORDER BY domain")?;
        let domains = query
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(serde_json::json!({"source":source,"seed":seed,"current":current,"domains":domains}))
    };
    match read() {
        Ok(value) => value.to_string(),
        Err(error) => format!("projection observation failed: {error:#}"),
    }
}
// enqueue returns its normalized admission DTO; the writer allocates the
// canonical sequence in the actual qualified Input fact.
async fn actual_accepted_running_message(
    runtime: &RuntimeHandle,
    id: &str,
) -> Result<MessageEnvelope> {
    let owner = runtime.begin_mutation().await?;
    let inputs = owner.capture_input_evidence()?;
    let key = InputEvidenceKey::new(InputEvidenceFamily::Message, id);
    let qualified = inputs
        .lookup(&key)?
        .context("actual accepted Running Input absent")?;
    let PreparedInputEvidence::Message(message) = &qualified.record else {
        anyhow::bail!("actual Running Input has another family");
    };
    assert_eq!(message.id, id);
    assert!(message.message_seq.is_some_and(|seq| seq > 0));
    Ok(message.clone())
}
async fn wait_terminal(runtime: &RuntimeHandle, task: &str) -> Result<TaskRecord> {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let read = runtime.capture_current_task_record(task)?;
            let record = read.record().context("actual monitor Task vanished")?;
            if crate::runtime::task_state_reducer::is_terminal_task_status(&record.status) {
                return Ok(record.clone());
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .context("actual child monitor did not commit its terminal whole ACK")?
}
fn original_child_prompt(child: &str) -> MessageEnvelope {
    MessageEnvelope::new(
        child,
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "complete this actual child invocation".into(),
        },
    )
}

// Keep the actual cut's profile/configuration files with its DB. Do not
// follow external links or recreate identities while restoring this fixture.
fn copy_clock_agent_home_tree(from: &std::path::Path, to: &std::path::Path) -> Result<usize> {
    std::fs::create_dir_all(to)?;
    let mut files = 0usize;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            files = files
                .checked_add(copy_clock_agent_home_tree(&source, &target)?)
                .context("clock AgentHome file count overflow")?;
        } else {
            anyhow::ensure!(
                kind.is_file(),
                "clock AgentHome cut contains a non-regular entry: {}",
                source.display()
            );
            std::fs::copy(&source, &target)?;
            files = files
                .checked_add(1)
                .context("clock AgentHome file count overflow")?;
        }
    }
    Ok(files)
}

#[tokio::test]
async fn accepted_old_running_after_real_restart_preserves_new_original_monitor_birth() -> Result<()>
{
    actual_restart_clock().await
}
fn actual_restart_clock() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>>
{
    Box::pin(async {
        let home = tempfile::tempdir()?;
        std::fs::write(
            home.path().join("config.json"),
            r#"{"model":{"default":"openai/gpt-5.4"}}"#,
        )?;
        let mut config = crate::config::AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
        config.default_agent_id = "default".into();
        config.user_home_dir = Some(home.path().to_path_buf());
        let old_host = crate::host::RuntimeHost::new_with_provider(
            config.clone(),
            Arc::new(crate::provider::StubProvider::new("actual child completed")),
        )?;
        let old_gate = Arc::new(Semaphore::new(0));
        old_host.set_clock_recovery_dispatch_gate_for_test("default", old_gate.clone());
        let parent = old_host.default_runtime().await?;
        let target = old_host
            .create_named_agent("clock-restart-target", None)
            .await?;
        let child = old_host.get_public_agent(&target.agent_id).await?;
        assert_eq!(parent.agent_state().await?.current_turn_id, None);
        let task = parent
            .create_agent_invocation_task(
                "real restart monitor".into(),
                "complete this actual child invocation".into(),
                AuthorityClass::OperatorInstruction,
                Some(target.agent_id.clone()),
                false,
                ChildAgentWorkspaceMode::Inherit,
            )
            .await?;
        let born = task.created_at;
        assert_eq!(
            crate::runtime::tasks::unbound_task_rejoin_generation(&task),
            Some(1)
        );
        assert!(
            task.rejoin_fence().is_err(),
            "actual creator has no parent Turn yet"
        );
        let mut detail = task.detail.clone().unwrap();
        detail["target_agent_id"] = serde_json::json!(target.agent_id);
        detail["target_identity_incarnation"] = serde_json::json!(target.incarnation);
        detail["child_turn_baseline"] = serde_json::json!(0);
        parent
            .start_agent_invocation_monitor(
                task.clone(),
                AuthorityClass::OperatorInstruction,
                false,
                target.agent_id.clone(),
                0,
                None,
                detail,
            )
            .await?;
        let old_running = wait_queued_running(&parent, &task.id, 1).await?.remove(0);
        assert!(old_running.message_seq.is_some_and(|seq| seq > 0));
        assert_eq!(
            parent
                .inner
                .control_cancellations
                .original_child_running_status_clock(&old_running)?,
            Some(born)
        );
        assert_eq!(
            parent
                .runtime_db()
                .queue_entries()
                .latest(&old_running.id)?
                .unwrap()
                .status,
            QueueEntryStatus::Queued
        );
        assert_eq!(parent.agent_state().await?.current_run_id, None);
        let starts: usize = parent.runtime_db().connection()?.query_row(
            "SELECT COUNT(*) FROM dispatch_memory_checkpoints WHERE source_message_id=?1",
            [&old_running.id],
            |row| row.get(0),
        )?;
        assert_eq!(starts, 0, "the durable cut is before any Claim/Start");
        let db_path = old_host.runtime_db().path().to_path_buf();
        // VACUUM INTO produces an integrity-checked, consistent actual DB cut,
        // including all Queue/Input/Task whole ACKs, never reconstructed DTOs.
        let cut = old_host
            .runtime_db()
            .create_verified_backup("running-clock-crash-cut")
            .context("create and integrity-check the actual Running crash cut")?;
        // The target is reusable only with its actual AgentHome as well as its
        // Active identity. Save the same cut before either old Runtime resumes.
        let agent_homes_cut = tempfile::tempdir()?;
        assert!(config.agent_root_dir().join(&target.agent_id).is_dir());
        let original_home_files =
            copy_clock_agent_home_tree(&config.agent_root_dir(), agent_homes_cut.path())?;
        assert!(original_home_files > 0);
        old_gate.add_permits(1024);
        child
            .enqueue(original_child_prompt(&target.agent_id))
            .await?;
        assert_eq!(
            wait_terminal(&parent, &task.id).await?.status,
            TaskStatus::Completed
        );
        old_host.shutdown().await?;
        drop(child);
        drop(parent);
        drop(old_host);
        // The verified image is restored to a distinct inode/home. Even a
        // retained closed writer connection cannot replay its old WAL here.
        let recovered_home = tempfile::tempdir()?;
        std::fs::write(
            recovered_home.path().join("config.json"),
            r#"{"model":{"default":"openai/gpt-5.4"}}"#,
        )?;
        config.data_dir = recovered_home.path().join(".holon");
        config.user_home_dir = Some(recovered_home.path().to_path_buf());
        let restored_home_files =
            copy_clock_agent_home_tree(agent_homes_cut.path(), &config.agent_root_dir())?;
        assert_eq!(restored_home_files, original_home_files);
        assert!(config.agent_root_dir().join(&target.agent_id).is_dir());
        let restored_path = config.runtime_db_path();
        assert_ne!(restored_path, db_path);
        std::fs::create_dir_all(restored_path.parent().unwrap())?;
        let copied = std::fs::copy(&cut, &restored_path)
            .context("restore the verified Running cut into a fresh database path")?;
        assert_eq!(copied, std::fs::metadata(&cut)?.len());
        let host = crate::host::RuntimeHost::new_with_provider(
            config,
            Arc::new(crate::provider::StubProvider::new(
                "actual restarted child completed",
            )),
        )
        .context("open the restored fresh-home Running cut")?;
        let gate = Arc::new(Semaphore::new(0));
        host.set_clock_recovery_dispatch_gate_for_test("default", gate.clone());
        assert!(
            host.bridge()
                .reusable_agent_exists(&target.agent_id)
                .await?,
            "actual restored target must satisfy the original reuse predicate"
        );
        let actual_target = host
            .agent_identity_record(&target.agent_id)?
            .context("original cut target identity absent")?;
        assert_eq!(actual_target.incarnation, target.incarnation);
        assert_eq!(
            actual_target.status,
            crate::types::AgentRegistryStatus::Active
        );
        host.recover_orphaned_queue_claims_at_startup().await?;
        let parent = host.default_runtime().await?;
        parent.wait_for_bootstrap().await?;
        let statuses = wait_queued_running(&parent, &task.id, 2).await?;
        let old = statuses
            .iter()
            .find(|message| message.id == old_running.id)
            .unwrap();
        let new = statuses
            .iter()
            .find(|message| message.id != old_running.id)
            .unwrap();
        assert_eq!(
            old, &old_running,
            "actual accepted old input survives the restart cut"
        );
        assert_eq!(
            parent
                .inner
                .control_cancellations
                .original_child_running_status_clock(old)?,
            None
        );
        assert_eq!(
            parent
                .inner
                .control_cancellations
                .original_child_running_status_clock(new)?,
            Some(born),
            "only the new original monitor has this transient seal"
        );
        let new_running_id = new.id.clone();
        {
            let owner = parent.begin_mutation().await?;
            let tasks = owner.capture_tasks(&TaskCaptureRequest::current())?;
            let inputs = owner.capture_input_evidence()?;
            let parsed = crate::runtime::tasks::task_from_message(old, "default")?;
            assert_ne!(
                parsed.created_at, born,
                "generic parser alone cannot supply original birth"
            );
            let proof = qualify(&inputs, &tasks, old, &parsed, None)?.unwrap();
            assert_eq!(proof.created_at(), born);
            let mut preserved = parsed.clone();
            proof.preserve_rejoin_shape(&mut preserved);
            assert_eq!(
                crate::runtime::tasks::unbound_task_rejoin_generation(&preserved),
                Some(1)
            );
            for case in [
                "foreign_owner",
                "different_kind",
                "different_work",
                "different_recovery",
                "different_generation",
                "different_parent",
            ] {
                let mut changed = parsed.clone();
                match case {
                    "foreign_owner" => changed.agent_id = "foreign-parent".into(),
                    "different_kind" => changed.kind = TaskKind::ChildAgentTask,
                    "different_work" => changed.work_item_id = Some("foreign-work".into()),
                    "different_recovery" => changed.recovery = None,
                    "different_generation" => {
                        changed.detail.as_mut().unwrap()["rejoin_generation"] = serde_json::json!(2)
                    }
                    "different_parent" => {
                        changed.detail.as_mut().unwrap()["parent_turn_id"] =
                            serde_json::json!("foreign-parent-turn")
                    }
                    _ => unreachable!(),
                }
                assert!(
                    qualify(&inputs, &tasks, old, &changed, None).is_err(),
                    "{case}"
                );
            }
            let mut pruned = owner.capture_input_evidence()?;
            let old_key = InputEvidenceKey::new(InputEvidenceFamily::Message, &old.id);
            pruned
                .records
                .retain(|record| record.locator.key != old_key);
            pruned.unavailable.insert(old_key);
            assert!(
                qualify(&pruned, &tasks, old, &parsed, None).is_err(),
                "unavailable accepted source cannot grant a clock"
            );
            // Neither an arbitrary timestamp/header nor a foreign address can
            // borrow the exact accepted Message locator to mint a clock.
            let mut foreign = old.clone();
            foreign.metadata.as_mut().unwrap()["task_created_at"] =
                serde_json::json!(born - chrono::Duration::days(20));
            assert!(qualify(&inputs, &tasks, &foreign, &parsed, None).is_err());
            foreign.id = crate::ids::message_id();
            assert!(qualify(&inputs, &tasks, &foreign, &parsed, None).is_err());
            let mut incomplete = owner.capture_tasks(&TaskCaptureRequest::current())?;
            incomplete.coverage.active_complete = false;
            assert!(matches!(
                qualify(&inputs, &incomplete, old, &parsed, None)
                    .err()
                    .unwrap()
                    .downcast_ref::<TaskReadError>(),
                Some(TaskReadError::IncompleteActive)
            ));
        }
        gate.add_permits(1);
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if parent
                    .runtime_db()
                    .queue_entries()
                    .latest(&old_running.id)?
                    .unwrap()
                    .status
                    == QueueEntryStatus::Processed
                {
                    return Ok::<(), anyhow::Error>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .context("old Running did not take the actual Start/common Finish path")??;
        let current = parent.capture_current_task_record(&task.id)?;
        let current_task = current.record().unwrap();
        assert_eq!(current_task.created_at, born);
        assert_eq!(current_task.recovery, task.recovery);
        assert_eq!(current_task.work_item_id, task.work_item_id);
        assert_eq!(current_task.rejoin_fence().ok(), task.rejoin_fence().ok());
        drop(current);
        assert_eq!(
            parent
                .runtime_db()
                .queue_entries()
                .latest(&new_running_id)?
                .unwrap()
                .status,
            QueueEntryStatus::Queued
        );
        // The newly recovered original producer reports a real child Turn
        // before its own new Running status gets any processing permit.
        let child = host.get_public_agent(&target.agent_id).await?;
        child
            .enqueue(original_child_prompt(&target.agent_id))
            .await?;
        let terminal = wait_terminal(&parent, &task.id).await?;
        assert_eq!(terminal.status, TaskStatus::Completed);
        assert_eq!(terminal.created_at, born);
        assert_eq!(
            parent
                .runtime_db()
                .queue_entries()
                .latest(&new_running_id)?
                .unwrap()
                .status,
            QueueEntryStatus::Queued
        );
        let child_receipts: usize = parent.runtime_db().connection()?.query_row(
            "SELECT COUNT(*) FROM input_mutation_receipts WHERE family='task'
             AND json_extract(outcome_json,'$.result.kind')='child_task_terminal'
             AND json_extract(outcome_json,'$.result.ack.original.admitted.key.id')=?1",
            [&task.id],
            |row| row.get(0),
        )?;
        assert_eq!(
            child_receipts, 1,
            "actual recovered source commits one whole terminal ACK"
        );
        parent.runtime_db().transaction(|tx| {
            let raw: String = tx.query_row("SELECT prepared_json FROM input_mutation_receipts
                WHERE family='task' AND json_extract(outcome_json,'$.result.kind')='child_task_terminal'
                AND json_extract(outcome_json,'$.result.ack.original.admitted.key.id')=?1", [&task.id], |row| row.get(0))?;
            let receipt = crate::runtime_db::input_mutation_receipts::lookup_input_mutation_receipt_tx(tx, &serde_json::from_str(&raw)?)?
                .context("actual recovered monitor whole ACK missing")?;
            assert!(matches!(receipt.outcome.result,
                crate::runtime_db::input_mutation_receipts::InputMutationResult::ChildTaskTerminal { .. }));
            Ok(())
        })?;
        gate.add_permits(1024);
        host.shutdown().await?;
        Ok(())
    })
}

#[tokio::test]
async fn actual_message_wait_running_preserves_original_control_lease_through_public_stop(
) -> Result<()> {
    wait_stop_birth().await
}
fn wait_stop_birth() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>> {
    Box::pin(async {
        struct PendingTarget;
        #[async_trait::async_trait]
        impl crate::provider::AgentProvider for PendingTarget {
            async fn complete_turn(
                &self,
                _request: crate::provider::ProviderTurnRequest,
            ) -> Result<crate::provider::ProviderTurnResponse> {
                std::future::pending().await
            }
        }
        let home = tempfile::tempdir()?;
        std::fs::write(
            home.path().join("config.json"),
            r#"{"model":{"default":"openai/gpt-5.4"}}"#,
        )?;
        let mut config = crate::config::AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
        config.default_agent_id = "default".into();
        config.user_home_dir = Some(home.path().to_path_buf());
        let host = crate::host::RuntimeHost::new_with_provider(config, Arc::new(PendingTarget))?;
        let gate = Arc::new(Semaphore::new(0));
        host.set_clock_recovery_dispatch_gate_for_test("default", gate.clone());
        let parent = host.default_runtime().await?;
        let target = host
            .create_named_agent("wait-original-clock-target", None)
            .await?;
        let receipt = parent
            .agent_invocation_service()
            .invoke(crate::types::InvokeAgentRequest {
                target: crate::types::InvokeAgentTarget::ExistingAgent {
                    agent_id: target.agent_id.clone(),
                },
                message: "actual message wait remains waiting until original cancellation".into(),
                authority_class: AuthorityClass::RuntimeInstruction,
            })
            .await?;
        let task_id = receipt.task_handle.task_id;
        let selected = parent.capture_current_task_record(&task_id)?;
        let original = selected
            .record()
            .context("actual Wait registration Task absent")?
            .clone();
        drop(selected);
        assert_eq!(original.kind, TaskKind::AgentMessageWait);
        let lease = parent
            .inner
            .task_handles
            .lock()
            .await
            .get(&task_id)
            .and_then(crate::runtime::command_task::ManagedTaskHandle::control_lease)
            .context("actual Wait monitor did not register its original Control lease")?;
        let original_generation = lease.domain_generation().clone();
        let running = wait_queued_running(&parent, &task_id, 1).await?.remove(0);
        assert_eq!(
            running.metadata.as_ref().unwrap()["task_created_at"],
            serde_json::to_value(original.created_at)?
        );
        gate.add_permits(1);
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if parent
                    .runtime_db()
                    .queue_entries()
                    .latest(&running.id)?
                    .unwrap()
                    .status
                    == QueueEntryStatus::Processed
                {
                    break Ok::<(), anyhow::Error>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .context("real Wait Running did not complete its Start/commonFinish ACK")??;
        let selected = parent.capture_current_task_record(&task_id)?;
        let current = selected.record().unwrap();
        assert_eq!(current.status, TaskStatus::Running);
        assert_eq!(current.created_at, original.created_at);
        assert_eq!(current.recovery, original.recovery);
        assert_eq!(
            crate::runtime::tasks::unbound_task_rejoin_generation(current),
            Some(1)
        );
        assert_eq!(lease.domain_generation(), &original_generation);
        drop(selected);
        let stopped = parent
            .stop_task(&task_id, &AuthorityClass::OperatorInstruction)
            .await?;
        assert_eq!(stopped.created_at, original.created_at);
        assert_eq!(stopped.status, TaskStatus::Cancelled);
        assert!(lease.cancellation_requested());
        tokio::time::timeout(Duration::from_secs(15), lease.join())
            .await
            .context("original Wait lease did not physically join")??;
        tokio::time::timeout(Duration::from_secs(15),async {
            loop {
                let joined:bool=parent.runtime_db().connection()?.query_row("SELECT EXISTS(SELECT 1 FROM task_stop_effects WHERE agent_id='default' AND task_id=?1 AND physical_state='monitor_joined')",[&task_id],|row|row.get(0))?;
                if joined { return Ok::<(),anyhow::Error>(()); }
                tokio::task::yield_now().await;
            }
        }).await.context("owned original Stop did not ACK real Wait monitor join")??;
        gate.add_permits(1024);
        host.shutdown().await?;
        Ok(())
    })
}

#[tokio::test]
async fn accepted_running_expected_birth_cannot_borrow_a_later_actual_task_generation() -> Result<()>
{
    use crate::runtime::tests::support::LifecycleHarness;
    use crate::types::{AdmissionContext, MessageDeliverySurface};
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let original = runtime
        .create_agent_invocation_task(
            "original native birth".into(),
            "actual original request".into(),
            AuthorityClass::RuntimeInstruction,
            None,
            false,
            ChildAgentWorkspaceMode::Inherit,
        )
        .await?;
    let mut message = MessageEnvelope::new(
        "default",
        MessageKind::TaskStatus,
        MessageOrigin::Task {
            task_id: original.id.clone(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Background,
        MessageBody::Text {
            text: "actual accepted native Running".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::TaskRejoin,
        AdmissionContext::RuntimeOwned,
    );
    message.task_id = Some(original.id.clone());
    message.work_item_id = original.work_item_id.clone();
    message.metadata = Some(
        serde_json::json!({"task_id":original.id,"task_created_at":original.created_at,"task_kind":original.kind,"task_status":"running","task_recovery":original.recovery,"task_detail":original.detail}),
    );
    let admitted = runtime.enqueue(message).await?;
    let accepted = actual_accepted_running_message(&runtime, &admitted.id).await?;
    {
        let owner = runtime.begin_mutation().await?;
        let tasks = owner.capture_tasks(&TaskCaptureRequest::current())?;
        let inputs = owner.capture_input_evidence()?;
        let parsed = crate::runtime::tasks::task_from_message(&accepted, "default")?;
        assert_eq!(
            qualify(&inputs, &tasks, &accepted, &parsed, None)?
                .unwrap()
                .created_at(),
            original.created_at
        );
    }
    for case in ["missing", "null", "malformed", "different_birth"] {
        let mut bad = accepted.clone();
        bad.id = crate::ids::runtime_id("unassociated_running");
        bad.message_seq = None;
        let metadata = bad.metadata.as_mut().unwrap().as_object_mut().unwrap();
        match case {
            "missing" => {
                metadata.remove("task_created_at");
            }
            "null" => {
                metadata.insert("task_created_at".into(), serde_json::Value::Null);
            }
            "malformed" => {
                metadata.insert("task_created_at".into(), serde_json::json!("not-a-time"));
            }
            "different_birth" => {
                metadata.insert(
                    "task_created_at".into(),
                    serde_json::json!(original.created_at + chrono::Duration::seconds(1)),
                );
            }
            _ => unreachable!(),
        }
        let admitted = runtime.enqueue(bad).await?;
        let actual = actual_accepted_running_message(&runtime, &admitted.id).await?;
        let owner = runtime.begin_mutation().await?;
        let tasks = owner.capture_tasks(&TaskCaptureRequest::current())?;
        let inputs = owner.capture_input_evidence()?;
        let parsed = crate::runtime::tasks::task_from_message(&actual, "default")?;
        let error = qualify(&inputs, &tasks, &actual, &parsed, None)
            .err()
            .with_context(|| format!("actual native {case} header acquired a generation"))?;
        assert!(
            matches!(
                error.downcast_ref::<TaskSnapshotError>(),
                Some(TaskSnapshotError::CanonicalChanged(_))
            ),
            "{case}: {error:#}"
        );
    }
    let mut changed = original.clone();
    changed.created_at += chrono::Duration::seconds(1);
    changed.updated_at += chrono::Duration::seconds(2);
    runtime
        .persist_task_transition(&changed, "actual_new_task_birth_ack")
        .await?;
    let owner = runtime.begin_mutation().await?;
    let tasks = owner.capture_tasks(&TaskCaptureRequest::current())?;
    let inputs = owner.capture_input_evidence()?;
    let parsed = crate::runtime::tasks::task_from_message(&accepted, "default")?;
    let error = qualify(&inputs, &tasks, &accepted, &parsed, None)
        .err()
        .context("old accepted Source borrowed new actual Task birth")?;
    assert!(matches!(
        error.downcast_ref::<TaskSnapshotError>(),
        Some(TaskSnapshotError::CanonicalChanged(_))
    ));
    let CanonicalTaskFact::Task(actual) = &tasks
        .lookup(&TaskKey::new(TaskFamily::Task, &original.id))?
        .unwrap()
        .fact
    else {
        unreachable!()
    };
    assert_eq!(actual.created_at, changed.created_at);
    assert_eq!(
        inputs
            .lookup(&InputEvidenceKey::new(
                InputEvidenceFamily::Message,
                &accepted.id
            ))?
            .unwrap()
            .record,
        PreparedInputEvidence::Message(accepted)
    );
    Ok(())
}

#[tokio::test]
async fn v81_no_header_pending_running_restarts_with_original_fixed_generation_and_real_child_ack(
) -> Result<()> {
    v81_running_restart().await
}
fn v81_running_restart() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>>
{
    Box::pin(async {
        use crate::runtime_db::{
            evidence,
            migrations::{apply_migration, ensure_migration_table, MIGRATIONS},
            repositories,
        };
        use crate::types::{
            AdmissionContext, AgentIdentityRecord, AgentKind, AgentLifecycleDisposition,
            AgentOwnership, AgentProfilePreset, AgentState, AgentVisibility,
            MessageDeliverySurface, QueueEntryRecord, TaskRecoverySpec,
        };
        let home = tempfile::tempdir()?;
        std::fs::write(
            home.path().join("config.json"),
            r#"{"model":{"default":"openai/gpt-5.4"}}"#,
        )?;
        let mut config = crate::config::AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
        config.default_agent_id = "default".into();
        config.user_home_dir = Some(home.path().to_path_buf());
        let (born, child_id, task, running) = {
            std::fs::create_dir_all(config.runtime_db_path().parent().unwrap())?;
            let mut connection = rusqlite::Connection::open(config.runtime_db_path())?;
            ensure_migration_table(&connection)?;
            for migration in MIGRATIONS
                .iter()
                .filter(|migration| migration.version <= 81)
            {
                apply_migration(&mut connection, migration)?;
            }
            let born: DateTime<Utc> = "2026-10-01T00:00:00.123456789Z".parse()?;
            let child_id = "v81-running-target";
            let task = TaskRecord {
                id: "v81-noheader-running-task".into(),
                agent_id: "default".into(),
                kind: TaskKind::ActorInvocation,
                status: TaskStatus::Queued,
                created_at: born,
                updated_at: born,
                parent_message_id: None,
                work_item_id: None,
                summary: Some("original v81 pending invocation".into()),
                detail: Some(
                    serde_json::json!({"rejoin_obligation_id":"v81-noheader-running-task","rejoin_generation":1,"target_agent_id":child_id,"target_identity_incarnation":1,"child_turn_baseline":0}),
                ),
                recovery: Some(TaskRecoverySpec::AgentInvocation {
                    summary: "original v81 pending invocation".into(),
                    prompt: "complete original v81 child".into(),
                    authority_class: AuthorityClass::RuntimeInstruction,
                    target_agent_id: Some(child_id.into()),
                    created_new_subagent: false,
                    workspace_mode: ChildAgentWorkspaceMode::Inherit,
                    lifecycle_disposition: AgentLifecycleDisposition::Retain,
                }),
            };
            let mut running = MessageEnvelope::new(
                "default",
                MessageKind::TaskStatus,
                MessageOrigin::Task {
                    task_id: task.id.clone(),
                },
                AuthorityClass::RuntimeInstruction,
                Priority::Background,
                MessageBody::Text {
                    text: "original v81 monitor running before restart".into(),
                },
            )
            .with_admission(
                MessageDeliverySurface::TaskRejoin,
                AdmissionContext::RuntimeOwned,
            );
            running.id = "v81-noheader-pending-running".into();
            // Released enqueue allocated a hint before any actual Turn/claim.
            running.turn_id = Some("turn-v81-noheader-pending-running".into());
            running.task_id = Some(task.id.clone());
            running.created_at = born + chrono::Duration::seconds(1);
            running.metadata = Some(
                serde_json::json!({"task_id":task.id,"task_kind":task.kind,"task_status":"running","task_summary":task.summary,"task_recovery":task.recovery,"task_detail":task.detail}),
            );
            // The released ingress normalized admission fields before its
            // typed writer, as later legacy import also expects.
            running.normalize_admission_fields();
            // Model the actual released v81 Task/Message/Queue writer transaction.
            // No current-schema alias, receipt, Turn, claim or owner is synthesized.
            let tx = connection.transaction()?;
            // The released v81 Runtime constructor completed these derived
            // imports before accepting a native pending Message. They are
            // empty at that boundary; the allocated hint is not a Turn.
            let empty:(i64,i64)=tx.query_row("SELECT (SELECT COUNT(*) FROM turn_records),(SELECT COUNT(*) FROM conversation_input_assignments)",[],|row|Ok((row.get(0)?,row.get(1)?)))?;
            assert_eq!(empty, (0, 0));
            for domain in ["context_episode_anchors", "turn_records", "evidence"] {
                tx.execute("INSERT INTO storage_domains(domain,schema_version,import_status,canonical_source,updated_at) VALUES(?1,1,'complete','db',?2)",rusqlite::params![domain,repositories::timestamp(born)])?;
            }
            for id in ["default", child_id] {
                // Released Named creation installed its real home before identity.
                let agent_home = config.agent_root_dir().join(id);
                crate::agent_template::ensure_agent_home_layout(&agent_home)?;
                assert!(agent_home.join("memory/self.md").is_file());
                assert!(agent_home.join(".holon/state").is_dir());
                evidence::upsert_agent_identity_tx(
                    &tx,
                    &AgentIdentityRecord::new(
                        id,
                        AgentKind::Named,
                        AgentVisibility::Public,
                        AgentOwnership::SelfOwned,
                        AgentProfilePreset::PublicNamed,
                        None,
                        None,
                    ),
                )?;
                let mut state = AgentState::new(id);
                if id == "default" {
                    state.total_message_count = 1;
                }
                evidence::upsert_agent_state_tx(&tx, &state)?;
            }
            repositories::upsert_task_tx(&tx, &task)?;
            let (running, _) = evidence::append_message_tx(&tx, &running)?;
            repositories::upsert_queue_entry_tx(
                &tx,
                &QueueEntryRecord {
                    message_id: running.id.clone(),
                    agent_id: "default".into(),
                    priority: Priority::Background,
                    status: QueueEntryStatus::Queued,
                    created_at: running.created_at,
                    updated_at: running.created_at,
                },
            )?;
            let no_actual_turn:(i64,i64)=tx.query_row("SELECT (SELECT COUNT(*) FROM turn_records),(SELECT COUNT(*) FROM conversation_input_assignments)",[],|row|Ok((row.get(0)?,row.get(1)?)))?;
            assert_eq!(
                no_actual_turn,
                (0, 0),
                "native QueueAdmit must not execute the preallocated hint"
            );
            tx.commit()?;
            let version: i64 =
                connection.query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
                    row.get(0)
                })?;
            assert_eq!(version, 81);
            assert!(running
                .metadata
                .as_ref()
                .unwrap()
                .get("task_created_at")
                .is_none());
            assert!(running.message_seq.is_some_and(|seq| seq > 0));
            drop(connection);
            (born, child_id, task, running)
        };
        // Opening the real new Host performs the one original upgrade cohort,
        // then actual bootstrap registers a new original monitor and Running.
        let clock_db_path = config.runtime_db_path();
        assert!(config
            .agent_root_dir()
            .join(child_id)
            .join("memory/self.md")
            .is_file());
        let host = crate::host::RuntimeHost::new_with_provider(
            config,
            Arc::new(crate::provider::StubProvider::new(
                "real v81 child completed",
            )),
        )
        .with_context(|| {
            format!(
                "v81 Host open: {}",
                clock_input_projection(&clock_db_path, &running.id)
            )
        })?;
        let assignments:i64=host.runtime_db().connection()?.query_row("SELECT COUNT(*) FROM conversation_input_assignments WHERE agent_id='default' AND message_id=?1",[&running.id],|row|row.get(0))?;
        assert_eq!(
            assignments, 0,
            "opening the cut must preserve the unexecuted hint"
        );
        assert!(
            host.bridge().reusable_agent_exists(child_id).await?,
            "the released Named target must satisfy the original Active+Home reuse predicate"
        );
        let target = host
            .agent_identity_record(child_id)?
            .context("v81 target identity absent")?;
        assert_eq!(target.incarnation, 1);
        assert_eq!(target.status, crate::types::AgentRegistryStatus::Active);
        let gate = Arc::new(Semaphore::new(0));
        host.set_clock_recovery_dispatch_gate_for_test("default", gate.clone());
        host.recover_orphaned_queue_claims_at_startup()
            .await
            .with_context(|| {
                format!(
                    "v81 startup recovery: {}",
                    clock_input_projection(&clock_db_path, &running.id)
                )
            })?;
        let before_runtime = clock_input_projection(&clock_db_path, &running.id);
        let parent = host.default_runtime().await.with_context(|| {
            format!(
                "v81 default Runtime: before={before_runtime}; after={}",
                clock_input_projection(&clock_db_path, &running.id)
            )
        })?;
        let statuses = wait_queued_running(&parent, &task.id, 2).await?;
        let old = statuses
            .iter()
            .find(|message| message.id == running.id)
            .context("original v81 Running lost")?;
        let new = statuses
            .iter()
            .find(|message| message.id != running.id)
            .context("actual recovered monitor did not enqueue its own Running")?;
        assert_eq!(old, &running);
        assert_eq!(
            new.metadata.as_ref().unwrap()["task_created_at"],
            serde_json::to_value(born)?
        );
        assert_eq!(
            parent
                .inner
                .control_cancellations
                .original_child_running_status_clock(old)?,
            None
        );
        assert_eq!(
            parent
                .inner
                .control_cancellations
                .original_child_running_status_clock(new)?,
            Some(born)
        );
        {
            let owner = parent.begin_mutation().await?;
            let selected = owner.capture_tasks(&TaskCaptureRequest::current())?;
            let inputs = owner.capture_input_evidence()?;
            assert_eq!(selected.coverage.running.sources[&old.id].created_at, born);
            let parsed = crate::runtime::tasks::task_from_message(old, "default")?;
            assert_eq!(
                qualify(&inputs, &selected, old, &parsed, None)?
                    .unwrap()
                    .created_at(),
                born
            );
        }
        let new_id = new.id.clone();
        gate.add_permits(1);
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if parent
                    .runtime_db()
                    .queue_entries()
                    .latest(&running.id)?
                    .unwrap()
                    .status
                    == QueueEntryStatus::Processed
                {
                    return Ok::<(), anyhow::Error>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .context("legacy old Running did not complete real Start/commonFinish")??;
        let selected = parent.capture_current_task_record(&task.id)?;
        assert_eq!(selected.record().unwrap().created_at, born);
        assert_eq!(selected.record().unwrap().recovery, task.recovery);
        assert_eq!(
            crate::runtime::tasks::unbound_task_rejoin_generation(selected.record().unwrap()),
            Some(1)
        );
        drop(selected);
        assert_eq!(
            parent
                .runtime_db()
                .queue_entries()
                .latest(&new_id)?
                .unwrap()
                .status,
            QueueEntryStatus::Queued
        );
        let child = host.get_public_agent(child_id).await?;
        child.enqueue(original_child_prompt(child_id)).await?;
        let terminal = wait_terminal(&parent, &task.id).await?;
        assert_eq!(terminal.created_at, born);
        assert_eq!(terminal.status, TaskStatus::Completed);
        assert_eq!(
            parent
                .runtime_db()
                .queue_entries()
                .latest(&new_id)?
                .unwrap()
                .status,
            QueueEntryStatus::Queued
        );
        let count:usize=parent.runtime_db().connection()?.query_row("SELECT COUNT(*) FROM input_mutation_receipts WHERE family='task' AND json_extract(outcome_json,'$.result.kind')='child_task_terminal' AND json_extract(outcome_json,'$.result.ack.original.admitted.key.id')=?1",[&task.id],|row|row.get(0))?;
        assert_eq!(count, 1);
        parent.runtime_db().transaction(|tx| {
            let raw:String=tx.query_row("SELECT prepared_json FROM input_mutation_receipts WHERE family='task' AND json_extract(outcome_json,'$.result.kind')='child_task_terminal' AND json_extract(outcome_json,'$.result.ack.original.admitted.key.id')=?1",[&task.id],|row|row.get(0))?;
            assert!(crate::runtime_db::input_mutation_receipts::lookup_input_mutation_receipt_tx(tx,&serde_json::from_str(&raw)?)?.is_some());Ok(())
        })?;
        gate.add_permits(1024);
        host.shutdown().await?;
        Ok(())
    })
}
