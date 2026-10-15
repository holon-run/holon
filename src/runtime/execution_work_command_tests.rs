use super::*;
use crate::runtime::tests::support::{context_config, tempdir, StubProvider};
use crate::runtime_db::transitions::{
    ExecutionProtocolTransition, PreparedInputTransition, WorkItemMutation,
    WorkItemTransitionCommand,
};

fn fixture(home: &std::path::Path) -> Result<RuntimeHandle> {
    RuntimeHandle::new(
        "execution-work",
        home.into(),
        None::<crate::types::WorkspaceEntry>,
        String::new(),
        Arc::new(StubProvider::new("unused")),
        "execution-work".into(),
        context_config(),
    )
}
fn deny_protocol(runtime: &RuntimeHandle, deny: bool) -> Result<()> {
    runtime.runtime_db().transaction(|tx| {
        for table in [
            "execution_protocol_partitions",
            "execution_protocol_work_items",
            "execution_protocol_attempts",
            "execution_protocol_outcomes",
            "execution_protocol_command_results",
        ] {
            tx.execute_batch(&if deny {
                format!("ALTER TABLE {table} RENAME TO c1_forbidden_{table}")
            } else {
                format!("ALTER TABLE c1_forbidden_{table} RENAME TO {table}")
            })?;
        }
        Ok(())
    })
}
fn update(before: &WorkItemRecord) -> (WorkItemRecord, WorkItemTransitionCommand) {
    let mut after = before.clone();
    after.revision += 1;
    after.updated_at = Utc::now();
    after.blocked_by = Some("original paused readiness".into());
    let command = WorkItemTransitionCommand {
        agent_id: before.agent_id.clone(),
        mutation: WorkItemMutation::Update {
            record: after.clone(),
            expected_revision: before.revision,
        },
        agent_state: None,
        brief_evidence: Vec::new(),
        audit_events: Vec::new(),
        index_changes: Vec::new(),
        notify_scheduler: false,
        fault: None,
    };
    (after, command)
}

#[tokio::test]
async fn actual_work_ack_plans_with_protocol_tables_denied_and_old_command_keeps_occ() -> Result<()>
{
    let home = tempdir()?;
    let runtime = fixture(home.path())?;
    let work = runtime
        .create_work_item("real Work".into(), None, None, Vec::new())
        .await?;
    let mut owner = runtime.begin_mutation().await?;
    let token = owner.capture_published_root()?.token;
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    deny_protocol(&runtime, true)?;
    let read = owner
        .execution_command_source()?
        .capture(Some(&work.id), &[&work.id, "original paused readiness"])?;
    let (after, command) = update(&work);
    let protocol = runtime.plan_work_item_execution_update(
        &after,
        Some(WorkItemReadinessMutation::Pause(
            after.blocked_by.clone().unwrap(),
        )),
        &read,
    )?;
    assert_eq!(
        read.work_execution(&work.id)?.unwrap().source_revision,
        work.revision
    );
    assert_eq!(owner.capture_published_root()?.token, token);
    assert!(runtime.inner.input_evidence_budget.report()?.observed_bytes > baseline);
    deny_protocol(&runtime, false)?;
    let commit = owner
        .commit_work_transition(
            "c1-actual-work".into(),
            command.clone(),
            protocol.clone(),
            true,
        )
        .await?;
    drop(owner);
    runtime.apply_transition_commit(commit).await;
    assert_eq!(
        runtime
            .runtime_db()
            .work_items()
            .latest(&work.id)?
            .unwrap()
            .revision,
        after.revision
    );
    let newer = runtime
        .update_work_item_fields(
            work.id.clone(),
            Some("later accepted Work revision".into()),
            None,
            None,
            None,
            None,
        )
        .await?;
    assert!(newer.revision > after.revision);
    let before_stale:(usize,String)=runtime.runtime_db().connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM input_mutation_receipts),payload_json FROM execution_protocol_work_items WHERE agent_id=?1 AND work_item_id=?2",
        rusqlite::params![work.agent_id,work.id],|row|Ok((row.get(0)?,row.get(1)?)))?;
    let mut owner = runtime.begin_mutation().await?;
    let stale = owner
        .commit_work_transition("c1-stale-work".into(), command, protocol, true)
        .await;
    assert!(
        stale.is_err(),
        "old selected read cannot replace original Work OCC"
    );
    drop(owner);
    let after_stale:(usize,String)=runtime.runtime_db().connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM input_mutation_receipts),payload_json FROM execution_protocol_work_items WHERE agent_id=?1 AND work_item_id=?2",
        rusqlite::params![work.agent_id,work.id],|row|Ok((row.get(0)?,row.get(1)?)))?;
    assert_eq!(after_stale, before_stale);
    let with_read = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    drop(read);
    assert!(runtime.inner.input_evidence_budget.report()?.observed_bytes < with_read);
    Ok(())
}

