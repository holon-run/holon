use super::*;
use crate::{
    provider::StubProvider,
    runtime::mutation_owner::MutationOwnerError,
    types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentRegistryStatus,
        AgentVisibility, AuthorityClass, MessageBody, MessageKind, MessageOrigin, Priority,
        TaskKind, TaskStatus,
    },
};
use tempfile::{tempdir, TempDir};

struct Fixture {
    runtime: RuntimeHandle,
    _data: TempDir,
    _workspace: TempDir,
}
impl Fixture {
    fn new() -> Result<Self> {
        let data = tempdir()?;
        let workspace = tempdir()?;
        let runtime = RuntimeHandle::new(
            "default",
            data.path().to_owned(),
            workspace.path().to_owned(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("unused")),
            "default".into(),
            crate::context::ContextConfig::default(),
        )?;
        Ok(Self {
            runtime,
            _data: data,
            _workspace: workspace,
        })
    }
    async fn claim(&self) -> Result<crate::runtime::scheduler_executor::ScheduledMessage> {
        self.runtime
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
                    text: "original Task retry run".into(),
                },
            ))
            .await?;
        let crate::runtime::scheduler_executor::RunLoopPoll::Message(scheduled) =
            crate::runtime::scheduler_executor::SchedulerDecisionExecutor::new(&self.runtime)
                .poll()
                .await?
        else {
            anyhow::bail!("expected an actual claimed run")
        };
        Ok(scheduled)
    }

    fn child(&self) -> Result<AgentIdentityRecord> {
        let mut identity = AgentIdentityRecord::new(
            "child:runtime-task",
            AgentKind::Child,
            AgentVisibility::Private,
            AgentOwnership::ParentSupervised,
            AgentProfilePreset::PrivateChild,
            Some("default".into()),
            Some("task:runtime-original".into()),
        )
        .with_lineage_parent_agent_id(Some("default".into()));
        identity.created_at = "2026-10-01T00:00:00Z".parse()?;
        identity.updated_at = identity.created_at;
        self.runtime
            .runtime_db()
            .agent_identities()
            .create_with_relations(
                &identity,
                &crate::runtime_db::agent_relations::supervised_creation_records(
                    &identity,
                    "default",
                    "task:runtime-original",
                    None,
                ),
            )?;
        Ok(identity)
    }
    fn command(
        &self,
        deletion: bool,
        fault: Option<TransitionFaultPoint>,
    ) -> TaskTransitionCommand {
        let at = Utc::now();
        TaskTransitionCommand {
            agent_id: "default".into(),
            expected_owner_incarnation: self.runtime.instance_incarnation(),
            task: TaskRecord {
                id: "task:runtime-original".into(),
                agent_id: "default".into(),
                kind: TaskKind::ChildAgentTask,
                status: TaskStatus::Completed,
                created_at: at,
                updated_at: at,
                parent_message_id: None,
                work_item_id: None,
                summary: Some("original runtime child completion".into()),
                detail: Some(serde_json::json!({"child_agent_id":"child:runtime-task"})),
                recovery: None,
            },
            agent_deletion: deletion.then(|| AgentDeletionRequest {
                agent_id: "child:runtime-task".into(),
                admission: AgentDeletionAdmission::TerminalEphemeralChild {
                    parent_agent_id: "default".into(),
                    task_id: "task:runtime-original".into(),
                },
                requested_by: "runtime-task-fixture".into(),
                cascade_private_children: false,
            }),
            task_result_settlement: None,
            queue_entry: None,
            work_items: Vec::new(),
            expected_wait_conditions: Vec::new(),
            wait_conditions: Vec::new(),
            agent_state: None,
            message_evidence: Vec::new(),
            audit_events: vec![AuditEvent::legacy(
                "task_runtime_original",
                serde_json::json!({"task_id":"task:runtime-original"}),
            )],
            index_changes: Vec::new(),
            notify_scheduler: true,
            commit_on_idempotent: false,
            fault,
        }
    }
}

