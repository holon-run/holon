//! Real Task ACK + real registered Async monitor; no fabricated receipt or cap.
use super::control_cancellation::{ControlJoinLease, SharedControlTaskHandle};
use super::*;
use crate::runtime::tests::support::LifecycleHarness;
use crate::runtime_db::input_mutation_receipts::InputMutationResult;
use crate::types::{AgentLifecycleDisposition, ChildAgentWorkspaceMode};
use tokio::sync::oneshot;

struct ChildMonitorFixture {
    _harness: LifecycleHarness,
    runtime: RuntimeHandle,
    task: TaskRecord,
    source: Arc<OriginalChildTaskProducer>,
    lease: Arc<ControlJoinLease>,
    start: oneshot::Sender<()>,
    done: oneshot::Receiver<Result<()>>,
}
async fn child_monitor_fixture(fault: Option<TransitionFaultPoint>) -> Result<ChildMonitorFixture> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime().clone();
    let at = runtime.now();
    let task = TaskRecord {
        id: crate::ids::task_id(),
        agent_id: "default".into(),
        kind: TaskKind::ChildAgentTask,
        status: TaskStatus::Running,
        created_at: at,
        updated_at: at,
        parent_message_id: None,
        work_item_id: None,
        summary: Some("registered child producer".into()),
        detail: Some(serde_json::json!({"workspace_mode":"inherit"})),
        recovery: Some(TaskRecoverySpec::ChildAgentTask {
            summary: "registered child producer".into(),
            prompt: "fixture result".into(),
            authority_class: AuthorityClass::OperatorInstruction,
            workspace_mode: ChildAgentWorkspaceMode::Inherit,
            lifecycle_disposition: AgentLifecycleDisposition::Retain,
        }),
    };
    runtime
        .persist_task_transition(&task, "child_fixture_admitted")
        .await?;
    let (registration, lease, source) = runtime.admit_original_child_monitor(&task).await?;
    let (start, started) = oneshot::channel();
    let (completed, done) = oneshot::channel();
    let runner = source.runner_material()?;
    let producer = source.clone();
    let monitor_runtime = runtime.clone();
    let original = task.clone();
    lease.attach(tokio::spawn(async move {
        let _runner=runner;
        if started.await.is_err(){return}
        let mut terminal=original;terminal.status=TaskStatus::Completed;terminal.updated_at=monitor_runtime.now();
        terminal.detail.as_mut().unwrap()["output_summary"]=serde_json::json!("actual child result");
        let mut message=MessageEnvelope::new(terminal.agent_id.clone(),MessageKind::TaskResult,MessageOrigin::Task{task_id:terminal.id.clone()},
            AuthorityClass::RuntimeInstruction,Priority::Next,MessageBody::Text{text:"actual child result".into()})
            .with_admission(MessageDeliverySurface::TaskRejoin,AdmissionContext::RuntimeOwned);
        message.task_id=Some(terminal.id.clone());message.work_item_id=terminal.work_item_id.clone();
        message.turn_id=Some(crate::ids::turn_id());message.metadata=Some(serde_json::json!({"task_id":terminal.id,"task_status":"completed","task_detail":terminal.detail}));
        terminal.parent_message_id=Some(message.id.clone());
        let result=async {
            producer.observe_terminal_result(terminal,message,None,None,false)?;
            let observed=producer.observed_terminal().context("real monitor observation missing")?;
            producer.freeze_terminal(&observed.task,&observed.message,None,None,&[],fault)?;
            let receiver=monitor_runtime.register_original_child_completion(producer)?;
            receiver.await.context("registered child completion vanished")?
        }.await;
        let _=completed.send(result);
    }));
    runtime.inner.task_handles.lock().await.insert(
        task.id.clone(),
        command_task::ManagedTaskHandle::Async(SharedControlTaskHandle(lease.clone())),
    );
    drop(registration);
    Ok(ChildMonitorFixture {
        _harness: harness,
        runtime,
        task,
        source,
        lease,
        start,
        done,
    })
}
fn child_receipt(
    runtime: &RuntimeHandle,
    source: &OriginalChildTaskProducer,
) -> Result<Option<crate::runtime_db::input_mutation_receipts::InputMutationReceipt>> {
    use rusqlite::OptionalExtension;
    runtime.runtime_db().transaction(|tx| {
        let prepared: Option<String> = tx
            .query_row(
                "SELECT prepared_json FROM input_mutation_receipts WHERE command_nonce=?1",
                [format!("child-terminal:{}", source.address().lease_id)],
                |row| row.get(0),
            )
            .optional()?;
        prepared
            .map(|raw| {
                crate::runtime_db::input_mutation_receipts::lookup_input_mutation_receipt_tx(
                    tx,
                    &serde_json::from_str(&raw)?,
                )
            })
            .transpose()
            .map(Option::flatten)
    })
}
fn original_input_birth_inventory(runtime: &RuntimeHandle) -> Result<(u64, u64, u64)> {
    Ok(runtime.runtime_db().connection()?.query_row(
        "SELECT COALESCE((SELECT SUM(high_water) FROM input_query_birth_cursors),0),(SELECT COUNT(*) FROM input_query_birth_members),(SELECT COUNT(*) FROM input_query_birth_cursors)",
        [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
    )?)
}
fn original_child_result_birth(runtime: &RuntimeHandle) -> Result<(u64, Option<u64>)> {
    Ok(runtime.runtime_db().connection()?.query_row(
        "SELECT COUNT(*),MIN(b.birth_seq) FROM messages m JOIN input_query_birth_members b ON b.family='message' AND b.record_id=m.evidence_id AND b.agent_id=m.agent_id AND b.identity_incarnation=?1 JOIN input_evidence_current a ON a.agent_id=b.agent_id AND a.identity_incarnation=b.identity_incarnation AND a.family=b.family AND a.record_id=b.record_id WHERE m.agent_id='default' AND m.kind='task_result' AND a.availability='present' AND a.qualification='committed_writer'",
        [runtime.inner.identity_incarnation], |row| Ok((row.get(0)?,row.get(1)?)),
    )?)
}
#[tokio::test]
async fn child_original_terminal_whole_ack_replays_after_both_fault_boundaries() -> Result<()> {
    for fault in [
        None,
        Some(TransitionFaultPoint::BeforeCommit),
        Some(TransitionFaultPoint::BeforeCacheUpdate),
    ] {
        let f = child_monitor_fixture(fault).await?;
        let original_birth = original_input_birth_inventory(&f.runtime)?;
        f.start.send(()).unwrap();
        let first = tokio::time::timeout(Duration::from_secs(10), f.done).await??;
        assert_eq!(first.is_err(), fault.is_some());
        f.lease.join().await?;
        if fault == Some(TransitionFaultPoint::BeforeCommit) {
            assert_eq!(original_input_birth_inventory(&f.runtime)?, original_birth);
            assert_eq!(original_child_result_birth(&f.runtime)?, (0, None));
            assert!(child_receipt(&f.runtime, &f.source)?.is_none());
            assert_eq!(
                f.runtime
                    .runtime_db()
                    .tasks()
                    .latest(&f.task.id)?
                    .unwrap()
                    .status,
                TaskStatus::Running
            );
        }
        if fault == Some(TransitionFaultPoint::BeforeCacheUpdate) {
            assert_eq!(
                original_child_result_birth(&f.runtime)?,
                (1, Some(original_birth.0 + 1))
            );
        }
        if fault.is_some() {
            assert!(!f.source.is_durable_finished());
            assert!(f.source.has_terminal_responsibility());
            f.runtime
                .continue_original_child_terminal(f.source.clone())
                .await?;
        }
        assert!(f.source.is_durable_finished());
        assert!(f.source.frozen_terminal()?.is_none());
        assert!(f.source.observed_terminal().is_none());
        assert_eq!(
            f.runtime
                .runtime_db()
                .tasks()
                .latest(&f.task.id)?
                .unwrap()
                .status,
            TaskStatus::Completed
        );
        let receipt = child_receipt(&f.runtime, &f.source)?.unwrap();
        assert!(
            matches!(receipt.outcome.result,InputMutationResult::ChildTaskTerminal{ack} if matches!(ack.result,crate::runtime_db::child_task_terminal::ChildTaskTerminalResult::Delivered{..}))
        );
        let counts:(u64,u64)=f.runtime.runtime_db().connection()?.query_row("SELECT (SELECT COUNT(*) FROM messages WHERE kind='task_result'),(SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce=?1)",
            [format!("child-terminal:{}",f.source.address().lease_id)],|row|Ok((row.get(0)?,row.get(1)?)))?;
        assert_eq!(counts, (1, 1));
        let born = original_input_birth_inventory(&f.runtime)?;
        assert_eq!(born.0, original_birth.0 + 1);
        assert_eq!(born.1, original_birth.1 + 1);
        assert_eq!(
            original_child_result_birth(&f.runtime)?,
            (1, Some(original_birth.0 + 1))
        );
        f.runtime
            .continue_original_child_terminal(f.source.clone())
            .await?;
        assert_eq!(original_input_birth_inventory(&f.runtime)?, born);
        assert!(f
            .runtime
            .inner
            .task_handles
            .lock()
            .await
            .get(&f.task.id)
            .is_none());
    }
    Ok(())
}
#[tokio::test]
async fn child_no_state_terminal_preserves_preexisting_accepted_input_and_keeps_task_unprepared(
) -> Result<()> {
    let f = child_monitor_fixture(None).await?;
    let source = f
        .runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "accepted before State loss".into(),
            },
        ))
        .await?;
    let before = f.runtime.inner.input_evidence_budget.report()?;
    let original_birth = original_input_birth_inventory(&f.runtime)?;
    let original_resident_state = f.runtime.agent_state().await?;
    f.runtime.runtime_db().transaction(|tx| {
        tx.execute("DELETE FROM agent_states WHERE agent_id='default'", [])?;
        Ok(())
    })?;
    f.start.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(10), f.done).await???;
    f.lease.join().await?;
    assert!(f.source.is_durable_finished());
    let receipt = child_receipt(&f.runtime, &f.source)?.unwrap();
    assert!(
        matches!(receipt.outcome.result,InputMutationResult::ChildTaskTerminal{ack} if matches!(ack.result,crate::runtime_db::child_task_terminal::ChildTaskTerminalResult::Unavailable{reason:crate::runtime_db::child_task_terminal::ChildTaskTerminalUnavailable::OwnerNoState}))
    );
    assert_eq!(
        f.runtime
            .runtime_db()
            .tasks()
            .latest(&f.task.id)?
            .unwrap()
            .status,
        TaskStatus::Completed
    );
    assert!(
        f.runtime.inner.mutation_owner.capture()?.is_none(),
        "NoState ACK clears published domains rather than manufacturing a Ready root"
    );
    assert!(f
        .runtime
        .capture_task_read(&resident_task::TaskCaptureRequest::current())
        .await
        .is_err());
    let state_rows: u64 = f.runtime.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM agent_states WHERE agent_id='default'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(state_rows, 0, "NoState maintenance cannot recreate State");
    assert_eq!(
        f.runtime.agent_state().await?,
        original_resident_state,
        "legacy State observation keeps its cached value; it is not a Ready publication"
    );
    let queued: u64 = f.runtime.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM queue_entries WHERE message_id=?1",
        [&source.id],
        |row| row.get(0),
    )?;
    assert_eq!(queued, 1);
    let after = f.runtime.inner.input_evidence_budget.report()?;
    assert_eq!(after.pending, before.pending);
    assert_eq!(original_input_birth_inventory(&f.runtime)?, original_birth);
    assert_eq!(original_child_result_birth(&f.runtime)?, (0, None));
    Ok(())
}
#[tokio::test]
async fn child_retired_terminal_never_reads_poisoned_successor_state_or_changes_task() -> Result<()>
{
    let f = child_monitor_fixture(None).await?;
    let original_birth = original_input_birth_inventory(&f.runtime)?;
    f.runtime.runtime_db().transaction(|tx|{
        tx.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',2) WHERE agent_id='default'",[])?;
        tx.execute("UPDATE agent_states SET payload_json=?1 WHERE agent_id='default'",[format!("{{bad{}","x".repeat(1024*1024))])?;Ok(())
    })?;
    f.start.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(10), f.done).await???;
    f.lease.join().await?;
    assert!(f.source.is_durable_finished());
    assert_eq!(
        f.runtime
            .runtime_db()
            .tasks()
            .latest(&f.task.id)?
            .unwrap()
            .status,
        TaskStatus::Running
    );
    let receipt = child_receipt(&f.runtime, &f.source)?.unwrap();
    assert!(
        matches!(receipt.outcome.result,InputMutationResult::ChildTaskTerminal{ack} if !ack.task_changed && matches!(ack.result,crate::runtime_db::child_task_terminal::ChildTaskTerminalResult::Unavailable{reason:crate::runtime_db::child_task_terminal::ChildTaskTerminalUnavailable::ScopeRetired}))
    );
    let count: u64 = f.runtime.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM messages WHERE kind='task_result'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(count, 0);
    assert_eq!(original_input_birth_inventory(&f.runtime)?, original_birth);
    assert_eq!(original_child_result_birth(&f.runtime)?, (0, None));
    Ok(())
}

