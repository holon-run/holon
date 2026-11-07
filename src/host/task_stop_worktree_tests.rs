use super::*;
use crate::provider::{ProviderTurnRequest, ProviderTurnResponse};
use crate::{
    runtime::resident_input_evidence::InputEvidenceScope,
    runtime::resident_task::{TaskCaptureRequest, TaskFamily, TaskKey},
    runtime_db::{
        evidence::upsert_agent_state_tx,
        input_mutation_receipts::InputMutationResult,
        task_snapshot::{self, TaskDomainLoadRequest},
        task_stop::{
            PreparedTaskStop, TaskStopChildTarget, TaskStopResultPlan, TaskStopSignal,
            TaskStopWorktreeTarget,
        },
        transitions::{InputMutationExecution, PreparedTaskTransition, TaskTransitionCommand},
        work_snapshot::WorkDomainLoadRequest,
    },
    types::{
        AdmissionContext, AgentState, AuthorityClass, MessageBody, MessageDeliverySurface,
        MessageEnvelope, MessageKind, MessageOrigin, Priority, TaskKind, TaskRecord, TaskStatus,
    },
};
use std::process::Command;
struct LiveChildStopProvider {
    started: Arc<tokio::sync::Notify>,
}
#[async_trait::async_trait]
impl AgentProvider for LiveChildStopProvider {
    async fn complete_turn(&self, _: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        self.started.notify_one();
        std::future::pending().await
    }
}