#[tokio::test]
async fn task_stale_baseline_reprepares_and_retries_only_the_frozen_command() -> Result<()> {
    use crate::runtime::resident_task::{TaskFamily, TaskKey};
    use crate::runtime_db::{
        repositories::upsert_task_tx,
        task_snapshot::{self as tasks, TaskWriteBaselineStale},
    };
    use std::collections::BTreeSet;
    let f = Fixture::new()?;
    let scheduled = f.claim().await?;
    let mut mutation = f.runtime.begin_mutation().await?;
    let original_run = mutation
        .current_run_abort
        .as_ref()
        .context("original run handle missing")?
        .clone();
    let lease = f
        .runtime
        .inner
        .control_cancellations
        .run_for(&original_run.run_id)
        .context("original completion lease missing")?;
    let (load, _material) = mutation.owner_task_load_request()?;
    mutation.prepare_owner_task_domain_request(&load).await?;
    mutation.defer_abort_current_run("abort after frozen Task retry");
    let scope = mutation.input_evidence_scope();
    let command = f.command(false, None);
    let original = command.task.clone();
    let prepared = PreparedInputTransition::task(
        scope.clone(),
        "runtime-task-stale-baseline".into(),
        original.updated_at,
        PreparedTaskTransition::new(command, Default::default(), true, None, None)?,
    )?
    .with_task_bases(
        vec![mutation.tasks.as_ref().context("baseline missing")?.clone()],
        &f.runtime.inner.input_evidence_budget,
    )?;
    let descriptor = prepared.descriptor().clone();
    let mut rival = f.command(false, None).task;
    rival.id = "task:rival-baseline".into();
    rival.status = TaskStatus::Running;
    let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &rival.id)]);
    let db = f.runtime.runtime_db();
    db.transaction(|tx| {
        let proof = tasks::prepare_task_write_tx(tx, &scope, &keys)?;
        upsert_task_tx(tx, &rival)?;
        tasks::register_task_writes_with_budget_tx(
            tx,
            &proof,
            &keys,
            mutation.committed_state(),
            &load,
            &f.runtime.inner.input_evidence_budget,
        )?;
        Ok(())
    })?;
    let work_request = mutation.owner_work_load_request()?;
    let probe = crate::runtime_db::write_log_probe::WriteLogProbe::default();
    let error = probe.capture(|| {
        db.transitions_in_scope(&scope)
            .commit_prepared_transition_with_input_query_and_task_bases(
                &prepared,
                &work_request,
                &f.runtime.inner.input_evidence_budget,
                &load,
                None,
                None,
                &Default::default(),
            )
            .unwrap_err()
    });
    assert!(
        error.downcast_ref::<TaskWriteBaselineStale>().is_some(),
        "{error:#}"
    );
    assert_eq!(
        probe.write_failure_warns(),
        0,
        "the intermediate reprepare attempt must not surface as a failure WARN"
    );
    assert!(
        probe.deferred_reprepare_events() >= 1,
        "the intermediate attempt must leave an identifiable reader_reprepare event"
    );
    assert!(db.tasks().latest(&original.id)?.is_none());
    assert!(!original_run.token.is_cancelled());
    let receipts = || -> Result<i64> {
        Ok(db.connection()?.query_row(
            "SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce=?1",
            [descriptor.command_nonce()],
            |row| row.get(0),
        )?)
    };
    assert_eq!(receipts()?, 0);
    let stale_baseline = mutation.tasks.as_ref().context("baseline missing")?.clone();
    mutation.prepare_owner_task_domain_request(&load).await?;
    assert!(Arc::ptr_eq(
        mutation.tasks.as_ref().context("baseline missing")?,
        &stale_baseline
    ));
    assert!(!stale_baseline
        .coverage()
        .active_task_ids
        .contains(&rival.id));
    drop(error);
    let (commit, _) = mutation.commit_prepared_input_with_result(prepared).await?;
    assert!(commit.applied);
    assert_eq!(db.tasks().latest(&original.id)?, Some(original));
    assert_eq!(receipts()?, 1);
    assert!(!Arc::ptr_eq(
        mutation
            .tasks
            .as_ref()
            .context("Task publication missing")?,
        &stale_baseline
    ));
    assert!(mutation
        .reservations
        .iter()
        .all(|reservation| reservation.recovery().unwrap().is_none()));
    let coverage = mutation
        .tasks
        .as_ref()
        .context("Task publication missing")?
        .coverage();
    assert!(coverage.active_task_ids.contains(&rival.id));
    let current_run = mutation
        .current_run_abort
        .as_ref()
        .context("Task retry lost the original run handle")?;
    assert_eq!(current_run.run_id, original_run.run_id);
    assert!(Arc::ptr_eq(&current_run.reason, &original_run.reason));
    assert!(original_run.token.is_cancelled());
    assert_eq!(
        *original_run.reason.lock().unwrap(),
        "abort after frozen Task retry"
    );
    assert!(!lease.is_complete());
    drop(mutation);
    drop(scheduled);
    assert!(lease.is_complete());
    Ok(())
}