/// Only test orchestration; admission, source, lease and whole ACK are real.
pub(crate) struct RegisteredChildMonitorForTest {
    pub(crate) source: Arc<OriginalChildTaskProducer>,
    lease: Arc<ControlJoinLease>,
    start: Option<oneshot::Sender<()>>,
    done: Option<oneshot::Receiver<Result<()>>>,
}
impl RegisteredChildMonitorForTest {
    /// Freeze this registered observation before a later genuine Running ACK.
    pub(crate) fn freeze_completed_observation_for_test(
        &self,
        task: &TaskRecord,
        child: &str,
        deletion: crate::runtime_db::repositories::AgentDeletionRequest,
        fault: Option<TransitionFaultPoint>,
    ) -> Result<(TaskRecord, MessageEnvelope)> {
        let mut terminal = task.clone();
        terminal.status = TaskStatus::Completed;
        terminal.updated_at = chrono::Utc::now();
        terminal.detail.get_or_insert_with(|| serde_json::json!({}))["output_summary"] =
            serde_json::json!("actual child result");
        let mut message = MessageEnvelope::new(
            task.agent_id.clone(),
            MessageKind::TaskResult,
            MessageOrigin::Task {
                task_id: task.id.clone(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Next,
            MessageBody::Text {
                text: "actual child result".into(),
            },
        )
        .with_admission(
            MessageDeliverySurface::TaskRejoin,
            AdmissionContext::RuntimeOwned,
        );
        message.created_at = terminal.updated_at;
        message.task_id = Some(task.id.clone());
        message.work_item_id = task.work_item_id.clone();
        message.turn_id = Some(crate::ids::turn_id());
        message.metadata = Some(serde_json::json!({"task_id":task.id,"task_kind":task.kind,
            "task_status":"completed","task_detail":terminal.detail,"task_recovery":task.recovery}));
        terminal.parent_message_id = Some(message.id.clone());
        self.source.observe_terminal_result(
            terminal,
            message,
            Some(child.to_owned()),
            Some(deletion),
            false,
        )?;
        let observed = self
            .source
            .observed_terminal()
            .context("original fixture observation absent")?;
        self.source.freeze_terminal(
            &observed.task,
            &observed.message,
            observed.child_id.as_deref(),
            observed.deletion.as_ref(),
            &[],
            fault,
        )?;
        Ok((observed.task.clone(), observed.message.clone()))
    }
    pub(crate) fn start(&mut self) {
        self.start.take().unwrap().send(()).unwrap();
    }
    pub(crate) async fn finish(&mut self) -> Result<Result<()>> {
        let result =
            tokio::time::timeout(Duration::from_secs(10), self.done.take().unwrap()).await??;
        self.lease.join().await?;
        Ok(result)
    }
}
pub(crate) async fn register_child_monitor_for_test(
    runtime: &RuntimeHandle,
    task: &TaskRecord,
    child: Option<&str>,
    deletion: Option<crate::runtime_db::repositories::AgentDeletionRequest>,
    fault: Option<TransitionFaultPoint>,
    worktree: bool,
) -> Result<RegisteredChildMonitorForTest> {
    let (registration, lease, source) = runtime.admit_original_child_monitor(task).await?;
    if let Some(child) = child {
        runtime.bind_original_child_monitor_target(&source, child, None, 0)?;
    }
    let (start, started) = oneshot::channel();
    let (completed, done) = oneshot::channel();
    let runner = source.runner_material()?;
    let producer = source.clone();
    let runtime = runtime.clone();
    let original = task.clone();
    let child = child.map(str::to_owned);
    let handle = tokio::spawn({
        let runtime = runtime.clone();
        async move {
            let _runner = runner;
            if started.await.is_err() {
                return;
            }
            let mut terminal = original;
            terminal.status = TaskStatus::Completed;
            terminal.updated_at = runtime.now();
            terminal.detail.get_or_insert_with(|| serde_json::json!({}))["output_summary"] =
                serde_json::json!("actual child result");
            let mut message = MessageEnvelope::new(
                terminal.agent_id.clone(),
                MessageKind::TaskResult,
                MessageOrigin::Task {
                    task_id: terminal.id.clone(),
                },
                AuthorityClass::RuntimeInstruction,
                Priority::Next,
                MessageBody::Text {
                    text: "actual child result".into(),
                },
            )
            .with_admission(
                MessageDeliverySurface::TaskRejoin,
                AdmissionContext::RuntimeOwned,
            );
            message.task_id = Some(terminal.id.clone());
            message.work_item_id = terminal.work_item_id.clone();
            message.turn_id = Some(crate::ids::turn_id());
            message.metadata = Some(
                serde_json::json!({"task_id":terminal.id,"task_status":"completed","task_detail":terminal.detail}),
            );
            terminal.parent_message_id = Some(message.id.clone());
            let result = async {
                producer.observe_terminal_result(terminal, message, child, deletion, worktree)?;
                let observed = producer
                    .observed_terminal()
                    .context("child monitor observation missing")?;
                if !worktree {
                    producer.freeze_terminal(
                        &observed.task,
                        &observed.message,
                        observed.child_id.as_deref(),
                        observed.deletion.as_ref(),
                        &[],
                        fault,
                    )?;
                }
                runtime
                    .register_original_child_completion(producer)?
                    .await
                    .context("child owned completion vanished")?
            }
            .await;
            let _ = completed.send(result);
        }
    });
    lease.attach(handle);
    runtime.inner.task_handles.lock().await.insert(
        task.id.clone(),
        command_task::ManagedTaskHandle::Async(SharedControlTaskHandle(lease.clone())),
    );
    drop(registration);
    Ok(RegisteredChildMonitorForTest {
        source,
        lease,
        start: Some(start),
        done: Some(done),
    })
}
impl RuntimeHandle {
    pub(crate) fn child_task_domain_pointer_for_test(&self) -> Result<usize> {
        let root = self
            .inner
            .mutation_owner
            .capture()?
            .context("original observer root absent")?;
        Ok(Arc::as_ptr(
            root.tasks
                .as_ref()
                .context("original observer Task domain absent")?,
        ) as usize)
    }
}
pub(crate) fn child_observer_domains_for_test(
    runtime: &RuntimeHandle,
    task_id: &str,
) -> Result<([Option<usize>; 4], usize, u64)> {
    let root = runtime
        .inner
        .mutation_owner
        .capture()?
        .context("observer root missing")?;
    let tasks = root.tasks.as_ref().context("observer Tasks unprepared")?;
    let locator = &tasks
        .coverage()
        .status
        .heads
        .get(task_id)
        .context("observer head missing")?
        .task;
    let record = tasks
        .original_status_fact(locator)?
        .context("observer Task fact missing")?;
    Ok((
        [
            Some(Arc::as_ptr(&root.state) as usize),
            root.episodes
                .as_ref()
                .map(|value| Arc::as_ptr(value) as usize),
            root.input_evidence
                .as_ref()
                .map(|value| Arc::as_ptr(value) as usize),
            root.work.as_ref().map(|value| Arc::as_ptr(value) as usize),
        ],
        Arc::as_ptr(record) as usize,
        tasks.coverage().revision,
    ))
}
pub(crate) async fn prepare_child_observer_for_test(
    runtime: &RuntimeHandle,
    task: &TaskRecord,
) -> Result<()> {
    let mut owner = runtime.begin_mutation().await?;
    let mut request = resident_task::TaskCaptureRequest::current();
    request.required_keys.insert(resident_task::TaskKey::new(
        resident_task::TaskFamily::Task,
        &task.id,
    ));
    owner.prepare_owner_task_request(&request).await
}
pub(crate) async fn write_child_task_for_test(
    runtime: &RuntimeHandle,
    task: &TaskRecord,
) -> Result<()> {
    runtime
        .persist_task_transition(task, "original_child_test_whole_ack")
        .await
}
pub(crate) async fn continue_child_monitor_for_test(
    runtime: &RuntimeHandle,
    source: Arc<OriginalChildTaskProducer>,
) -> Result<()> {
    runtime.continue_original_child_terminal(source).await
}

#[tokio::test]
async fn child_actual_git_cleanup_is_saved_before_audit_failure_and_never_repeated() -> Result<()> {
    fn git(path: &std::path::Path, args: &[&str]) -> Result<()> {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .output()?;
        anyhow::ensure!(
            output.status.success(),
            "Git fixture failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let repo = runtime.workspace_root();
    git(&repo, &["init", "--quiet"])?;
    git(&repo, &["config", "user.email", "child-fixture@localhost"])?;
    git(&repo, &["config", "user.name", "Child Fixture"])?;
    std::fs::write(repo.join("tracked"), "original\n")?;
    git(&repo, &["add", "tracked"])?;
    git(&repo, &["commit", "--quiet", "-m", "original"])?;
    let worktree_home = tempfile::tempdir()?;
    let path = worktree_home.path().join("worktree");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "child-original-result",
            path.to_str().unwrap(),
        ],
    )?;
    let at = runtime.now();
    let task = TaskRecord {
        id: crate::ids::task_id(),
        agent_id: "default".into(),
        kind: TaskKind::WorktreeSubagentTask,
        status: TaskStatus::Running,
        created_at: at,
        updated_at: at,
        parent_message_id: None,
        work_item_id: None,
        summary: Some("real Git child completion".into()),
        detail: Some(
            serde_json::json!({"workspace_mode":"worktree","worktree":{"worktree_path":path,"worktree_branch":"child-original-result"}}),
        ),
        recovery: Some(TaskRecoverySpec::ChildAgentTask {
            summary: "real Git child completion".into(),
            prompt: "accepted".into(),
            authority_class: AuthorityClass::OperatorInstruction,
            workspace_mode: ChildAgentWorkspaceMode::Worktree,
            lifecycle_disposition: AgentLifecycleDisposition::Retain,
        }),
    };
    write_child_task_for_test(runtime, &task).await?;
    let mut monitor =
        register_child_monitor_for_test(runtime, &task, None, None, None, true).await?;
    runtime.runtime_db().transaction(|tx|{tx.execute_batch("CREATE TRIGGER child_audit_reject BEFORE INSERT ON audit_events WHEN NEW.kind='worktree_auto_cleaned_up' BEGIN SELECT RAISE(ABORT,'original cleanup audit unavailable'); END;")?;Ok(())})?;
    monitor.start();
    assert!(monitor.finish().await?.is_err());
    assert!(
        !path.exists(),
        "real original Git removal completed before SQL failure"
    );
    let saved = monitor
        .source
        .completed_terminal_observation()
        .context("actual cleanup outcome must survive audit failure")?;
    assert_eq!(
        saved.task.detail.as_ref().unwrap()["worktree"]["cleanup_status"],
        "cleaned"
    );
    assert!(saved
        .audits
        .iter()
        .any(|event| event.kind == "worktree_auto_cleaned_up"));
    assert!(child_receipt(runtime, &monitor.source)?.is_none());
    assert_eq!(
        runtime
            .runtime_db()
            .tasks()
            .latest(&task.id)?
            .unwrap()
            .status,
        TaskStatus::Running
    );
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch("DROP TRIGGER child_audit_reject")?;
        Ok(())
    })?;
    runtime
        .continue_original_child_terminal(monitor.source.clone())
        .await?;
    let actual = runtime.runtime_db().tasks().latest(&task.id)?.unwrap();
    assert_eq!(actual.status, TaskStatus::Completed);
    assert_eq!(
        actual.detail.as_ref().unwrap()["worktree"]["cleanup_status"],
        "cleaned"
    );
    let counts:(u64,u64)=runtime.runtime_db().connection()?.query_row("SELECT (SELECT COUNT(*) FROM audit_events WHERE kind='worktree_auto_cleaned_up'),(SELECT COUNT(*) FROM audit_events WHERE kind='task_worktree_cleanup_already_removed')",[],|row|Ok((row.get(0)?,row.get(1)?)))?;
    assert_eq!(
        counts,
        (1, 0),
        "retry persists the original audit and never runs cleanup again"
    );
    drop(saved);
    assert!(monitor.source.is_durable_finished());
    assert!(monitor.source.completed_terminal_observation().is_none());
    Ok(())
}