#[tokio::test]
async fn frozen_work_command_replays_original_ack_after_publication_fault() -> Result<()> {
    let home = tempdir()?;
    let runtime = fixture(home.path())?;
    let work = runtime
        .create_work_item("original replay".into(), None, None, Vec::new())
        .await?;
    let mut owner = runtime.begin_mutation().await?;
    let read = owner
        .execution_command_source()?
        .capture(Some(&work.id), &[&work.id, "original paused readiness"])?;
    let (after, mut command) = update(&work);
    command.fault = Some(crate::runtime_db::transitions::TransitionFaultPoint::BeforeCacheUpdate);
    let protocol: ExecutionProtocolTransition = runtime.plan_work_item_execution_update(
        &after,
        Some(WorkItemReadinessMutation::Pause(
            after.blocked_by.clone().unwrap(),
        )),
        &read,
    )?;
    let scope = owner.input_evidence_scope();
    let at = runtime.now();
    let original = PreparedInputTransition::work(
        scope.clone(),
        "c1-replay-original".into(),
        at,
        command.clone(),
        protocol.clone(),
        true,
    )?;
    let replay = PreparedInputTransition::work(
        scope,
        "c1-replay-original".into(),
        at,
        command,
        protocol,
        true,
    )?;
    let descriptor = original.descriptor().clone();
    assert!(owner
        .commit_prepared_input_transition(original)
        .await
        .is_err());
    drop(owner);
    assert!(runtime.inner.mutation_owner.capture().is_err());
    let before:(usize,usize,String)=runtime.runtime_db().connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM input_mutation_receipts),(SELECT COUNT(*) FROM execution_protocol_command_results),payload_json FROM execution_protocol_work_items WHERE agent_id=?1 AND work_item_id=?2",
        rusqlite::params![work.agent_id,work.id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
    let mut owner = runtime.begin_mutation().await?;
    let commit = owner.commit_prepared_input_transition(replay).await?;
    drop(owner);
    runtime.apply_transition_commit(commit).await;
    let after_replay:(usize,usize,String)=runtime.runtime_db().connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM input_mutation_receipts),(SELECT COUNT(*) FROM execution_protocol_command_results),payload_json FROM execution_protocol_work_items WHERE agent_id=?1 AND work_item_id=?2",
        rusqlite::params![work.agent_id,work.id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
    assert_eq!(after_replay, before);
    assert_eq!(
        runtime
            .runtime_db()
            .work_items()
            .latest(&work.id)?
            .unwrap()
            .revision,
        after.revision
    );
    assert_eq!(descriptor.command_nonce(), "c1-replay-original");
    drop(read);
    Ok(())
}

#[tokio::test]
async fn actual_inflight_work_read_does_not_grant_readiness_change() -> Result<()> {
    Box::pin(async {
        let home=tempdir()?;let runtime=fixture(home.path())?;
        let work=runtime.create_work_item("real active Work".into(),Some(WorkItemPlanStatus::Ready),None,Vec::new()).await?;
        let mut source=MessageEnvelope::new("execution-work",MessageKind::OperatorPrompt,
            MessageOrigin::Operator {actor_id:None,actor_display_name:None},AuthorityClass::OperatorInstruction,
            Priority::Normal,MessageBody::Text {text:"actual active Work".into()});
        source.work_item_id=Some(work.id.clone());runtime.enqueue(source).await?;
        let super::super::scheduler_executor::RunLoopPoll::Message(scheduled)=
            super::super::scheduler_executor::SchedulerDecisionExecutor::new(&runtime).poll().await?
            else {anyhow::bail!("actual Work claim missing")};
        runtime.begin_dispatch_turn(&scheduled.message,None,None,
            scheduled.dispatch_plan.execution_admission_provenance.clone(),&scheduled.dispatch_plan.prior_closure).await?;
        let owner=runtime.begin_mutation().await?;
        let read=owner.execution_command_source()?.capture(Some(&work.id),&[&work.id,"no readiness authority"])?;
        let before:(usize,String)=runtime.runtime_db().connection()?.query_row(
            "SELECT (SELECT COUNT(*) FROM input_mutation_receipts),payload_json FROM execution_protocol_work_items WHERE agent_id=?1 AND work_item_id=?2",
            rusqlite::params![work.agent_id,work.id],|row|Ok((row.get(0)?,row.get(1)?)))?;
        deny_protocol(&runtime,true)?;
        let error=runtime.plan_work_item_execution_update(&work,Some(WorkItemReadinessMutation::Pause("no readiness authority".into())),&read)
            .unwrap_err();
        deny_protocol(&runtime,false)?;
        assert!(error.to_string().contains("active or requires repair"));
        drop(owner);
        let after:(usize,String)=runtime.runtime_db().connection()?.query_row(
            "SELECT (SELECT COUNT(*) FROM input_mutation_receipts),payload_json FROM execution_protocol_work_items WHERE agent_id=?1 AND work_item_id=?2",
            rusqlite::params![work.agent_id,work.id],|row|Ok((row.get(0)?,row.get(1)?)))?;
        assert_eq!(after,before);
        let terminal=runtime.build_reducer_only_terminal_transition("execution_active_work_fixture",false).await?;
        let run=scheduled.running_state.current_run_id.as_deref().context("original Run missing")?;
        runtime.finish_scheduled_message_processing(&scheduled.message,run,std::time::Instant::now(),Ok(terminal)).await?;
        drop((scheduled,read));
        Ok(())
    }).await
}