#[tokio::test]
async fn task_baseline_retry_does_not_apply_deferred_abort_after_business_rollback() -> Result<()> {
    use crate::runtime::resident_task::{TaskFamily, TaskKey};
    use crate::runtime_db::{repositories::upsert_task_tx, task_snapshot as tasks};
    use std::collections::BTreeSet;

    let f = Fixture::new()?;
    let scheduled = f.claim().await?;
    let mut mutation = f.runtime.begin_mutation().await?;
    let (load, _material) = mutation.owner_task_load_request()?;
    mutation.prepare_owner_task_domain_request(&load).await?;
    let committed_state = mutation.committed_state().clone();
    let original_run = mutation
        .current_run_abort
        .as_ref()
        .context("original run handle missing")?
        .clone();
    let private_run = CurrentRunAbortHandle {
        run_id: "uncommitted-private-run".into(),
        token: tokio_util::sync::CancellationToken::new(),
        reason: Arc::new(std::sync::Mutex::new(String::new())),
    };
    mutation.state.current_run_id = Some(private_run.run_id.clone());
    mutation.current_run_abort = Some(private_run.clone());
    mutation.defer_abort_current_run("must not cancel after rollback");
    let command = f.command(false, Some(TransitionFaultPoint::BeforeCommit));
    let task_id = command.task.id.clone();
    let prepared = PreparedInputTransition::task(
        mutation.input_evidence_scope(),
        "runtime-task-retry-rollback".into(),
        command.task.updated_at,
        PreparedTaskTransition::new(command, Default::default(), true, None, None)?,
    )?
    .with_task_bases(
        vec![mutation.tasks.as_ref().context("baseline missing")?.clone()],
        &f.runtime.inner.input_evidence_budget,
    )?;
    let stale_baseline = mutation.tasks.as_ref().unwrap().clone();
    let scope = mutation.input_evidence_scope();
    let mut rival = f.command(false, None).task;
    rival.id = "task:rival-rollback-baseline".into();
    rival.status = TaskStatus::Running;
    let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &rival.id)]);
    f.runtime.runtime_db().transaction(|tx| {
        let proof = tasks::prepare_task_write_tx(tx, &scope, &keys)?;
        upsert_task_tx(tx, &rival)?;
        tasks::register_task_writes_with_budget_tx(
            tx,
            &proof,
            &keys,
            &committed_state,
            &load,
            &f.runtime.inner.input_evidence_budget,
        )?;
        Ok(())
    })?;
    let error = mutation
        .commit_prepared_input_with_result(prepared)
        .await
        .unwrap_err();
    assert_eq!(
        persistence_certainty(&error),
        Some(PersistenceCertainty::DefinitelyNotCommitted)
    );
    assert!(f.runtime.runtime_db().tasks().latest(&task_id)?.is_none());
    assert!(!original_run.token.is_cancelled());
    assert!(!private_run.token.is_cancelled());
    assert!(mutation.deferred_abort.is_none());
    assert_eq!(mutation.state, committed_state);
    let published = f.runtime.inner.mutation_owner.capture()?.unwrap();
    assert_eq!(published.state.as_ref(), &committed_state);
    assert!(!Arc::ptr_eq(
        published
            .tasks
            .as_ref()
            .context("new Task baseline missing")?,
        &stale_baseline
    ));
    let current_run = mutation
        .current_run_abort
        .as_ref()
        .context("rollback lost the original committed run handle")?;
    assert_eq!(current_run.run_id, original_run.run_id);
    assert!(Arc::ptr_eq(&current_run.reason, &original_run.reason));
    drop(mutation);
    drop(scheduled);
    Ok(())
}