#[tokio::test]
async fn child_current_entry_rejects_oversized_malformed_state_and_task_before_decode() -> Result<()>
{
    use crate::runtime_db::{
        task_snapshot::{TaskDomainLoadRequest, TaskSnapshotError},
        work_snapshot::WorkDomainLoadRequest,
    };
    for table in ["agent_states", "tasks"] {
        let f = child_monitor_fixture(Some(TransitionFaultPoint::BeforeCommit)).await?;
        f.start.send(()).unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(10), f.done)
            .await??
            .is_err());
        f.lease.join().await?;
        let frozen = f
            .source
            .frozen_terminal()?
            .context("rollback retains original frozen body")?;
        let request = TaskDomainLoadRequest {
            query: Default::default(),
            capture: resident_task::TaskCaptureRequest::current(),
            record_limit: 65_536,
            byte_limit: 1024 * 1024,
        };
        let prepared=f.runtime.runtime_db().transaction(|tx|{
            let inventory=crate::runtime_db::child_task_terminal::discover_original_child_observers_tx(tx,&f.source,&request,&f.runtime.inner.input_evidence_budget)?;
            crate::runtime_db::transitions::PreparedInputTransition::prepare_original_child_terminal_tx(tx,&f.source,&frozen,inventory,Vec::new(),None,&request,&f.runtime.inner.input_evidence_budget)
        })?;
        let bad = format!("{{malformed{}", "x".repeat(1024 * 1024));
        f.runtime.runtime_db().transaction(|tx| {
            match table {
                "agent_states" => {
                    tx.execute(
                        "UPDATE agent_states SET payload_json=?1 WHERE agent_id='default'",
                        [&bad],
                    )?;
                }
                _ => {
                    tx.execute(
                        "UPDATE tasks SET payload_json=?1 WHERE task_id=?2",
                        rusqlite::params![&bad, &f.task.id],
                    )?;
                }
            }
            Ok(())
        })?;
        let before = f
            .runtime
            .inner
            .input_evidence_budget
            .report()?
            .observed_bytes;
        let work = WorkDomainLoadRequest {
            capture: resident_work::WorkCaptureRequest::current(),
            record_limit: request.record_limit,
            byte_limit: request.byte_limit,
        };
        let result = f
            .runtime
            .runtime_db()
            .transitions_in_scope(&f.source.address().scope)
            .commit_prepared_transition_with_work_request_and_memory_budget(
                &prepared,
                &work,
                &f.runtime.inner.input_evidence_budget,
                &request,
            );
        let error = match result {
            Ok(_) => anyhow::bail!("oversized malformed Current body committed"),
            Err(error) => error,
        };
        assert!(
            error
                .downcast_ref::<TaskSnapshotError>()
                .is_some_and(|error| matches!(error, TaskSnapshotError::ResourceExhausted(_))),
            "{table} must hit the typed resource gate before serde/SQLite JSON: {error:#}"
        );
        assert!(child_receipt(&f.runtime, &f.source)?.is_none());
        assert_eq!(
            f.runtime
                .inner
                .input_evidence_budget
                .report()?
                .observed_bytes,
            before,
            "failed temporary body view releases all guards"
        );
    }
    Ok(())
}