// Before the real fixed cutover, this Work was live but had no execution row.
fn original_v81_unregistered_work(home: &std::path::Path) -> Result<WorkItemRecord> {
    use crate::runtime_db::{
        evidence::{upsert_agent_identity_tx, upsert_agent_state_tx},
        migrations::{apply_migration, ensure_migration_table, MIGRATIONS},
        repositories::{enum_string, timestamp},
    };
    let work = WorkItemRecord::new(
        "execution-work",
        "original unregistered Work",
        WorkItemState::Open,
    );
    let path = home.join(".holon/state/runtime.sqlite");
    std::fs::create_dir_all(path.parent().unwrap())?;
    let mut connection = rusqlite::Connection::open(path)?;
    ensure_migration_table(&connection)?;
    for migration in MIGRATIONS
        .iter()
        .filter(|migration| migration.version <= 81)
    {
        apply_migration(&mut connection, migration)?;
    }
    let tx = connection.transaction()?;
    let identity = crate::types::AgentIdentityRecord::new(
        "execution-work",
        crate::types::AgentKind::Named,
        crate::types::AgentVisibility::Public,
        crate::types::AgentOwnership::SelfOwned,
        crate::types::AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    upsert_agent_identity_tx(&tx, &identity)?;
    upsert_agent_state_tx(&tx, &AgentState::new("execution-work"))?;
    tx.execute("INSERT INTO work_items (work_item_id,agent_id,state,objective,plan_status,revision,current_focus,created_at,updated_at,completed_at,plan_artifact_path,last_turn_id,payload_json) VALUES (?1,?2,'open',?3,?4,?5,0,?6,?7,NULL,NULL,NULL,?8)",rusqlite::params![work.id,work.agent_id,work.objective,enum_string(&work.plan_status)?,i64::try_from(work.revision)?,timestamp(work.created_at),timestamp(work.updated_at),serde_json::to_string(&work)?])?;
    tx.commit()?;
    Ok(work)
}

#[tokio::test]
async fn fixed_unregistered_work_update_prepares_exact_negative_before_registering() -> Result<()> {
    let home = tempdir()?;
    let legacy = original_v81_unregistered_work(home.path())?;
    let runtime = fixture(home.path())?;
    runtime
        .create_work_item(
            "other Work initializes protocol".into(),
            None,
            None,
            Vec::new(),
        )
        .await?;
    let before = runtime.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM execution_protocol_work_items WHERE agent_id=?1 AND work_item_id=?2",
        rusqlite::params![legacy.agent_id, legacy.id],
        |row| row.get::<_, usize>(0),
    )?;
    assert_eq!(before, 0);
    {
        let owner = runtime.begin_mutation().await?;
        let source = owner.execution_command_source()?;
        assert!(source.requires_work_preparation(&legacy.id));
        assert!(source.capture(Some(&legacy.id), &[&legacy.id]).is_err());
    }
    let updated = runtime
        .update_work_item_fields(
            legacy.id.clone(),
            Some("accepted legacy update".into()),
            None,
            None,
            None,
            None,
        )
        .await?;
    assert_eq!(updated.revision, legacy.revision + 1);
    let owner = runtime.begin_mutation().await?;
    deny_protocol(&runtime, true)?;
    let read = owner
        .execution_command_source()?
        .capture(Some(&legacy.id), &[&legacy.id])?;
    assert_eq!(
        read.work_execution(&legacy.id)?.unwrap().source_revision,
        updated.revision
    );
    let mut next = updated.clone();
    next.revision += 1;
    let plan = runtime.plan_work_item_execution_update(&next, None, &read)?;
    assert!(plan.bootstrap.is_none());
    deny_protocol(&runtime, false)?;
    assert_eq!(
        runtime.runtime_db().connection()?.query_row(
            "SELECT COUNT(*) FROM execution_protocol_attempts WHERE agent_id=?1",
            [&legacy.agent_id],
            |row| row.get::<_, usize>(0)
        )?,
        0
    );
    Ok(())
}

#[tokio::test]
async fn by_id_execution_preparation_cannot_adopt_later_raw_absence() -> Result<()> {
    let home = tempdir()?;
    let runtime = fixture(home.path())?;
    runtime
        .create_work_item("initialize protocol".into(), None, None, Vec::new())
        .await?;
    let mut owner = runtime.begin_mutation().await?;
    let token = owner.capture_published_root()?.token;
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let before: usize = runtime.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM input_evidence_current",
        [],
        |row| row.get(0),
    )?;
    let error = owner
        .prepare_execution_work_command("unqualified-later-address")
        .await
        .err()
        .context("unqualified absence was adopted")?;
    assert!(
        format!("{error:#}").contains("unavailable without tombstone"),
        "{error:#}"
    );
    assert_eq!(owner.capture_published_root()?.token, token);
    assert_eq!(
        runtime.runtime_db().connection()?.query_row(
            "SELECT COUNT(*) FROM input_evidence_current",
            [],
            |row| row.get::<_, usize>(0)
        )?,
        before
    );
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    Ok(())
}