#[tokio::test]
async fn task_adapter_freezes_the_held_child_and_returns_its_exact_deletion_ack() -> Result<()> {
    let f = Fixture::new()?;
    let child = f.child()?;
    let command = f.command(true, None);
    let task = command.task.clone();
    let mut mutation = f
        .runtime
        .begin_mutation_with_agent_deletion(command.agent_deletion.as_ref())
        .await?;
    assert_eq!(mutation.reservations.len(), 2);
    assert_eq!(
        mutation.frozen_task_child_identity(command.agent_deletion.as_ref().unwrap())?,
        child
    );
    let (commit, deletion) = mutation
        .commit_task_transition(
            "runtime-task-original".into(),
            command,
            Default::default(),
            true,
            None,
        )
        .await?;
    let deletion = deletion.expect("original Task must return its child deletion ACK");
    assert!(commit.applied);
    assert!(deletion.created);
    assert_eq!(deletion.identity.agent_id, child.agent_id);
    assert_eq!(deletion.identity.incarnation, child.incarnation);
    assert_eq!(deletion.identity.status, AgentRegistryStatus::Deleting);
    assert_eq!(
        f.runtime
            .runtime_db()
            .agent_identities()
            .latest(&child.agent_id)?,
        Some(deletion.identity)
    );
    assert_eq!(
        f.runtime
            .runtime_db()
            .agent_deletions()
            .latest_for_agent(&child.agent_id)?,
        Some(deletion.job)
    );
    assert!(mutation
        .reservations
        .iter()
        .all(|reservation| reservation.recovery().unwrap().is_none()));
    drop(mutation);
    f.runtime.apply_transition_commit(commit).await;
    assert_eq!(f.runtime.task_record(&task.id).await?, Some(task));
    let scopes: String = f.runtime.runtime_db().connection()?.query_row(
        "SELECT scopes_json FROM input_mutation_receipts WHERE command_nonce=?1",
        ["runtime-task-original"],
        |row| row.get(0),
    )?;
    let scopes: Vec<super::super::super::resident_input_evidence::InputEvidenceScope> =
        serde_json::from_str(&scopes)?;
    assert_eq!(scopes.len(), 2);
    assert!(scopes
        .iter()
        .any(|scope| scope.agent_id == child.agent_id
            && scope.identity_incarnation == child.incarnation));
    Ok(())
}

#[tokio::test]
async fn task_adapter_known_failure_keeps_the_whole_group_and_recovers_without_business_replay(
) -> Result<()> {
    let f = Fixture::new()?;
    let child = f.child()?;
    let child_owner = f
        .runtime
        .inner
        .standalone_related_mutation_owners
        .current_owner(&child.agent_id, child.incarnation)?;
    let command = f.command(true, Some(TransitionFaultPoint::BeforeCacheUpdate));
    let task = command.task.clone();
    let mut mutation = f
        .runtime
        .begin_mutation_with_agent_deletion(command.agent_deletion.as_ref())
        .await?;
    let error = mutation
        .commit_task_transition(
            "runtime-task-known".into(),
            command,
            Default::default(),
            true,
            None,
        )
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("committed but publication"),
        "{error:#}"
    );
    assert!(matches!(
        f.runtime.inner.mutation_owner.capture(),
        Err(MutationOwnerError::NeedsReconcile(_))
    ));
    assert!(matches!(
        child_owner.capture(),
        Err(MutationOwnerError::NeedsReconcile(_))
    ));
    let child_recovery = mutation
        .reservations
        .iter()
        .find(|scope| scope.agent_id() == child.agent_id)
        .unwrap()
        .recovery()?
        .unwrap();
    assert!(
        matches!(child_recovery.known, Some(KnownCommit::Metadata(ref facts))
        if facts.identity.as_ref().is_some_and(|identity| identity.status == AgentRegistryStatus::Deleting))
    );
    let identity = f
        .runtime
        .runtime_db()
        .agent_identities()
        .latest(&child.agent_id)?;
    let job = f
        .runtime
        .runtime_db()
        .agent_deletions()
        .latest_for_agent(&child.agent_id)?;
    let events = f.runtime.storage().read_recent_events(100)?;
    drop(mutation);
    let recovered = f.runtime.begin_mutation().await?;
    drop(recovered);
    assert!(child_owner.capture().is_ok());
    assert_eq!(f.runtime.task_record(&task.id).await?, Some(task));
    assert_eq!(
        f.runtime
            .runtime_db()
            .agent_identities()
            .latest(&child.agent_id)?,
        identity
    );
    assert_eq!(
        f.runtime
            .runtime_db()
            .agent_deletions()
            .latest_for_agent(&child.agent_id)?,
        job
    );
    assert_eq!(f.runtime.storage().read_recent_events(100)?, events);
    Ok(())
}