fn git(root: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git").args(args).current_dir(root).output()?;
    anyhow::ensure!(
        output.status.success(),
        "git {:?}: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
fn request() -> TaskDomainLoadRequest {
    TaskDomainLoadRequest {
        recovery_at: None,
        query: Default::default(),
        capture: TaskCaptureRequest::current(),
        record_limit: 4096,
        byte_limit: 16 * 1024 * 1024,
    }
}
fn work() -> WorkDomainLoadRequest {
    WorkDomainLoadRequest {
        capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
        record_limit: 4096,
        byte_limit: 16 * 1024 * 1024,
    }
}
fn command(task: TaskRecord, expected_owner_incarnation: u64) -> TaskTransitionCommand {
    TaskTransitionCommand {
        agent_id: task.agent_id.clone(),
        expected_owner_incarnation,
        task,
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
        fault: None,
    }
}
#[derive(Debug, Clone, Copy)]
enum TargetFixture {
    OriginalChild,
    DeletedMetadata,
    UnknownTarget,
    AlreadyDeletedChild,
    LiveRunChild,
}

struct Fixture {
    _home: tempfile::TempDir,
    host: RuntimeHost,
    scope: InputEvidenceScope,
    original: Arc<TaskStopRecord>,
    exit: Arc<TaskStopWorktreeProducerExit>,
    repo: PathBuf,
    path: PathBuf,
    _source: Arc<BudgetViewGuard>,
    child_runtime: RuntimeHandle,
}
impl Fixture {
    fn new() -> Pin<Box<dyn Future<Output = Result<Self>> + Send>> {
        Self::with_target(TargetFixture::OriginalChild)
    }
    fn with_target(target: TargetFixture) -> Pin<Box<dyn Future<Output = Result<Self>> + Send>> {
        Box::pin(async move {
            let home = tempfile::tempdir()?;
            std::fs::write(
                home.path().join("config.json"),
                r#"{"model":{"default":"openai/gpt-5.4"}}"#,
            )?;
            let mut config = crate::config::AppConfig::load_with_home(Some(home.path().into()))?;
            config.user_home_dir = Some(home.path().into());
            let started = Arc::new(tokio::sync::Notify::new());
            let provider: Arc<dyn AgentProvider> = if matches!(target, TargetFixture::LiveRunChild)
            {
                Arc::new(LiveChildStopProvider {
                    started: started.clone(),
                })
            } else {
                Arc::new(crate::provider::StubProvider::new("done"))
            };
            let host = RuntimeHost::new_with_provider(config, provider)?;
            let identity = host.ensure_default_agent_identity()?;
            let scope = InputEvidenceScope {
                agent_id: identity.agent_id.clone(),
                identity_incarnation: identity.incarnation,
            };
            host.runtime_db()
                .transaction(|tx| upsert_agent_state_tx(tx, &AgentState::new(&scope.agent_id)))?;
            let root = home.path().join("original-repo");
            std::fs::create_dir(&root)?;
            git(&root, &["init", "--quiet"])?;
            git(&root, &["config", "user.email", "fixture@localhost"])?;
            git(&root, &["config", "user.name", "Fixture"])?;
            std::fs::write(root.join("tracked"), "original\n")?;
            git(&root, &["add", "tracked"])?;
            git(&root, &["commit", "--quiet", "-m", "original"])?;
            let path = home.path().join("original-worktree");
            git(
                &root,
                &[
                    "worktree",
                    "add",
                    "--quiet",
                    "-b",
                    "original-stop",
                    path.to_str().unwrap(),
                ],
            )?;
            let at: chrono::DateTime<Utc> = "2026-10-01T00:00:00.123456789Z".parse()?;
            let mut task = TaskRecord {
                id: "original-worktree-task".into(),
                agent_id: scope.agent_id.clone(),
                kind: TaskKind::ChildAgentTask,
                status: TaskStatus::Running,
                created_at: at,
                updated_at: at,
                parent_message_id: None,
                work_item_id: None,
                summary: Some("original worktree fixture".into()),
                detail: Some(json!({"workspace_mode":"worktree","worktree":{
                "worktree_path":path,"worktree_branch":"original-stop","original_cwd":root,"original_branch":"original"}})),
                recovery: None,
            };
            let child = host
                .create_child_identity(
                    &scope.agent_id,
                    &task,
                    None,
                    &host.agent_data_dir(&scope.agent_id),
                )
                .await?;
            task.detail.as_mut().unwrap()["child_agent_id"] = json!(child.agent_id);
            match target {
                TargetFixture::DeletedMetadata => {
                    task.detail.as_mut().unwrap()["worktree"]["cleanup_reason"] =
                        json!("agent_deleted");
                }
                TargetFixture::UnknownTarget => {
                    // An explicit malformed target must not fall back to the child id.
                    task.detail.as_mut().unwrap()["target"] = json!({});
                }
                TargetFixture::OriginalChild
                | TargetFixture::AlreadyDeletedChild
                | TargetFixture::LiveRunChild => {}
            }
            let runtime = host.get_or_create_agent(&child.agent_id).await?;
            runtime.wait_for_bootstrap().await?;
            let ledger = &host.inner.input_evidence_budget;
            let first = PreparedInputTransition::task(
                scope.clone(),
                "fs-task-first-admission".into(),
                at,
                PreparedTaskTransition::new(
                    command(task.clone(), scope.identity_incarnation),
                    Default::default(),
                    false,
                    None,
                    None,
                )?,
            )?;
            host.runtime_db()
                .transitions_in_scope(&scope)
                .commit_prepared_transition_with_work_request_and_memory_budget(
                    &first,
                    &work(),
                    ledger,
                    &request(),
                )?;
            let locator = host.runtime_db().transaction(|tx| {
                let snapshot = task_snapshot::prepare_task_domain_snapshot_with_budget_tx(
                    tx,
                    &scope,
                    &request(),
                    ledger,
                )?
                .require_current()?;
                Ok(snapshot
                    .resident
                    .capture(&request().capture)?
                    .lookup(&TaskKey::new(TaskFamily::Task, &task.id))?
                    .context("fixture qualified Task")?
                    .locator
                    .clone())
            })?;
            let mut message = MessageEnvelope::new(
                &scope.agent_id,
                MessageKind::TaskResult,
                MessageOrigin::Task {
                    task_id: task.id.clone(),
                },
                AuthorityClass::RuntimeInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "Task cancelled".into(),
                },
            );
            message.id = "original-stop-frozen-result".into();
            message.task_id = Some(task.id.clone());
            message.created_at = at;
            message.delivery_surface = Some(MessageDeliverySurface::TaskRejoin);
            message.admission_context = Some(AdmissionContext::RuntimeOwned);
            let intent = PreparedTaskStop {
                task: locator,
                kind: task.kind,
                created_at: task.created_at,
                rejoin: None,
                owner_epoch: 1,
                signal: TaskStopSignal::Abort,
                lease_id: Some("original-fixture-monitor".into()),
                child: Some(TaskStopChildTarget {
                    scope: InputEvidenceScope {
                        agent_id: child.agent_id.clone(),
                        identity_incarnation: child.incarnation,
                    },
                    parent_agent_id: scope.agent_id.clone(),
                    creation_task_id: task.id.clone(),
                }),
                child_unavailable: None,
                worktree: Some(TaskStopWorktreeTarget {
                    path: path.to_str().unwrap().into(),
                    branch: "original-stop".into(),
                }),
                worktree_unavailable: None,
                result: Some(TaskStopResultPlan {
                    phase_nonce: "original-stop-result-phase".into(),
                    message,
                }),
            };
            task.status = TaskStatus::Cancelled;
            task.updated_at = at + chrono::Duration::seconds(1);
            let initial = PreparedInputTransition::task(
                scope.clone(),
                "original-fs-stop".into(),
                task.updated_at,
                PreparedTaskTransition::new(
                    command(task, scope.identity_incarnation),
                    Default::default(),
                    false,
                    None,
                    None,
                )?
                .with_stop_intent(intent)?,
            )?;
            let committed = host
                .runtime_db()
                .transitions_in_scope(&scope)
                .commit_prepared_transition_with_work_request_and_memory_budget(
                    &initial,
                    &work(),
                    ledger,
                    &request(),
                )?;
            let InputMutationExecution::Committed { receipt, .. } = committed else {
                unreachable!()
            };
            let InputMutationResult::Task {
                stop: Some(binding),
                ..
            } = &receipt.outcome.result
            else {
                unreachable!()
            };
            let (original, source) = host.runtime_db().transaction(|tx| {
                let loaded = task_stop::load_task_stop_by_sequence_tx(
                    tx,
                    &scope,
                    binding.effect_seq,
                    request().byte_limit,
                    ledger,
                )?;
                let record = loaded.records[0].clone();
                let view = ledger.register_task_stop_view(
                    &scope,
                    InputMemoryCategory::Candidate,
                    std::iter::once(&record),
                )?;
                Ok((record, view))
            })?;
            if matches!(target, TargetFixture::AlreadyDeletedChild) {
                // This tombstone precedes Stop's actual retirement transaction.
                host.runtime_db()
                    .agent_identities()
                    .tombstone_with_closed_supervision(&child.agent_id)?;
            }
            if matches!(target, TargetFixture::LiveRunChild) {
                runtime
                    .enqueue(
                        MessageEnvelope::new(
                            &child.agent_id,
                            MessageKind::OperatorPrompt,
                            MessageOrigin::Operator {
                                actor_id: None,
                                actor_display_name: None,
                            },
                            AuthorityClass::OperatorInstruction,
                            Priority::Normal,
                            MessageBody::Text {
                                text: "Hold this original child provider request until cancelled"
                                    .into(),
                            },
                        )
                        .with_admission(
                            MessageDeliverySurface::CliPrompt,
                            AdmissionContext::LocalProcess,
                        ),
                    )
                    .await?;
                tokio::time::timeout(std::time::Duration::from_secs(10), started.notified())
                    .await?;
                assert!(runtime.current_run_abort_token().await.is_some());
            }
            let retired = host
                .bridge()
                .stop_private_agent_for_task_stop(original.clone())
                .await?;
            let exit = retired
                .producer_exit()
                .context("actual complete empty child fleet")?;
            drop(retired);
            Ok(Self {
                _home: home,
                host,
                scope,
                original,
                exit,
                repo: root,
                path,
                _source: source,
                child_runtime: runtime,
            })
        })
    }
    fn current(&self) -> Result<(Arc<TaskStopRecord>, Arc<BudgetViewGuard>)> {
        self.host.runtime_db().transaction(|tx| {
            let selected = task_stop::load_task_stop_by_sequence_tx(
                tx,
                &self.scope,
                self.original.effect_seq,
                request().byte_limit,
                &self.host.inner.input_evidence_budget,
            )?;
            let record = selected.records[0].clone();
            let view = self
                .host
                .inner
                .input_evidence_budget
                .register_task_stop_view(
                    &self.scope,
                    InputMemoryCategory::Candidate,
                    std::iter::once(&record),
                )?;
            Ok((record, view))
        })
    }
    fn receipts(&self) -> Result<i64> {
        let conn = self.host.runtime_db().connection()?;
        Ok(
            conn.query_row("SELECT COUNT(*) FROM input_mutation_receipts", [], |r| {
                r.get(0)
            })?,
        )
    }
    fn audits(&self) -> Result<i64> {
        let conn = self.host.runtime_db().connection()?;
        Ok(conn.query_row("SELECT COUNT(*) FROM audit_events", [], |r| r.get(0))?)
    }
    fn cleanup_event(&self, kind: &str) -> Result<AuditEvent> {
        let conn = self.host.runtime_db().connection()?;
        let raw: String = conn.query_row(
            "SELECT data_json FROM audit_events WHERE kind=?1 AND json_extract(data_json,'$.data.task_id')=?2",
            rusqlite::params![kind, self.original.intent.task.key.id],
            |row| row.get(0),
        )?;
        Ok(serde_json::from_str(&raw)?)
    }
    fn assert_worktree_retained(&self) -> Result<()> {
        assert!(self.path.join("tracked").exists());
        git(
            &self.repo,
            &["show-ref", "--verify", "refs/heads/original-stop"],
        )?;
        git(&self.path, &["rev-parse", "--is-inside-work-tree"])?;
        Ok(())
    }
    fn disposition(&self) -> Result<task_stop::OriginalTaskStopWorktreeDisposition> {
        self.host.runtime_db().transaction(|tx| {
            Ok(task_stop::prepare_original_task_stop_worktree_tx(
                tx,
                &self.original,
                &request(),
                &self.host.inner.input_evidence_budget,
            )?
            .context("fixture qualified worktree facts")?
            .disposition)
        })
    }
}
#[tokio::test]
async fn original_worktree_fs_ack_is_receipt_first_and_never_reexecutes_after_commit() -> Result<()>
{
    let f = Fixture::new().await?;
    let task_stop::OriginalTaskStopWorktreeDisposition::RequireOriginalChildTeardown {
        identity_revision,
    } = f.disposition()?
    else {
        anyhow::bail!("original child must require its own teardown proof");
    };
    assert!(f
        .exit
        .proves_original_child_teardown(&f.original, identity_revision));
    f.host
        .bridge()
        .finish_original_task_stop_worktree(f.original.clone(), f.exit.clone(), request())
        .await?;
    assert!(!f.path.exists());
    assert_eq!(
        f.current()?.0.progress.worktree,
        TaskStopWorktreeState::Removed
    );
    assert!(git(
        &f.repo,
        &["show-ref", "--verify", "refs/heads/original-stop"]
    )
    .is_err());
    let event = f.cleanup_event("worktree_auto_cleaned_up")?;
    assert_eq!(event.data["reason"], "task_stop");
    assert_eq!(event.data["branch_cleanup_status"], "deleted");
    let receipts = f.receipts()?;
    let audits = f.audits()?;
    // Reusing the removed pathname must never cause a second FS operation.
    std::fs::create_dir(&f.path)?;
    std::fs::write(f.path.join("successor-marker"), "preserve")?;
    f.host
        .bridge()
        .finish_original_task_stop_worktree(f.original.clone(), f.exit.clone(), request())
        .await?;
    assert!(f.path.join("successor-marker").exists());
    assert_eq!(f.receipts()?, receipts);
    assert_eq!(f.audits()?, audits);
    assert_eq!(
        f.current()?.0.progress.worktree,
        TaskStopWorktreeState::Removed
    );
    f.host.shutdown().await?;
    Ok(())
}
#[tokio::test]
async fn deleted_and_unproven_targets_retain_branch_and_worktree_with_atomic_ack() -> Result<()> {
    use task_stop::OriginalTaskStopWorktreeDisposition::*;
    for (target, reason) in [
        (TargetFixture::DeletedMetadata, "agent_deleted"),
        (TargetFixture::UnknownTarget, "task_target_unproven"),
        (
            TargetFixture::AlreadyDeletedChild,
            "original_child_teardown_unproven",
        ),
    ] {
        let f = Fixture::with_target(target).await?;
        match (target, f.disposition()?) {
            (TargetFixture::DeletedMetadata, RetainDeletedTarget)
            | (TargetFixture::UnknownTarget, RetainUnprovenTarget) => {}
            (
                TargetFixture::AlreadyDeletedChild,
                RequireOriginalChildTeardown { identity_revision },
            ) => assert!(!f
                .exit
                .proves_original_child_teardown(&f.original, identity_revision)),
            actual => anyhow::bail!("unexpected retention fixture: {actual:?}"),
        }
        let receipts = f.receipts()?;
        let audits = f.audits()?;
        f.host.runtime_db().transaction(|tx| {
            tx.execute_batch("CREATE TRIGGER reject_retention_phase BEFORE INSERT ON input_mutation_receipts WHEN NEW.command_nonce LIKE 'stop-worktree-%' BEGIN SELECT RAISE(ABORT,'fixture retention receipt failure'); END;")?;
            Ok(())
        })?;
        let error = f
            .host
            .bridge()
            .finish_original_task_stop_worktree(f.original.clone(), f.exit.clone(), request())
            .await
            .err()
            .context("retention receipt fault must rollback")?;
        assert!(format!("{error:#}").contains("fixture retention receipt failure"));
        f.assert_worktree_retained()?;
        assert_eq!(
            f.current()?.0.progress.worktree,
            TaskStopWorktreeState::Pending
        );
        assert_eq!(f.receipts()?, receipts);
        assert_eq!(f.audits()?, audits);
        f.host.runtime_db().transaction(|tx| {
            tx.execute_batch("DROP TRIGGER reject_retention_phase;")?;
            Ok(())
        })?;
        f.host
            .bridge()
            .finish_original_task_stop_worktree(f.original.clone(), f.exit.clone(), request())
            .await?;
        f.assert_worktree_retained()?;
        assert_eq!(
            f.current()?.0.progress.worktree,
            TaskStopWorktreeState::Retained
        );
        assert_eq!(f.receipts()?, receipts + 1);
        assert_eq!(f.audits()?, audits + 1);
        let event = f.cleanup_event("task_worktree_cleanup_retained")?;
        assert_eq!(event.data["reason"], reason);
        assert_eq!(event.data["cleanup_trigger"], "task_stop");
        assert_eq!(event.data["worktree"]["cleanup_reason"], reason);
        assert_eq!(event.data["worktree"]["cleanup_status"], "retained");
        assert_eq!(event.data["worktree"]["retained_for_review"], true);
        assert_eq!(event.data["worktree"]["auto_cleaned_up"], false);
        f.host
            .bridge()
            .finish_original_task_stop_worktree(f.original.clone(), f.exit.clone(), request())
            .await?;
        f.assert_worktree_retained()?;
        assert_eq!(f.receipts()?, receipts + 1);
        assert_eq!(f.audits()?, audits + 1);
        f.host.shutdown().await?;
    }
    Ok(())
}
#[tokio::test]
async fn original_teardown_revision_must_match_the_held_child_identity() -> Result<()> {
    let f = Fixture::new().await?;
    let task_stop::OriginalTaskStopWorktreeDisposition::RequireOriginalChildTeardown {
        identity_revision,
    } = f.disposition()?
    else {
        anyhow::bail!("original child must require teardown proof");
    };
    let child = f.original.intent.child.as_ref().unwrap();
    f.host.runtime_db().transaction(|tx| {
        tx.execute(
            "UPDATE agent_identities SET payload_json=json_set(payload_json,'$.revision',json_extract(payload_json,'$.revision')+1) WHERE agent_id=?1",
            [&child.scope.agent_id],
        )?;
        Ok(())
    })?;
    let task_stop::OriginalTaskStopWorktreeDisposition::RequireOriginalChildTeardown {
        identity_revision: changed_revision,
    } = f.disposition()?
    else {
        anyhow::bail!("same original child must still require teardown proof");
    };
    assert_ne!(identity_revision, changed_revision);
    assert!(!f
        .exit
        .proves_original_child_teardown(&f.original, changed_revision));
    f.host
        .bridge()
        .finish_original_task_stop_worktree(f.original.clone(), f.exit.clone(), request())
        .await?;
    f.assert_worktree_retained()?;
    assert_eq!(
        f.current()?.0.progress.worktree,
        TaskStopWorktreeState::Retained
    );
    let event = f.cleanup_event("task_worktree_cleanup_retained")?;
    assert_eq!(event.data["reason"], "original_child_teardown_unproven");
    assert_eq!(
        event.data["worktree"]["cleanup_reason"],
        "original_child_teardown_unproven"
    );
    f.host.shutdown().await?;
    Ok(())
}
#[tokio::test]
async fn original_worktree_fs_rollback_retries_as_already_removed_same_request() -> Result<()> {
    let f = Fixture::new().await?;
    let before_receipts = f.receipts()?;
    let before_audits = f.audits()?;
    let probe = PreparedInputTransition::task_stop_worktree_cleanup_request(
        &f.original,
        &request(),
        &f.host.inner.input_evidence_budget,
    )?;
    f.host.runtime_db().transaction(|tx|{tx.execute_batch("CREATE TRIGGER reject_fs_phase BEFORE INSERT ON input_mutation_receipts WHEN NEW.command_nonce LIKE 'stop-worktree-%' BEGIN SELECT RAISE(ABORT,'fixture FS receipt failure'); END;")?;Ok(())})?;
    let error = f
        .host
        .bridge()
        .finish_original_task_stop_worktree(f.original.clone(), f.exit.clone(), request())
        .await
        .err()
        .context("receipt fault must rollback")?;
    assert!(format!("{error:#}").contains("fixture FS receipt failure"));
    assert!(!f.path.exists());
    assert_eq!(
        f.current()?.0.progress.worktree,
        TaskStopWorktreeState::Pending
    );
    assert_eq!(f.receipts()?, before_receipts);
    assert_eq!(f.audits()?, before_audits);
    f.host.runtime_db().transaction(|tx| {
        tx.execute_batch("DROP TRIGGER reject_fs_phase;")?;
        Ok(())
    })?;
    f.host
        .bridge()
        .finish_original_task_stop_worktree(f.original.clone(), f.exit.clone(), request())
        .await?;
    assert_eq!(
        f.current()?.0.progress.worktree,
        TaskStopWorktreeState::AlreadyRemoved
    );
    let current = f.current()?;
    let fresh = PreparedInputTransition::task_stop_worktree_cleanup_request(
        &current.0,
        &request(),
        &f.host.inner.input_evidence_budget,
    )?;
    assert_eq!(probe.descriptor(), fresh.descriptor());
    assert_eq!(f.receipts()?, before_receipts + 1);
    f.host.shutdown().await?;
    Ok(())
}
#[tokio::test]
async fn original_worktree_changed_child_incarnation_is_retained_without_successor_state_read(
) -> Result<()> {
    let f = Fixture::new().await?;
    let child = f.original.intent.child.as_ref().unwrap();
    f.host.runtime_db().transaction(|tx| {
        // Explicit conflicting-restore fixture. The original Stop retains inc1;
        // the successor's State must not become an FS authorization source.
        tx.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',2) WHERE agent_id=?1",[&child.scope.agent_id])?;
        upsert_agent_state_tx(tx,&AgentState::new(&child.scope.agent_id))?;
        tx.execute("UPDATE agent_states SET payload_json='poison-successor-State' WHERE agent_id=?1",[&child.scope.agent_id])?;Ok(())
    })?;
    f.host
        .bridge()
        .finish_original_task_stop_worktree(f.original.clone(), f.exit.clone(), request())
        .await?;
    assert!(f.path.exists());
    assert_eq!(
        f.current()?.0.progress.worktree,
        TaskStopWorktreeState::Retained
    );
    let error = f.host.shutdown().await.expect_err(
        "conflicting successor must leave the original child responsibility unresolved",
    );
    assert!(format!("{error:#}")
        .contains("original TaskStop continuation failed; durable responsibility retained"));
    let current = f.current()?;
    assert_eq!(
        current.0.progress.child,
        crate::runtime_db::task_stop::TaskStopChildState::Pending
    );
    assert_eq!(current.0.progress.worktree, TaskStopWorktreeState::Retained);
    assert!(f.path.exists());
    let raw: String = f.host.runtime_db().connection()?.query_row(
        "SELECT payload_json FROM agent_states WHERE agent_id=?1",
        [&child.scope.agent_id],
        |row| row.get(0),
    )?;
    assert_eq!(raw, "poison-successor-State");
    Ok(())
}

#[tokio::test]
async fn unfinished_original_phase_attempt_blocks_git_until_receipt_absence_is_resolved(
) -> Result<()> {
    let f = Fixture::new().await?;
    let probe = PreparedInputTransition::task_stop_worktree_cleanup_request(
        &f.original,
        &request(),
        &f.host.inner.input_evidence_budget,
    )?;
    let mut held = f
        .host
        .reserve_mutation_scopes(&[(f.scope.agent_id.clone(), f.scope.identity_incarnation)])
        .await?;
    let witness = crate::runtime::mutation_witness::MutationWitness::input_mutation(
        probe.descriptor().command_nonce().to_owned(),
        crate::runtime_db::input_mutation_receipts::OriginalInputMutationAttempt::new(
            probe.descriptor().clone(),
        ),
        Vec::new(),
    )?
    .with_task_stop_parent_observation(&held[0], &probe)?;
    MutationReservation::begin_all_with_witness(
        &mut held,
        probe.descriptor().command_nonce().to_owned(),
        witness.clone(),
    )?;
    drop(held);
    let receipts = f.receipts()?;
    let audits = f.audits()?;
    let error = f
        .host
        .bridge()
        .finish_original_task_stop_worktree(f.original.clone(), f.exit.clone(), request())
        .await
        .err()
        .context("unfinished original phase must block FS")?;
    assert!(format!("{error:#}").contains("reconcil"), "{error:#}");
    assert!(f.path.exists());
    assert_eq!(f.receipts()?, receipts);
    assert_eq!(f.audits()?, audits);
    witness.finish_original_attempt();
    f.host
        .bridge()
        .finish_original_task_stop_worktree(f.original.clone(), f.exit.clone(), request())
        .await?;
    assert!(!f.path.exists());
    assert_eq!(
        f.current()?.0.progress.worktree,
        TaskStopWorktreeState::Removed
    );
    f.host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn child_cleanup_retains_original_readonly_parent_stop_receipt() -> Result<()> {
    let f = Fixture::new().await?;
    let child = f.original.intent.child.as_ref().unwrap();
    let db = f.host.runtime_db();
    let retired = db
        .agent_identities()
        .latest(&child.scope.agent_id)?
        .unwrap();
    // This focused fixture has already produced the original tombstone.
    // Admit native physical cleanup before execution/history settlement.
    db.agent_deletions().begin(
        &child.scope.agent_id,
        retired.revision,
        "test_original_stop_cleanup",
        false,
    )?;
    let before = db.transaction(|tx| {
        Ok(
            crate::runtime_db::input_mutation_receipts::lookup_input_mutation_receipt_tx(
                tx,
                &f.original.prepared,
            )?
            .context("original Stop receipt")?,
        )
    })?;
    crate::runtime_db::agent_deletion_cleanup::export_private_history(
        db,
        &child.scope.agent_id,
        child.scope.identity_incarnation,
    )?;
    crate::runtime_db::agent_deletion_cleanup::settle_execution(
        db,
        &child.scope.agent_id,
        child.scope.identity_incarnation,
        Utc::now(),
    )?;
    crate::runtime_db::agent_deletion_cleanup::purge_private_history(
        db,
        &child.scope.agent_id,
        child.scope.identity_incarnation,
        Utc::now(),
    )?;
    let after = db.transaction(|tx| {
        Ok(
            crate::runtime_db::input_mutation_receipts::lookup_input_mutation_receipt_tx(
                tx,
                &f.original.prepared,
            )?
            .context("parent Stop receipt must survive child purge")?,
        )
    })?;
    assert_eq!(before, after);
    let (current, _view) = f.current()?;
    assert_eq!(current.prepared, f.original.prepared);
    assert_eq!(current.intent, f.original.intent);
    f.assert_worktree_retained()?;
    // Physical child history removal must not orphan the parent's next phase.
    f.host
        .bridge()
        .finish_original_task_stop_worktree(f.original.clone(), f.exit.clone(), request())
        .await?;
    assert!(!f.path.exists());
    f.host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn child_cleanup_readonly_stop_rejects_mutated_effects_and_generations() -> Result<()> {
    for scenario in [
        "child-notification",
        "child-incarnation",
        "child-parent",
        "parent-incarnation",
        "parent-task-nonterminal",
        "missing-parent-membership",
        "typed-target-wrong-incarnation",
        "typed-target-foreign-child",
        "typed-target-unknown-incarnation",
    ] {
        let f = Fixture::new().await?;
        let child = f.original.intent.child.as_ref().unwrap();
        let db = f.host.runtime_db();
        let retired = db
            .agent_identities()
            .latest(&child.scope.agent_id)?
            .unwrap();
        // This focused fixture has already produced the original tombstone.
        // Admit native physical cleanup before execution/history settlement.
        db.agent_deletions().begin(
            &child.scope.agent_id,
            retired.revision,
            "test_original_stop_cleanup",
            false,
        )?;
        let saved = db.transaction(|tx| {
            let receipt: (String, String, String) = tx.query_row(
                "SELECT outcome_json,result_digest,scopes_json FROM input_mutation_receipts WHERE command_nonce=?1",
                [f.original.prepared.command_nonce()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            let child_row: (String, String) = tx.query_row(
                "SELECT parent_agent_id,payload_json FROM agent_identities WHERE agent_id=?1",
                [&child.scope.agent_id], |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let parent: String = tx.query_row("SELECT payload_json FROM agent_identities WHERE agent_id=?1", [&f.scope.agent_id], |r| r.get(0))?;
            let task: (String, String) = tx.query_row("SELECT status,payload_json FROM tasks WHERE task_id=?1", [&f.original.intent.task.key.id], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok((receipt, child_row, parent, task))
        })?;
        db.transaction(|tx| {
            match scenario {
                "child-notification" => {
                    use crate::runtime_db::input_mutation_receipts::{
                        lookup_input_mutation_receipt_tx, canonical_command_digest,
                    };
                    let mut receipt = lookup_input_mutation_receipt_tx(tx, &f.original.prepared)?.unwrap();
                    receipt.outcome.scopes.iter_mut().find(|s| s.scope == child.scope).unwrap().notify_scheduler = true;
                    let digest = canonical_command_digest(&json!({"prepared":receipt.prepared,"outcome":receipt.outcome,"committed_at":receipt.committed_at}))?;
                    tx.execute("UPDATE input_mutation_receipts SET outcome_json=?2,result_digest=?3 WHERE command_nonce=?1",
                        rusqlite::params![f.original.prepared.command_nonce(),serde_json::to_string(&receipt.outcome)?,digest])?;
                }
                "missing-parent-membership" => { tx.execute("UPDATE input_mutation_receipts SET scopes_json=?2 WHERE command_nonce=?1", rusqlite::params![f.original.prepared.command_nonce(), serde_json::to_string(&vec![child.scope.clone()])?])?; }
                "child-incarnation" => { tx.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',2) WHERE agent_id=?1", [&child.scope.agent_id])?; }
                "child-parent" => { tx.execute("UPDATE agent_identities SET parent_agent_id='foreign-parent',payload_json=json_set(payload_json,'$.parent_agent_id','foreign-parent') WHERE agent_id=?1", [&child.scope.agent_id])?; }
                "parent-incarnation" => { tx.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',2) WHERE agent_id=?1", [&f.scope.agent_id])?; }
                "parent-task-nonterminal" => { tx.execute("UPDATE tasks SET status='running',payload_json=json_set(payload_json,'$.status','running') WHERE task_id=?1", [&f.original.intent.task.key.id])?; }
                scenario if scenario.starts_with("typed-target-") => {
                    let mut target = json!({"kind":"child_agent","agent_id":child.scope.agent_id,"incarnation":child.scope.identity_incarnation});
                    match scenario {
                        "typed-target-wrong-incarnation" => target["incarnation"] = json!(child.scope.identity_incarnation + 1),
                        "typed-target-foreign-child" => target["agent_id"] = json!("foreign-child"),
                        "typed-target-unknown-incarnation" => { target.as_object_mut().unwrap().remove("incarnation"); }
                        _ => unreachable!(),
                    }
                    tx.execute("UPDATE tasks SET payload_json=json_set(payload_json,'$.detail.target',json(?2)) WHERE task_id=?1", rusqlite::params![f.original.intent.task.key.id, serde_json::to_string(&target)?])?;
                }
                _ => unreachable!(),
            }
            Ok(())
        })?;
        let error = crate::runtime_db::agent_deletion_cleanup::export_private_history(
            db,
            &child.scope.agent_id,
            child.scope.identity_incarnation,
        )
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("shared command history")
                || (scenario == "child-incarnation"
                    && format!("{error:#}").contains("incarnation")),
            "{scenario}: {error:#}"
        );
        let count: i64 = db.connection()?.query_row(
            "SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce=?1",
            [f.original.prepared.command_nonce()],
            |r| r.get(0),
        )?;
        assert_eq!(count, 1, "{scenario}: original parent receipt must survive");
        f.assert_worktree_retained()?;
        // Restore the synthetic mutations so shutdown can drain its original
        // valid Stop continuation, rather than ignore a teardown failure.
        db.transaction(|tx| {
            tx.execute("UPDATE input_mutation_receipts SET outcome_json=?2,result_digest=?3,scopes_json=?4 WHERE command_nonce=?1", rusqlite::params![f.original.prepared.command_nonce(),saved.0.0,saved.0.1,saved.0.2])?;
            tx.execute("UPDATE agent_identities SET parent_agent_id=?2,payload_json=?3 WHERE agent_id=?1", rusqlite::params![child.scope.agent_id,saved.1.0,saved.1.1])?;
            tx.execute("UPDATE agent_identities SET payload_json=?2 WHERE agent_id=?1", rusqlite::params![f.scope.agent_id,saved.2])?;
            tx.execute("UPDATE tasks SET status=?2,payload_json=?3 WHERE task_id=?1", rusqlite::params![f.original.intent.task.key.id,saved.3.0,saved.3.1])?;
            Ok(())
        })?;
        f.host.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn live_child_stop_drains_original_loop_before_metadata_recovery() -> Result<()> {
    let f = Fixture::with_target(TargetFixture::LiveRunChild).await?;
    assert!(
        f.child_runtime.current_run_abort_token().await.is_none(),
        "joined original child loop retained a stale transient run handle"
    );
    let child = f.original.intent.child.as_ref().unwrap();
    let identity = f
        .host
        .runtime_db()
        .agent_identities()
        .latest(&child.scope.agent_id)?
        .unwrap();
    let (_, job, _) = f.host.runtime_db().agent_deletions().begin(
        &child.scope.agent_id,
        identity.revision,
        "test_actual_live_stop_cleanup",
        false,
    )?;
    f.host.execute_deletion_job(job).await?;
    let job = f
        .host
        .runtime_db()
        .agent_deletions()
        .latest_for_agent(&child.scope.agent_id)?
        .unwrap();
    assert_eq!(
        job.status,
        crate::types::AgentDeletionStatus::Completed,
        "{job:?}"
    );
    assert!(!f.host.agent_data_dir(&child.scope.agent_id).exists());
    f.host.shutdown().await?;
    Ok(())
}