#[tokio::test]
async fn child_fresh_receipt_and_primary_joint_task_material_share_one_remaining_allowance(
) -> Result<()> {
    use crate::runtime_db::{
        child_task_terminal::OriginalChildTerminalRecoveryMaterial,
        task_snapshot::{TaskDomainLoadRequest, TaskSnapshotError},
    };
    fn read_primary(
        runtime: &RuntimeHandle,
        tx: &rusqlite::Transaction<'_>,
        reader: &mut OriginalChildTerminalRecoveryMaterial<'_, '_, '_>,
    ) -> Result<crate::runtime_db::task_snapshot::TaskMaintenanceMaterial> {
        let limits = (reader.request().record_limit, reader.request().byte_limit);
        let state = reader.read_primary_state(tx)?;
        let version = runtime.inner.config_snapshot.load_full();
        let context = version
            .model_catalog
            .resolved_context_config(&version.base_context_config, state.model_override.as_ref());
        let model = version
            .model_catalog
            .effective_model(state.model_override.as_ref());
        let history = super::turn::context_history_preparation(&state.id, &model);
        let input = super::context_capture::context_input_evidence_request(
            &state,
            None,
            &context,
            history,
            Default::default(),
            limits.0,
            limits.1,
        )?;
        reader.observe_primary_selectors(tx)?;
        let work = resident_work::WorkCaptureRequest::current();
        let tasks = resident_task::TaskCaptureRequest::current();
        let current = reader.read_primary_input_domains(
            tx,
            &state,
            &input,
            context.recent_episode_candidates,
            &work,
            &tasks,
            None,
            None,
        )?;
        assert!(
            current.tasks.is_some(),
            "the real primary joint Task loader must consume the shared material"
        );
        Ok(reader.material_for_test())
    }
    let f = child_monitor_fixture(Some(TransitionFaultPoint::BeforeCacheUpdate)).await?;
    f.start.send(()).unwrap();
    assert!(tokio::time::timeout(Duration::from_secs(10), f.done)
        .await??
        .is_err());
    f.lease.join().await?;
    let reservation = f.runtime.inner.mutation_owner.reserve().await?;
    let recovery = reservation
        .recovery()?
        .context("actual lost publication keeps original finished attempt")?;
    let witness = recovery
        .witness
        .as_ref()
        .context("actual original witness missing")?;
    let attempt = witness
        .input_attempt()
        .context("actual original attempt missing")?;
    let request = TaskDomainLoadRequest {
        query: Default::default(),
        capture: resident_task::TaskCaptureRequest::current(),
        record_limit: 65_536,
        byte_limit: 64 * 1024 * 1024,
    };
    let (evidence, (overhead, primary)) = f
        .runtime
        .runtime_db()
        .with_original_child_terminal_recovery(
            attempt,
            &f.source,
            &request,
            &f.runtime.inner.input_evidence_budget,
            |tx, _, reader| {
                let overhead = request.byte_limit - reader.request().byte_limit;
                let material = read_primary(&f.runtime, tx, reader)?;
                Ok((overhead, material.bytes))
            },
        )?
        .context("actual original receipt must be present")?;
    drop(evidence);
    let combined = overhead
        .checked_add(primary)
        .context("fixture combined material overflow")?;
    assert!(overhead > 0 && primary > 0);
    let mut bounded = request;
    bounded.byte_limit = combined - 1;
    assert!(
        overhead < bounded.byte_limit && primary < bounded.byte_limit,
        "each acquisition fits, but the original phase as a whole must not"
    );
    let before = f
        .runtime
        .inner
        .input_evidence_budget
        .report()?
        .observed_bytes;
    let result = f
        .runtime
        .runtime_db()
        .with_original_child_terminal_recovery(
            attempt,
            &f.source,
            &bounded,
            &f.runtime.inner.input_evidence_budget,
            |tx, _, reader| read_primary(&f.runtime, tx, reader),
        );
    let error = match result {
        Err(error) => error,
        Ok(_) => anyhow::bail!("receipt and primary Task loader obtained separate full allowances"),
    };
    assert!(
        error
            .downcast_ref::<TaskSnapshotError>()
            .is_some_and(|error| matches!(error, TaskSnapshotError::ResourceExhausted(_))),
        "{error:#}"
    );
    assert_eq!(
        before,
        f.runtime
            .inner
            .input_evidence_budget
            .report()?
            .observed_bytes,
        "failed fresh preparation releases its new guards"
    );
    assert_eq!(
        child_receipt(&f.runtime, &f.source)?.unwrap().prepared,
        *attempt.prepared()
    );
    assert!(
        !f.source.is_durable_finished(),
        "failed read cannot acknowledge the original capsule"
    );
    drop(reservation);
    f.runtime
        .continue_original_child_terminal(f.source.clone())
        .await?;
    assert!(f.source.is_durable_finished());
    Ok(())
}

#[tokio::test]
async fn child_full_state_native_nodes_are_rejected_before_all_three_decoders() -> Result<()> {
    use crate::runtime_db::task_snapshot::{TaskDomainLoadRequest, TaskSnapshotError};
    for phase in ["prepare", "entry", "fresh"] {
        let f = child_monitor_fixture(Some(if phase == "fresh" {
            TransitionFaultPoint::BeforeCacheUpdate
        } else {
            TransitionFaultPoint::BeforeCommit
        }))
        .await?;
        f.start.send(()).unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(10), f.done)
            .await??
            .is_err());
        f.lease.join().await?;
        let frozen = f.source.frozen_terminal()?.unwrap();
        let request = TaskDomainLoadRequest {
            query: Default::default(),
            capture: resident_task::TaskCaptureRequest::current(),
            record_limit: 65_536,
            byte_limit: 1024 * 1024,
        };
        let prepare = |tx: &rusqlite::Transaction<'_>| {
            let inventory =
                crate::runtime_db::child_task_terminal::discover_original_child_observers_tx(
                    tx,
                    &f.source,
                    &request,
                    &f.runtime.inner.input_evidence_budget,
                )?;
            crate::runtime_db::transitions::PreparedInputTransition::prepare_original_child_terminal_tx(tx,&f.source,&frozen,inventory,Vec::new(),None,&request,&f.runtime.inner.input_evidence_budget)
        };
        let prepared = if phase == "entry" {
            Some(f.runtime.runtime_db().transaction(prepare)?)
        } else {
            None
        };
        f.runtime.runtime_db().transaction(|tx| {
            let raw: String = tx.query_row(
                "SELECT payload_json FROM agent_states WHERE agent_id='default'",
                [],
                |row| row.get(0),
            )?;
            let mut body: serde_json::Value = serde_json::from_str(&raw)?;
            body["working_memory"]["current_working_memory"]["waiting_on"] =
                serde_json::json!(vec![""; 20_000]);
            let raw = serde_json::to_string(&body)?;
            assert!(
                raw.len() * 8 < request.byte_limit,
                "raw-only preflight must fit this actual WorkingMemory fixture"
            );
            tx.execute(
                "UPDATE agent_states SET payload_json=?1 WHERE agent_id='default'",
                [raw],
            )?;
            Ok(())
        })?;
        let before = f
            .runtime
            .inner
            .input_evidence_budget
            .report()?
            .observed_bytes;
        let error = if phase == "prepare" {
            match f.runtime.runtime_db().transaction(prepare) {
                Err(error) => error,
                Ok(_) => anyhow::bail!("State nodes decoded in preparation"),
            }
        } else if phase == "entry" {
            let work = crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
                capture: resident_work::WorkCaptureRequest::current(),
                record_limit: request.record_limit,
                byte_limit: request.byte_limit,
            };
            match f
                .runtime
                .runtime_db()
                .transitions_in_scope(&f.source.address().scope)
                .commit_prepared_transition_with_work_request_and_memory_budget(
                    prepared.as_ref().unwrap(),
                    &work,
                    &f.runtime.inner.input_evidence_budget,
                    &request,
                ) {
                Err(error) => error,
                Ok(_) => anyhow::bail!("State nodes decoded at Current entry"),
            }
        } else {
            let held = f.runtime.inner.mutation_owner.reserve().await?;
            let recovery = held.recovery()?.unwrap();
            let witness = recovery.witness.as_ref().unwrap();
            match f
                .runtime
                .runtime_db()
                .with_original_child_terminal_recovery(
                    witness.input_attempt().unwrap(),
                    &f.source,
                    &request,
                    &f.runtime.inner.input_evidence_budget,
                    |tx, _, reader| reader.read_primary_state(tx).map(|_| ()),
                ) {
                Err(error) => error,
                Ok(_) => anyhow::bail!("State nodes decoded in fresh recovery"),
            }
        };
        assert!(
            error
                .downcast_ref::<TaskSnapshotError>()
                .is_some_and(|error| matches!(error, TaskSnapshotError::ResourceExhausted(_))),
            "{phase}: {error:#}"
        );
        assert_eq!(
            before,
            f.runtime
                .inner
                .input_evidence_budget
                .report()?
                .observed_bytes
        );
        assert!(!f.source.is_durable_finished());
    }
    Ok(())
}
#[tokio::test]
async fn child_receipt_nested_ack_nodes_are_budgeted_before_lookup_and_checksum() -> Result<()> {
    use crate::runtime_db::task_snapshot::{TaskDomainLoadRequest, TaskSnapshotError};
    for column in ["outcome_json", "prepared_json", "scopes_json"] {
        let f = child_monitor_fixture(Some(TransitionFaultPoint::BeforeCacheUpdate)).await?;
        f.start.send(()).unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(10), f.done)
            .await??
            .is_err());
        f.lease.join().await?;
        let held = f.runtime.inner.mutation_owner.reserve().await?;
        let recovery = held.recovery()?.unwrap();
        let witness = recovery.witness.as_ref().unwrap();
        let attempt = witness.input_attempt().unwrap();
        f.runtime.runtime_db().transaction(|tx| {
        let (prepared_raw,outcome_raw,scopes_raw):(String,String,String)=tx.query_row("SELECT prepared_json,outcome_json,scopes_json FROM input_mutation_receipts WHERE command_nonce=?1",[attempt.prepared().command_nonce()],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
        let mut prepared:serde_json::Value=serde_json::from_str(&prepared_raw)?;let mut outcome:serde_json::Value=serde_json::from_str(&outcome_raw)?;
        let scopes:serde_json::Value=serde_json::from_str(&scopes_raw)?;
        let raw=match column {
            "outcome_json"=>{let row=outcome["result"]["ack"]["status_scopes"][0].clone();outcome["result"]["ack"]["status_scopes"]=serde_json::json!(vec![row;4096]);serde_json::to_string(&outcome)?},
            "prepared_json"=>{let scope=prepared["scopes"][0].clone();prepared["scopes"]=serde_json::json!(vec![scope;4096]);serde_json::to_string(&prepared)?},
            _=>serde_json::to_string(&vec![scopes[0].clone();4096])?,
        };
        match column {"outcome_json"=>tx.execute("UPDATE input_mutation_receipts SET outcome_json=?1 WHERE command_nonce=?2",rusqlite::params![raw,attempt.prepared().command_nonce()])?,"prepared_json"=>tx.execute("UPDATE input_mutation_receipts SET prepared_json=?1 WHERE command_nonce=?2",rusqlite::params![raw,attempt.prepared().command_nonce()])?,_=>tx.execute("UPDATE input_mutation_receipts SET scopes_json=?1 WHERE command_nonce=?2",rusqlite::params![raw,attempt.prepared().command_nonce()])?};
        Ok(())
    })?;
        let request = TaskDomainLoadRequest {
            query: Default::default(),
            capture: resident_task::TaskCaptureRequest::current(),
            record_limit: 4096,
            byte_limit: 16 * 1024 * 1024,
        };
        let before = f
            .runtime
            .inner
            .input_evidence_budget
            .report()?
            .observed_bytes;
        let fresh = f
            .runtime
            .runtime_db()
            .with_original_child_terminal_recovery(
                attempt,
                &f.source,
                &request,
                &f.runtime.inner.input_evidence_budget,
                |_, _, _| -> Result<()> { anyhow::bail!("lookup must not reach callback") },
            );
        let error = match fresh {
            Err(error) => error,
            Ok(_) => anyhow::bail!("oversized Child ACK decoded"),
        };
        assert!(
            error
                .downcast_ref::<TaskSnapshotError>()
                .is_some_and(|error| matches!(error, TaskSnapshotError::ResourceExhausted(_))),
            "{error:#}"
        );
        let pending = f.source.prepared.lock().await;
        let prepared = pending.as_deref().unwrap();
        let work = crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
            capture: resident_work::WorkCaptureRequest::current(),
            record_limit: request.record_limit,
            byte_limit: request.byte_limit,
        };
        let replay = f
            .runtime
            .runtime_db()
            .transitions_in_scope(&f.source.address().scope)
            .commit_prepared_transition_with_work_request_and_memory_budget(
                prepared,
                &work,
                &f.runtime.inner.input_evidence_budget,
                &request,
            );
        let error = match replay {
            Err(error) => error,
            Ok(_) => anyhow::bail!("direct Child replay decoded oversized ACK"),
        };
        assert!(
            error
                .downcast_ref::<TaskSnapshotError>()
                .is_some_and(|error| matches!(error, TaskSnapshotError::ResourceExhausted(_))),
            "{error:#}"
        );
        assert_eq!(
            before,
            f.runtime
                .inner
                .input_evidence_budget
                .report()?
                .observed_bytes
        );
        assert!(!f.source.is_durable_finished());
    }
    Ok(())
}