#[tokio::test]
async fn generic_transition_adapter_rejects_task_before_sql_or_owner_begin() -> Result<()> {
    let f = Fixture::new()?;
    let command = f.command(false, None);
    let task_id = command.task.id.clone();
    let mut mutation = f.runtime.begin_mutation().await?;
    let prepared = PreparedInputTransition::task(
        mutation.input_evidence_scope(),
        "runtime-task-wrong-adapter".into(),
        Utc::now(),
        PreparedTaskTransition::new(command, Default::default(), true, None, None)?,
    )?;
    let error = mutation
        .commit_prepared_input_transition(prepared)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("typed runtime adapter"));
    assert!(f.runtime.inner.mutation_owner.capture().is_ok());
    assert!(mutation.reservations[mutation.primary_index]
        .recovery()?
        .is_none());
    assert!(f.runtime.runtime_db().tasks().latest(&task_id)?.is_none());
    let receipts: i64 = f.runtime.runtime_db().connection()?.query_row(
        "SELECT count(*) FROM input_mutation_receipts WHERE command_nonce=?1",
        ["runtime-task-wrong-adapter"],
        |row| row.get(0),
    )?;
    assert_eq!(receipts, 0);
    Ok(())
}

#[tokio::test]
async fn task_status_reducer_reports_canonical_mismatch_without_committing() -> Result<()> {
    use crate::runtime_db::task_snapshot::TaskSnapshotError;
    use crate::runtime_error::{describe_runtime_error, RuntimeErrorDomain};

    let f = Fixture::new()?;
    let mut task = f.command(false, None).task;
    task.status = TaskStatus::Running;
    f.runtime.reduce_task_status_message(task.clone()).await?;
    let db = f.runtime.runtime_db();
    // The projection changes independently of the canonical body and writer proof.
    db.transaction(|tx| {
        assert_eq!(
            tx.execute(
                "UPDATE tasks SET correlation_id='external-drift' WHERE task_id=?1",
                [&task.id],
            )?,
            1
        );
        Ok(())
    })?;
    let before = db.tasks().latest(&task.id)?.expect("original Task");
    let events = f.runtime.storage().read_recent_events(100)?;

    task.status = TaskStatus::Cancelling;
    task.updated_at = Utc::now();
    let error = f
        .runtime
        .reduce_task_status_message(task)
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<TaskSnapshotError>(),
        Some(TaskSnapshotError::CanonicalChanged(_))
    ));
    let descriptor = describe_runtime_error(&error);
    assert_eq!(descriptor.domain, RuntimeErrorDomain::Task);
    assert_eq!(descriptor.code, "task_status_canonical_mismatch");
    assert!(!descriptor.retryable);
    assert!(descriptor.recovery_hint.is_some());
    assert_eq!(db.tasks().latest(&before.id)?, Some(before));
    assert_eq!(f.runtime.storage().read_recent_events(100)?, events);
    Ok(())
}

#[tokio::test]
async fn task_reducer_writes_typed_receipts_and_keeps_missing_child_fallback() -> Result<()> {
    let f = Fixture::new()?;
    let mut task = f.command(false, None).task;
    task.status = TaskStatus::Running;
    f.runtime.reduce_task_status_message(task.clone()).await?;
    let mut missing = MessageEnvelope::new(
        "default",
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: task.id.clone(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "child did not materialize".into(),
        },
    );
    missing.task_id = Some(task.id.clone());
    task.status = TaskStatus::Failed;
    task.updated_at = Utc::now();
    let deletion = AgentDeletionRequest {
        agent_id: "child:missing-runtime-task".into(),
        admission: AgentDeletionAdmission::TerminalEphemeralChild {
            parent_agent_id: "default".into(),
            task_id: task.id.clone(),
        },
        requested_by: "runtime-task-fixture".into(),
        cascade_private_children: false,
    };
    assert!(
        !f.runtime
            .commit_terminal_child_task_result(
                &task,
                "task_status_updated",
                &missing,
                Some(deletion)
            )
            .await?
    );
    assert_eq!(f.runtime.task_record(&task.id).await?, Some(task));
    let receipts: i64 = f.runtime.runtime_db().connection()?.query_row(
        "SELECT count(*) FROM input_mutation_receipts WHERE family='task'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(receipts, 2);
    assert!(f
        .runtime
        .runtime_db()
        .agent_deletions()
        .latest_for_agent("child:missing-runtime-task")?
        .is_none());
    Ok(())
}