#[tokio::test]
async fn child_prebind_failure_retains_original_monitor_through_reap_and_actual_terminal_retry(
) -> Result<()> {
    let (_home, host, runtime) = crate::runtime::tests::support::host_backed_test_runtime().await;
    let parent_id = runtime.agent_state().await?.id;
    let target = host
        .create_named_agent("original-monitor-target", None)
        .await?;
    let child = host.get_public_agent(&target.agent_id).await?;
    let at = runtime.now();
    let detail = serde_json::json!({"target_agent_id":target.agent_id,
        "target_identity_incarnation":target.incarnation,"created_new_subagent":false});
    let task = TaskRecord {
        id: crate::ids::task_id(),
        agent_id: parent_id,
        kind: TaskKind::ActorInvocation,
        status: TaskStatus::Running,
        created_at: at,
        updated_at: at,
        parent_message_id: None,
        work_item_id: None,
        summary: Some("original prebind monitor".into()),
        detail: Some(detail.clone()),
        recovery: Some(TaskRecoverySpec::AgentInvocation {
            summary: "original prebind monitor".into(),
            prompt: "real target completion".into(),
            authority_class: AuthorityClass::OperatorInstruction,
            target_agent_id: Some(target.agent_id.clone()),
            created_new_subagent: false,
            workspace_mode: ChildAgentWorkspaceMode::Inherit,
            lifecycle_disposition: AgentLifecycleDisposition::Retain,
        }),
    };
    write_child_task_for_test(&runtime, &task).await?;
    let (registration, lease, source) = runtime.admit_original_child_monitor(&task).await?;
    let original_status: String = runtime.runtime_db().connection()?.query_row(
        "SELECT status FROM agent_identities WHERE agent_id=?1",
        [&target.agent_id],
        |r| r.get(0),
    )?;
    runtime.runtime_db().transaction(|tx| {
        tx.execute(
            "UPDATE agent_identities SET status='deleted' WHERE agent_id=?1",
            [&target.agent_id],
        )?;
        Ok(())
    })?;
    let (send, done) = oneshot::channel();
    let monitor_runtime = runtime.clone();
    let monitor_source = source.clone();
    let monitor_task = task.clone();
    let target_id = target.agent_id.clone();
    lease.attach(tokio::spawn(async move {
        let result = monitor_runtime
            .monitor_spawned_child_agent_task(
                monitor_task,
                monitor_source,
                AuthorityClass::OperatorInstruction,
                false,
                false,
                AgentLifecycleDisposition::Retain,
                target_id,
                0,
                None,
                detail,
            )
            .await;
        let _ = send.send(result);
    }));
    runtime.inner.task_handles.lock().await.insert(
        task.id.clone(),
        command_task::ManagedTaskHandle::Async(SharedControlTaskHandle(lease.clone())),
    );
    drop(registration);
    let error = tokio::time::timeout(Duration::from_secs(10), done)
        .await??
        .err();
    // Written without manufacturing a failed terminal result: the original stage remains pre-observation.
    let error = error.context("actual malformed target must fail before observation")?;
    assert!(
        format!("{error:#}").contains("child target identity normalized projection differs"),
        "{error:#}"
    );
    lease.join().await?;
    assert!(source.monitor_stage().is_some());
    assert!(source.observed_terminal().is_none());
    assert!(source.has_terminal_responsibility());
    assert!(child_receipt(&runtime, &source)?.is_none());
    let running_before:usize=runtime.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM messages WHERE kind='task_status' AND json_extract(payload_json,'$.metadata.task_id')=?1",[&task.id],|r|r.get(0))?;
    assert_eq!(running_before, 0, "an invalid binding cannot emit Running");
    // A genuine second registration runs the real registry reap pass.
    let mut another = task.clone();
    another.id = crate::ids::task_id();
    write_child_task_for_test(&runtime, &another).await?;
    let mut other =
        register_child_monitor_for_test(&runtime, &another, None, None, None, false).await?;
    assert!(runtime
        .inner
        .control_cancellations
        .contains_original_child_lease_for_test(&lease));
    runtime.runtime_db().transaction(|tx| {
        tx.execute(
            "UPDATE agent_identities SET status=?2 WHERE agent_id=?1",
            rusqlite::params![target.agent_id, original_status],
        )?;
        Ok(())
    })?;
    let retry_runtime = runtime.clone();
    let retry_source = source.clone();
    let retry = tokio::spawn(async move {
        retry_runtime
            .continue_original_child_terminal(retry_source)
            .await
    });
    // Actual Running dispatch must preserve the sealed birth before this same
    // original monitor later reports its real terminal result.
    tokio::time::timeout(Duration::from_secs(10),async {
        loop {
            let processed:bool=runtime.runtime_db().connection()?.query_row(
                "SELECT EXISTS(SELECT 1 FROM queue_entries q JOIN messages m ON m.evidence_id=q.message_id WHERE m.kind='task_status' AND json_extract(m.payload_json,'$.metadata.task_id')=?1 AND q.status='processed')",[&task.id],|r|r.get(0))?;
            if processed {return Ok::<_,anyhow::Error>(())}
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await??;
    assert_eq!(
        runtime
            .runtime_db()
            .tasks()
            .latest(&task.id)?
            .unwrap()
            .created_at,
        task.created_at
    );
    let running_raw:String=runtime.runtime_db().connection()?.query_row(
        "SELECT payload_json FROM messages WHERE kind='task_status' AND json_extract(payload_json,'$.metadata.task_id')=?1",[&task.id],|r|r.get(0))?;
    let mut forged: MessageEnvelope = serde_json::from_str(&running_raw)?;
    assert_eq!(
        runtime
            .inner
            .control_cancellations
            .original_child_running_status_clock(&forged)?,
        Some(task.created_at)
    );
    forged.metadata.as_mut().unwrap()["task_created_at"] =
        serde_json::json!(task.created_at - chrono::Duration::days(7));
    assert!(
        runtime
            .inner
            .control_cancellations
            .original_child_running_status_clock(&forged)
            .is_err(),
        "same address altered header is not the authentic frozen source"
    );
    forged.id = crate::ids::message_id();
    assert_eq!(
        runtime
            .inner
            .control_cancellations
            .original_child_running_status_clock(&forged)?,
        None,
        "foreign metadata header cannot carry the original producer clock"
    );
    assert_eq!(
        super::tasks::task_from_message(&forged, &task.agent_id)?
            .created_at
            .date_naive(),
        runtime.now().date_naive(),
        "generic parser keeps its existing clock contract"
    );
    assert_eq!(
        runtime
            .runtime_db()
            .tasks()
            .latest(&task.id)?
            .unwrap()
            .status,
        TaskStatus::Running
    );
    // The child has real accepted work. Its result is observed by the original Host wait.
    child
        .enqueue(MessageEnvelope::new(
            target.agent_id.clone(),
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "complete the original monitor fixture".into(),
            },
        ))
        .await?;
    let actual_child = tokio::time::timeout(
        Duration::from_secs(15),
        runtime
            .inner
            .host_bridge
            .as_ref()
            .unwrap()
            .await_agent_invocation_terminal_result(&target.agent_id, 0, None, &task.id, false),
    )
    .await??;
    assert_eq!(actual_child.status, TaskStatus::Completed);
    other.start();
    other.finish().await??;
    tokio::time::timeout(Duration::from_secs(15), retry).await???;
    assert!(source.is_durable_finished());
    assert!(source.monitor_stage().is_none());
    assert!(source.observed_terminal().is_none());
    assert!(!source.has_terminal_responsibility());
    assert_eq!(
        runtime
            .runtime_db()
            .tasks()
            .latest(&task.id)?
            .unwrap()
            .status,
        TaskStatus::Completed
    );
    let counts:(u64,u64,u64)=runtime.runtime_db().connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM messages WHERE kind='task_status' AND json_extract(payload_json,'$.metadata.task_id')=?1),(SELECT COUNT(*) FROM messages WHERE kind='task_result' AND json_extract(payload_json,'$.task_id')=?1),(SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce=?2)",
        rusqlite::params![task.id,format!("child-terminal:{}",source.address().lease_id)],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    assert_eq!(counts, (1, 1, 1));
    assert!(runtime
        .inner
        .task_handles
        .lock()
        .await
        .get(&task.id)
        .is_none());
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn child_cancelled_pre_result_monitor_never_restarts_wait_and_retains_original_retirement_responsibility(
) -> Result<()> {
    let (_home, _host, runtime) = crate::runtime::tests::support::host_backed_test_runtime().await;
    let parent_id = runtime.agent_state().await?.id;
    let target = _host
        .create_named_agent("unfinished-original-monitor-target", None)
        .await?;
    let child = _host.get_public_agent(&target.agent_id).await?;
    assert_eq!(child.agent_state().await?.turn_index, 0);
    let at = runtime.now();
    let detail = serde_json::json!({"target_agent_id":target.agent_id,
        "target_identity_incarnation":target.incarnation,"created_new_subagent":false});
    let task = TaskRecord {
        id: crate::ids::task_id(),
        agent_id: parent_id,
        kind: TaskKind::ActorInvocation,
        status: TaskStatus::Running,
        created_at: at,
        updated_at: at,
        parent_message_id: None,
        work_item_id: None,
        summary: Some("unfinished original monitor".into()),
        detail: Some(detail.clone()),
        recovery: Some(TaskRecoverySpec::AgentInvocation {
            summary: "unfinished original monitor".into(),
            prompt: "accepted original target".into(),
            authority_class: AuthorityClass::OperatorInstruction,
            target_agent_id: Some(target.agent_id.clone()),
            created_new_subagent: false,
            workspace_mode: ChildAgentWorkspaceMode::Inherit,
            lifecycle_disposition: AgentLifecycleDisposition::Retain,
        }),
    };
    write_child_task_for_test(&runtime, &task).await?;
    let (registration, lease, source) = runtime.admit_original_child_monitor(&task).await?;
    let monitor_runtime = runtime.clone();
    let monitor_source = source.clone();
    let original = task.clone();
    let target_id = target.agent_id.clone();
    lease.attach(tokio::spawn(async move {
        let _ = monitor_runtime
            .monitor_spawned_child_agent_task(
                original,
                monitor_source,
                AuthorityClass::OperatorInstruction,
                false,
                false,
                AgentLifecycleDisposition::Retain,
                target_id,
                0,
                None,
                detail,
            )
            .await;
    }));
    runtime.inner.task_handles.lock().await.insert(
        task.id.clone(),
        command_task::ManagedTaskHandle::Async(SharedControlTaskHandle(lease.clone())),
    );
    drop(registration);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let bound = {
                let view = source.source_view()?;
                view.target().is_some()
            };
            if bound {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    assert!(source.monitor_stage().is_some());
    assert!(source.observed_terminal().is_none());
    runtime
        .retire_instance_locally("original monitor cancellation fixture")
        .await;
    let error = tokio::time::timeout(
        Duration::from_secs(2),
        runtime
            .inner
            .control_cancellations
            .drain_for_original_runtime(&runtime, true),
    )
    .await?
    .expect_err("physical cancellation cannot discharge durable original monitor responsibility");
    assert!(
        error
            .downcast_ref::<super::child_task_terminal::OriginalChildMonitorCancellationPending>()
            .is_some(),
        "{error:#}"
    );
    let again = tokio::time::timeout(
        Duration::from_secs(2),
        runtime.continue_original_child_terminal(source.clone()),
    )
    .await?
    .expect_err(
        "inline retry must observe the original cancellation instead of starting a new child wait",
    );
    assert!(
        again
            .downcast_ref::<super::child_task_terminal::OriginalChildMonitorCancellationPending>()
            .is_some(),
        "{again:#}"
    );
    assert!(source.has_terminal_responsibility());
    assert!(!source.is_durable_finished());
    assert!(source.monitor_stage().is_some());
    assert!(source.observed_terminal().is_none());
    assert!(source.frozen_terminal()?.is_none());
    assert!(child_receipt(&runtime, &source)?.is_none());
    assert!(runtime
        .inner
        .control_cancellations
        .contains_original_child_lease_for_test(&lease));
    assert_eq!(
        child.agent_state().await?.turn_index,
        0,
        "no child terminal outcome was invented"
    );
    assert_eq!(
        runtime
            .runtime_db()
            .tasks()
            .latest(&task.id)?
            .unwrap()
            .status,
        TaskStatus::Running
    );
    Ok(())
}

#[tokio::test]
async fn actual_creator_without_current_turn_keeps_child_terminal_partial_contract_without_fake_rejoin(
) -> Result<()> {
    let (_home, host, runtime) = crate::runtime::tests::support::host_backed_test_runtime().await;
    assert!(runtime.agent_state().await?.current_turn_id.is_none());
    let target = host
        .create_named_agent("partial-original-child-terminal-target", None)
        .await?;
    let child = host.get_public_agent(&target.agent_id).await?;
    let task = runtime
        .create_agent_invocation_task(
            "actual no-Turn creator".into(),
            "original invocation".into(),
            AuthorityClass::OperatorInstruction,
            Some(target.agent_id.clone()),
            false,
            ChildAgentWorkspaceMode::Inherit,
        )
        .await?;
    assert_eq!(
        task.detail
            .as_ref()
            .unwrap()
            .get("rejoin_obligation_id")
            .and_then(serde_json::Value::as_str),
        Some(task.id.as_str())
    );
    assert_eq!(
        task.detail
            .as_ref()
            .unwrap()
            .get("rejoin_generation")
            .and_then(serde_json::Value::as_u64),
        Some(1)
    );
    assert!(task
        .detail
        .as_ref()
        .unwrap()
        .get("parent_turn_id")
        .is_none());
    assert!(task.rejoin_fence().is_err());
    child
        .enqueue(MessageEnvelope::new(
            target.agent_id.clone(),
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "complete the genuinely observed invocation".into(),
            },
        ))
        .await?;
    let bridge = runtime.inner.host_bridge.as_ref().unwrap();
    let actual = tokio::time::timeout(
        Duration::from_secs(15),
        bridge.await_agent_invocation_terminal_result(&target.agent_id, 0, None, &task.id, false),
    )
    .await??;
    assert_eq!(actual.status, TaskStatus::Completed);
    let (registration, lease, source) = runtime.admit_original_child_monitor(&task).await?;
    let (send, start) = oneshot::channel();
    let (completed, done) = oneshot::channel();
    let runner = source.runner_material()?;
    let producer = source.clone();
    let monitor = runtime.clone();
    let original = task.clone();
    let target_id = target.agent_id.clone();
    let incarnation = target.incarnation;
    lease.attach(tokio::spawn(async move {
        let _runner=runner;
        if start.await.is_err(){return}
        let result=monitor.monitor_spawned_child_agent_task(original,producer,AuthorityClass::OperatorInstruction,
            false,false,AgentLifecycleDisposition::Retain,target_id.clone(),0,None,
            serde_json::json!({"target_agent_id":target_id,"target_identity_incarnation":incarnation,
                "child_turn_baseline":0,"created_new_subagent":false})).await;
        let _=completed.send(result);
    }));
    runtime.inner.task_handles.lock().await.insert(
        task.id.clone(),
        command_task::ManagedTaskHandle::Async(SharedControlTaskHandle(lease.clone())),
    );
    drop(registration);
    // Establish the newly required actual binding before closing physical
    // admission. The already accepted terminal still closes under that fence.
    source.retain_monitor_stage(task.clone(),AuthorityClass::OperatorInstruction,false,false,AgentLifecycleDisposition::Retain,target.agent_id.clone(),0,None,
        serde_json::json!({"target_agent_id":target.agent_id,"target_identity_incarnation":target.incarnation,"child_turn_baseline":0,"created_new_subagent":false}))?;
    runtime.bind_original_child_monitor_target(&source, &target.agent_id, None, 0)?;
    assert!(
        runtime
            .continue_original_child_monitor_binding(source.clone())
            .await?
    );
    runtime.close_execution_admission();
    assert!(
        runtime.spawn_owned_mutation(async {}).is_err(),
        "closed admission still rejects an ordinary new mutation worker"
    );
    send.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(15), done).await???;
    lease.join().await?;
    let stored = runtime
        .runtime_db()
        .tasks()
        .latest(&task.id)?
        .context("actual canonical terminal missing")?;
    assert_eq!(stored.status, TaskStatus::Completed);
    assert_eq!(stored.created_at, task.created_at);
    assert_eq!(
        stored.detail.as_ref().unwrap().get("rejoin_obligation_id"),
        task.detail.as_ref().unwrap().get("rejoin_obligation_id")
    );
    assert_eq!(
        stored.detail.as_ref().unwrap().get("rejoin_generation"),
        task.detail.as_ref().unwrap().get("rejoin_generation")
    );
    assert!(stored
        .detail
        .as_ref()
        .unwrap()
        .get("parent_turn_id")
        .is_none());
    assert!(
        stored.rejoin_fence().is_err(),
        "a Result provenance turn must not create a new rejoin contract"
    );
    let raw: String = runtime.runtime_db().connection()?.query_row(
        "SELECT payload_json FROM messages WHERE evidence_id=?1",
        [stored
            .parent_message_id
            .as_ref()
            .context("actual Result address missing")?],
        |r| r.get(0),
    )?;
    let result: MessageEnvelope = serde_json::from_str(&raw)?;
    assert!(
        result.turn_id.is_some(),
        "Message source provenance is retained"
    );
    assert_eq!(result.task_id.as_deref(), Some(task.id.as_str()));
    assert!(result
        .metadata
        .as_ref()
        .unwrap()
        .get("task_detail")
        .unwrap()
        .get("parent_turn_id")
        .is_none());
    let settlements: u64 = runtime.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM task_result_settlements WHERE task_id=?1",
        [&task.id],
        |r| r.get(0),
    )?;
    assert_eq!(
        settlements, 0,
        "unbound accepted Result carries no fabricated rejoin settlement"
    );
    assert!(source.is_durable_finished());
    assert!(source.observed_terminal().is_none());
    assert!(child_receipt(&runtime, &source)?.is_some());
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn actual_task_ack_cannot_change_partial_generation_under_an_original_child_source(
) -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let original = runtime
        .create_agent_invocation_task(
            "original partial generation".into(),
            "accepted original".into(),
            AuthorityClass::OperatorInstruction,
            Some("original-target".into()),
            false,
            ChildAgentWorkspaceMode::Inherit,
        )
        .await?;
    assert_eq!(
        super::tasks::unbound_task_rejoin_generation(&original),
        Some(1)
    );
    let (registration, lease, source) = runtime.admit_original_child_monitor(&original).await?;
    let (_send, done) = oneshot::channel::<()>();
    lease.attach(tokio::spawn(async move {
        let _ = done.await;
    }));
    drop(registration);
    let mut changed = original.clone();
    changed.detail.as_mut().unwrap()["rejoin_generation"] = serde_json::json!(2);
    changed.updated_at = original.updated_at + chrono::Duration::milliseconds(1);
    write_child_task_for_test(runtime, &changed).await?;
    let current = runtime.capture_current_task_record(&original.id)?;
    let actual = current
        .record()
        .context("actual new qualified Task missing")?;
    assert_eq!(
        super::tasks::unbound_task_rejoin_generation(actual),
        Some(2)
    );
    let error = source
        .source_view()?
        .validate_generation(actual)
        .expect_err("old source cannot authorize a different valid partial generation");
    assert!(format!("{error:#}").contains("original Task generation"));
    // DB Current/sparse constructors use this same sealed source view validator,
    // so no second None-based DB comparison can authorize the stale producer.
    let keys = std::collections::BTreeSet::from([source.address().admitted.key.clone()]);
    let material = runtime.runtime_db().transaction(|tx| {
        crate::runtime_db::task_snapshot::task_maintenance_material_tx(
            tx,
            &source.address().scope,
            &keys,
        )
    })?;
    let request = crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
        query: Default::default(),
        capture: resident_task::TaskCaptureRequest::current(),
        record_limit: 65_536,
        byte_limit: 64 * 1024 * 1024,
    };
    let _guard = crate::runtime_db::command_task_terminal::charge_material(
        &source.address().scope,
        material.bytes,
        material.records,
        &request,
        &runtime.inner.input_evidence_budget,
    )?;
    let error = runtime
        .runtime_db()
        .transaction(|tx| {
            let proof = crate::runtime_db::task_snapshot::prepare_existing_task_maintenance_tx(
                tx,
                &source.address().scope,
                &keys,
            )?;
            source
                .source_view()?
                .validate_generation(proof.qualified_task_before(&original.id)?)
        })
        .expect_err(
            "actual qualified-before lookup must also reject the original partial generation",
        );
    assert!(format!("{error:#}").contains("original Task generation"));
    assert!(source.observed_terminal().is_none());
    assert!(source.frozen_terminal()?.is_none());
    assert!(child_receipt(runtime, &source)?.is_none());
    assert_eq!(
        runtime
            .runtime_db()
            .tasks()
            .latest(&original.id)?
            .unwrap()
            .status,
        TaskStatus::Queued
    );
    lease.cancel();
    lease.join().await?;
    Ok(())
}

#[tokio::test]
async fn child_original_public_stop_whole_ack_takes_over_unobserved_monitor_without_child_result(
) -> Result<()> {
    let (_home, host, runtime) = crate::runtime::tests::support::host_backed_test_runtime().await;
    let parent_id = runtime.agent_state().await?.id;
    let target = host
        .create_named_agent("stopped-unfinished-monitor-target", None)
        .await?;
    let child = host.get_public_agent(&target.agent_id).await?;
    let at = runtime.now();
    let detail = serde_json::json!({"target_agent_id":target.agent_id,"target_identity_incarnation":target.incarnation,"created_new_subagent":false});
    let task = TaskRecord {
        id: crate::ids::task_id(),
        agent_id: parent_id,
        kind: TaskKind::ActorInvocation,
        status: TaskStatus::Running,
        created_at: at,
        updated_at: at,
        parent_message_id: None,
        work_item_id: None,
        summary: Some("original stopped monitor".into()),
        detail: Some(detail.clone()),
        recovery: Some(TaskRecoverySpec::AgentInvocation {
            summary: "original stopped monitor".into(),
            prompt: "accepted".into(),
            authority_class: AuthorityClass::OperatorInstruction,
            target_agent_id: Some(target.agent_id.clone()),
            created_new_subagent: false,
            workspace_mode: ChildAgentWorkspaceMode::Inherit,
            lifecycle_disposition: AgentLifecycleDisposition::Retain,
        }),
    };
    write_child_task_for_test(&runtime, &task).await?;
    let (registration, lease, source) = runtime.admit_original_child_monitor(&task).await?;
    let monitor = runtime.clone();
    let original = task.clone();
    let monitor_source = source.clone();
    let target_id = target.agent_id.clone();
    lease.attach(tokio::spawn(async move {
        let _ = monitor
            .monitor_spawned_child_agent_task(
                original,
                monitor_source,
                AuthorityClass::OperatorInstruction,
                false,
                false,
                AgentLifecycleDisposition::Retain,
                target_id,
                0,
                None,
                detail,
            )
            .await;
    }));
    runtime.inner.task_handles.lock().await.insert(
        task.id.clone(),
        command_task::ManagedTaskHandle::Async(SharedControlTaskHandle(lease.clone())),
    );
    drop(registration);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let bound = {
                let view = source.source_view()?;
                view.target().is_some()
            };
            if bound {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    let stopped = runtime
        .stop_task(&task.id, &AuthorityClass::OperatorInstruction)
        .await?;
    assert_eq!(stopped.status, TaskStatus::Cancelled);
    lease.join().await?;
    tokio::time::timeout(
        Duration::from_secs(5),
        runtime.continue_original_child_terminal(source.clone()),
    )
    .await??;
    assert!(source.is_durable_finished());
    assert!(!source.has_terminal_responsibility());
    assert!(source.monitor_stage().is_none());
    assert!(source.observed_terminal().is_none());
    assert!(source.frozen_terminal()?.is_none());
    assert!(
        child_receipt(&runtime, &source)?.is_none(),
        "the durable original Stop owns the result; no fabricated child terminal receipt"
    );
    assert_eq!(child.agent_state().await?.turn_index, 0);
    runtime.join_owned_mutations().await?;
    let bound:(String,String)=runtime.runtime_db().connection()?.query_row("SELECT json_extract(intent_json,'$.lease_id'),physical_state FROM task_stop_effects WHERE task_id=?1",[&task.id],|r|Ok((r.get(0)?,r.get(1)?)))?;
    assert_eq!(bound, (lease.lease_id().into(), "monitor_joined".into()));
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn child_physical_retirement_hands_exact_durable_monitor_to_real_reload_without_a_stop(
) -> Result<()> {
    use crate::runtime_db::task_snapshot::{TaskDomainLoadRequest, TaskSnapshotError};
    let _home = tempfile::tempdir()?;
    std::fs::write(
        _home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"},"runtime":{"state_memory":{"history_per_agent_bytes":67108864,"pending_per_agent_bytes":67108864,"record_limit":131072}}}"#,
    )?;
    let config = crate::config::AppConfig::load_with_home(Some(_home.path().to_path_buf()))?;
    let host = crate::host::RuntimeHost::new_with_provider(
        config,
        Arc::new(crate::provider::StubProvider::new("done")),
    )?;
    let runtime = host.default_runtime().await?;
    let parent_id = runtime.agent_state().await?.id;
    let target = host
        .create_named_agent("durable-original-monitor-target", None)
        .await?;
    let child = host.get_public_agent(&target.agent_id).await?;
    assert_eq!(child.agent_state().await?.turn_index, 0);
    let detail = serde_json::json!({"target_agent_id":target.agent_id,
        "target_identity_incarnation":target.incarnation,"created_new_subagent":false,
        "child_turn_baseline":0,"workspace_mode":"inherit"});
    let at = runtime.now();
    let task = TaskRecord {
        id: crate::ids::task_id(),
        agent_id: parent_id.clone(),
        kind: TaskKind::ActorInvocation,
        status: TaskStatus::Queued,
        created_at: at,
        updated_at: at,
        parent_message_id: None,
        work_item_id: None,
        summary: Some("durably recoverable original invocation".into()),
        detail: Some(detail.clone()),
        recovery: Some(TaskRecoverySpec::AgentInvocation {
            summary: "durably recoverable original invocation".into(),
            prompt: "accepted original target".into(),
            authority_class: AuthorityClass::OperatorInstruction,
            target_agent_id: Some(target.agent_id.clone()),
            created_new_subagent: false,
            workspace_mode: ChildAgentWorkspaceMode::Inherit,
            lifecycle_disposition: AgentLifecycleDisposition::Retain,
        }),
    };
    write_child_task_for_test(&runtime, &task).await?;
    // Use the actual admission/Task wholeACK/Async registration producer, not a
    // synthetic finished lease or a fabricated recovery publication capability.
    let task = runtime
        .start_agent_invocation_monitor(
            task,
            AuthorityClass::OperatorInstruction,
            false,
            target.agent_id.clone(),
            0,
            None,
            detail,
        )
        .await?;
    let lease = {
        let handles = runtime.inner.task_handles.lock().await;
        match handles
            .get(&task.id)
            .context("actual original monitor handle missing")?
        {
            command_task::ManagedTaskHandle::Async(handle) => handle.lease(),
            _ => anyhow::bail!("original monitor registered another handle kind"),
        }
    };
    let source = lease
        .original_child()?
        .context("actual original child source missing")?;
    tokio::time::timeout(Duration::from_secs(10),async {
        loop {
            let bound={let view=source.source_view()?;view.target().is_some()};
            let running:bool=runtime.runtime_db().connection()?.query_row(
                "SELECT EXISTS(SELECT 1 FROM queue_entries q JOIN messages m ON m.evidence_id=q.message_id WHERE m.kind='task_status' AND json_extract(m.payload_json,'$.metadata.task_id')=?1 AND q.status='processed')",[&task.id],|r|r.get(0))?;
            if bound && running {return Ok::<_,anyhow::Error>(())}
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await??;
    runtime
        .retire_instance_locally("durable original monitor handoff fixture")
        .await;
    runtime.inner.control_cancellations.cancel_local_producers();
    lease.join().await?;
    assert!(source.observed_terminal().is_none());
    assert!(source.monitor_stage().is_some());
    // A real small request must reject the native Task material before decode,
    // keep the source responsibility, and release all temporary view guards.
    let request = TaskDomainLoadRequest {
        query: Default::default(),
        capture: resident_task::TaskCaptureRequest::current(),
        record_limit: 1024,
        byte_limit: 64 * 1024 * 1024,
    };
    let owned_before = runtime
        .inner
        .input_evidence_budget
        .report()?
        .scopes
        .into_iter()
        .find(|scope| scope.scope == source.address().scope)
        .context("original scope budget absent")?
        .owned;
    let error = match runtime.runtime_db().transaction(|tx| {
        crate::runtime_db::child_task_terminal::observe_original_child_monitor_recovery_handoff_tx(
            tx,
            &source,
            &request,
            &runtime.inner.input_evidence_budget,
        )
    }) {
        Err(error) => error,
        Ok(_) => anyhow::bail!("small native material request must not discharge the monitor"),
    };
    assert!(
        error
            .downcast_ref::<TaskSnapshotError>()
            .is_some_and(|error| matches!(error, TaskSnapshotError::ResourceExhausted(_))),
        "{error:#}"
    );
    assert_eq!(
        runtime
            .inner
            .input_evidence_budget
            .report()?
            .scopes
            .into_iter()
            .find(|scope| scope.scope == source.address().scope)
            .context("original scope budget absent")?
            .owned,
        owned_before
    );
    assert!(source.has_terminal_responsibility());
    assert!(child_receipt(&runtime, &source)?.is_none());
    // The configured request exceeds the phase hard cap. Size one bad raw row
    // so the DB's original full hard cap fits its raw material, but Host slots
    // plus that same acquisition do not. Budget must win before JSON parsing.
    let high = TaskDomainLoadRequest {
        query: Default::default(),
        capture: resident_task::TaskCaptureRequest::current(),
        record_limit: 131072,
        byte_limit: 128 * 1024 * 1024,
    };
    assert!(runtime.inner.input_evidence_budget.limits().record_limit > 65_536);
    let address_bytes = usize::try_from(super::input_evidence_budget::serialized_bytes(
        source.address(),
    )?)?;
    let slots = (address_bytes + 512) * 8;
    let (original_raw,raw_len,owner_bytes,coverage_bytes):(String,usize,usize,usize)=runtime.runtime_db().connection()?.query_row(
        "SELECT t.payload_json,length(CAST(t.payload_json AS BLOB)),(SELECT length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(status AS BLOB)) FROM agent_identities WHERE agent_id=?2),(SELECT length(CAST(coverage_json AS BLOB)) FROM task_domain_current WHERE agent_id=?2 AND identity_incarnation=?3) FROM tasks t WHERE task_id=?1",
        rusqlite::params![task.id,parent_id,source.address().scope.identity_incarnation],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
    let maintenance = runtime.runtime_db().transaction(|tx| {
        crate::runtime_db::task_snapshot::task_maintenance_material_tx(
            tx,
            &source.address().scope,
            &std::collections::BTreeSet::from([source.address().admitted.key.clone()]),
        )
    })?;
    let prefix = (owner_bytes + parent_id.len() + 256) * 8 + slots + maintenance.bytes
        - raw_len * 8
        + coverage_bytes * 8;
    let bad_len = (64 * 1024 * 1024 - prefix) / 8;
    assert!(prefix + bad_len * 8 <= 64 * 1024 * 1024);
    assert!(
        slots + prefix + bad_len * 8 > 64 * 1024 * 1024,
        "separate fits must not share the full hard cap twice"
    );
    let bad_raw = format!("[{}", "x".repeat(bad_len - 1));
    runtime.runtime_db().transaction(|tx| {
        tx.execute(
            "UPDATE tasks SET payload_json=?2 WHERE task_id=?1",
            rusqlite::params![task.id, bad_raw],
        )?;
        Ok(())
    })?;
    let before = runtime
        .inner
        .input_evidence_budget
        .report()?
        .scopes
        .into_iter()
        .find(|scope| scope.scope == source.address().scope)
        .context("original scope budget absent")?
        .owned;
    let direct = match runtime.runtime_db().transaction(|tx| {
        crate::runtime_db::child_task_terminal::observe_original_child_monitor_recovery_handoff_tx(
            tx,
            &source,
            &high,
            &runtime.inner.input_evidence_budget,
        )
    }) {
        Err(error) => error,
        Ok(_) => anyhow::bail!("bad raw Task must be rejected"),
    };
    assert!(
        direct.downcast_ref::<TaskSnapshotError>().is_none(),
        "standalone DB raw gate fits before its malformed JSON: {direct:#}"
    );
    let combined = match runtime
        .inner
        .host_bridge
        .as_ref()
        .unwrap()
        .handoff_original_child_monitor_recovery(&source, &high)
        .await
    {
        Err(error) => error,
        Ok(_) => anyhow::bail!("Host plus DB must exceed the single phase hard cap"),
    };
    assert!(
        combined
            .downcast_ref::<TaskSnapshotError>()
            .is_some_and(|error| matches!(error, TaskSnapshotError::ResourceExhausted(_))),
        "{combined:#}"
    );
    assert_eq!(
        runtime
            .inner
            .input_evidence_budget
            .report()?
            .scopes
            .into_iter()
            .find(|scope| scope.scope == source.address().scope)
            .context("original scope budget absent")?
            .owned,
        before
    );
    assert!(source.has_terminal_responsibility());
    runtime.runtime_db().transaction(|tx| {
        tx.execute(
            "UPDATE tasks SET payload_json=?2 WHERE task_id=?1",
            rusqlite::params![task.id, original_raw],
        )?;
        Ok(())
    })?;
    tokio::time::timeout(Duration::from_secs(15), host.unload_runtime(&parent_id)).await?;
    assert!(
        source.is_durable_finished(),
        "normal retirement requires actual durable recovery handoff"
    );
    assert!(source.monitor_stage().is_none());
    assert!(source.observed_terminal().is_none());
    assert!(source.frozen_terminal()?.is_none());
    assert!(!runtime
        .inner
        .control_cancellations
        .contains_original_child_lease_for_test(&lease));
    assert!(
        child_receipt(&runtime, &source)?.is_none(),
        "handoff is not a fabricated terminal ACK"
    );
    assert_eq!(
        runtime
            .runtime_db()
            .tasks()
            .latest(&task.id)?
            .unwrap()
            .status,
        TaskStatus::Running
    );
    assert_eq!(child.agent_state().await?.turn_index, 0);
    let stop_count: u64 = runtime.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM task_stop_effects WHERE task_id=?1",
        [&task.id],
        |r| r.get(0),
    )?;
    assert_eq!(
        stop_count, 0,
        "no durable Stop is required or fabricated for physical retirement"
    );
    let current = host.default_runtime().await?;
    assert!(current.instance_owner_epoch() > runtime.instance_owner_epoch());
    let recovered_lease = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let lease = {
                let handles = current.inner.task_handles.lock().await;
                match handles.get(&task.id) {
                    Some(command_task::ManagedTaskHandle::Async(handle)) => Some(handle.lease()),
                    _ => None,
                }
            };
            if let Some(lease) = lease {
                return Ok::<_, anyhow::Error>(lease);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    assert_ne!(recovered_lease.lease_id(), lease.lease_id());
    let recovered = recovered_lease
        .original_child()?
        .context("real bootstrap did not register recovered child producer")?;
    assert_eq!(recovered.address().scope, source.address().scope);
    assert_eq!(
        recovered.address().admitted.key,
        source.address().admitted.key
    );
    child
        .enqueue(MessageEnvelope::new(
            target.agent_id.clone(),
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "finish the actual recovered invocation".into(),
            },
        ))
        .await?;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if current
                .runtime_db()
                .tasks()
                .latest(&task.id)?
                .is_some_and(|task| task.status == TaskStatus::Completed)
            {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    current.join_owned_mutations().await?;
    assert!(recovered.is_durable_finished());
    let results:u64=current.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM messages WHERE kind='task_result' AND json_extract(payload_json,'$.task_id')=?1",[&task.id],|r|r.get(0))?;
    assert_eq!(
        results, 1,
        "only the genuinely observed recovered terminal produces a Result"
    );
    assert!(child_receipt(&current, &source)?.is_none());
    assert!(child_receipt(&current, &recovered)?.is_some());
    host.shutdown().await?;
    Ok(())
}
