use super::*;
use crate::{
    domain::execution_protocol::WorkItemExecutionRecord,
    runtime::{
        resident_input_evidence::{
            InputEvidenceFamily as Family, InputEvidenceKey as Key, PreparedInputEvidence,
            QualifiedInputEvidence,
        },
        tests::support::{context_config, seed_live_waiting_work_execution},
    },
    types::{AuthorityClass, MessageBody, MessageKind, MessageOrigin, Priority},
};
use tempfile::{tempdir, TempDir};

pub(super) async fn fixture() -> Result<(TempDir, TempDir, RuntimeHandle, TimerRecord)> {
    let home = tempdir()?;
    let workspace = tempdir()?;
    let runtime = RuntimeHandle::new(
        "default",
        home.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(crate::provider::StubProvider::new("unused")),
        "default".into(),
        context_config(),
    )?;
    runtime.await_initial_resources().await;
    let now = runtime.now();
    let timer = TimerRecord {
        calendar: None,
        id: crate::ids::timer_id(),
        agent_id: "default".into(),
        created_at: now,
        duration_ms: 3_600_000,
        interval_ms: Some(3_600_000),
        repeat: true,
        status: TimerStatus::Active,
        summary: None,
        next_fire_at: Some(now + chrono::Duration::hours(1)),
        last_fired_at: None,
        fire_count: 0,
    };
    crate::runtime::tests::support::persist_timer_creation_for_restart(&runtime, &timer).await?;
    Ok((home, workspace, runtime, timer))
}

pub(super) async fn waiting_work(
    runtime: &RuntimeHandle,
    timer: &TimerRecord,
) -> Result<(WorkItemRecord, WaitConditionRecord)> {
    let work = Box::pin(runtime.create_work_item(
        "retain observed timer cancellation Input".into(),
        None,
        None,
        Vec::new(),
    ))
    .await?;
    let registration = Box::pin(runtime.register_wait_for(
        "default",
        Some(work.id.clone()),
        WaitForWakeKind::Timer,
        Some(timer.id.clone()),
        "wait for exact timer".into(),
        None,
    ))
    .await?;
    let work = runtime.latest_work_item(&work.id).await?.unwrap();
    Ok((work, registration.condition))
}

fn canonical_execution(runtime: &RuntimeHandle, work_id: &str) -> Result<WorkItemExecutionRecord> {
    runtime.runtime_db().transaction(|tx| {
        let json: String = tx.query_row(
            "SELECT payload_json FROM execution_protocol_work_items
             WHERE agent_id='default' AND work_item_id=?1",
            [work_id],
            |row| row.get(0),
        )?;
        Ok(serde_json::from_str(&json)?)
    })
}

pub(super) fn assert_qualified_canonical(
    runtime: &RuntimeHandle,
    observed: &QualifiedInputEvidence,
) -> Result<()> {
    let PreparedInputEvidence::WorkExecution {
        work_item_id,
        record,
    } = &observed.record
    else {
        panic!("expected observed WorkExecution");
    };
    assert_eq!(record, &canonical_execution(runtime, work_item_id)?);
    let alias: (u64, String, String) = runtime.runtime_db().transaction(|tx| {
        Ok(tx.query_row(
            "SELECT revision, content_digest, availability FROM input_evidence_current
             WHERE agent_id=?1 AND identity_incarnation=?2
               AND family='work_execution' AND record_id=?3",
            rusqlite::params![
                observed.locator.scope.agent_id,
                observed.locator.scope.identity_incarnation,
                work_item_id,
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?)
    })?;
    assert_eq!(alias.0, observed.locator.revision);
    assert_eq!(alias.1, observed.locator.content_digest);
    assert_eq!(alias.2, "present");
    Ok(())
}

pub(super) async fn observe_execution(
    runtime: &RuntimeHandle,
    work_id: &str,
) -> Result<Arc<QualifiedInputEvidence>> {
    let key = Key::new(Family::WorkExecution, work_id);
    let mut mutation = runtime.begin_mutation().await?;
    mutation
        .prepare_owner_inputs([key.clone()].into_iter().collect(), Default::default())
        .await?;
    let captured = mutation.capture_input_evidence()?;
    let observed = captured.lookup(&key)?.unwrap().clone();
    assert_qualified_canonical(runtime, &observed)?;
    Ok(observed)
}

fn operator_input() -> MessageEnvelope {
    MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "independent input after timer cancellation".into(),
        },
    )
}

fn protocol_counts(runtime: &RuntimeHandle) -> Result<(u64, u64)> {
    runtime.runtime_db().transaction(|tx| {
        Ok(tx.query_row(
            "SELECT
               (SELECT count(*) FROM execution_protocol_work_items WHERE agent_id='default'),
               (SELECT count(*) FROM execution_protocol_attempts WHERE agent_id='default')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?)
    })
}

pub(super) fn execution_qualifications(
    runtime: &RuntimeHandle,
) -> Result<Vec<(String, u64, String, String)>> {
    runtime.runtime_db().transaction(|tx| {
        let mut statement = tx.prepare(
            "SELECT record_id, revision, content_digest, qualification
             FROM input_evidence_current
             WHERE agent_id='default' AND family='work_execution'
             ORDER BY identity_incarnation, record_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    })
}

#[tokio::test]
async fn timer_cancel_input_observed_fresh_work_execution_remains_qualified() -> Result<()> {
    let (_home, _workspace, runtime, timer) = fixture().await?;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (work, wait) = waiting_work(&runtime, &timer).await?;
    seed_live_waiting_work_execution(&runtime, &work, &wait.id).await;
    let before = observe_execution(&runtime, &work.id).await?;
    // Retain the original published domain as well as its qualified address.
    let original = runtime.inner.mutation_owner.capture()?.unwrap();
    let before_events = runtime.storage().read_recent_events(100)?;
    let before_pending = runtime.agent_state().await?.pending;
    let before_protocol = protocol_counts(&runtime)?;

    assert_eq!(
        runtime.cancel_timer(&timer.id).await?.status,
        TimerStatus::Cancelled
    );
    let cancelled_work = runtime.latest_work_item(&work.id).await?.unwrap();
    assert_eq!(cancelled_work.revision, work.revision + 1);
    assert!(cancelled_work
        .blocked_by
        .as_deref()
        .unwrap()
        .starts_with("timer_cancelled:"));
    assert!(cancelled_work.blocked_by_wait_id.is_none());
    let cancelled_wait = runtime
        .runtime_db()
        .wait_conditions()
        .latest_for_agent("default")?
        .into_iter()
        .find(|candidate| candidate.id == wait.id)
        .unwrap();
    assert_eq!(cancelled_wait.status, WaitConditionStatus::Cancelled);
    assert_eq!(runtime.agent_state().await?.pending, before_pending);
    assert_eq!(protocol_counts(&runtime)?, before_protocol);

    // No preparation, qualification deletion, or repair is allowed here:
    // the original CancelTimer must publish its own exact Input delta.
    let published = runtime.inner.mutation_owner.capture()?.unwrap();
    let captured = published.input_evidence.as_ref().unwrap().capture()?;
    let key = Key::new(Family::WorkExecution, &work.id);
    let after = captured.lookup(&key)?.unwrap().clone();
    // Exercise ordinary ingress/selection before diagnostic comparisons, so
    // the regression exposes ProjectionChanged through the runtime path.
    let message = runtime.enqueue(operator_input()).await?;
    runtime
        .begin_interactive_turn_with_provenance(Some(&message), None, None, None)
        .await?;
    assert_qualified_canonical(&runtime, &after)?;
    assert_eq!(
        canonical_execution(&runtime, &work.id)?.source_revision,
        cancelled_work.revision
    );
    assert_eq!(after.locator.revision, before.locator.revision + 1);
    assert_ne!(after.locator.content_digest, before.locator.content_digest);
    assert_eq!(
        original
            .input_evidence
            .as_ref()
            .unwrap()
            .capture()?
            .lookup(&key)?
            .unwrap()
            .as_ref(),
        before.as_ref()
    );
    assert!(runtime.storage().read_recent_events(100)?.len() > before_events.len());

    // The independent operator input must not unblock the cancelled Work.
    assert!(runtime
        .latest_work_item(&work.id)
        .await?
        .unwrap()
        .blocked_by
        .is_some());
    Ok(())
}

#[tokio::test]
async fn timer_cancel_input_unobserved_execution_does_not_create_authority() -> Result<()> {
    let (_home, _workspace, runtime, timer) = fixture().await?;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (work, _) = waiting_work(&runtime, &timer).await?;
    let before_protocol = protocol_counts(&runtime)?;
    assert_eq!(before_protocol.1, 0);
    let before_qualifications = execution_qualifications(&runtime)?;
    let before_pending = runtime.agent_state().await?.pending;

    runtime.cancel_timer(&timer.id).await?;

    assert_eq!(protocol_counts(&runtime)?, before_protocol);
    let after_qualifications = execution_qualifications(&runtime)?;
    assert_eq!(
        after_qualifications
            .iter()
            .map(|row| row.0.as_str())
            .collect::<Vec<_>>(),
        before_qualifications
            .iter()
            .map(|row| row.0.as_str())
            .collect::<Vec<_>>()
    );
    // Cancellation may advance the existing source revision, but must not mint
    // another execution or qualification. The ACK has to match canonical SQL.
    observe_execution(&runtime, &work.id).await?;
    assert_eq!(runtime.agent_state().await?.pending, before_pending);
    assert!(runtime
        .latest_work_item(&work.id)
        .await?
        .unwrap()
        .blocked_by
        .as_deref()
        .unwrap()
        .starts_with("timer_cancelled:"));
    let message = runtime.enqueue(operator_input()).await?;
    runtime
        .begin_interactive_turn_with_provenance(Some(&message), None, None, None)
        .await?;
    Ok(())
}

#[tokio::test]
async fn timer_cancel_input_only_updates_exact_wait_and_repeat_is_idempotent() -> Result<()> {
    let (_home, _workspace, runtime, timer) = fixture().await?;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let mut other_timer = timer.clone();
    other_timer.id = crate::ids::timer_id();
    crate::runtime::tests::support::persist_timer_creation_for_restart(&runtime, &other_timer)
        .await?;
    let (work, wait) = waiting_work(&runtime, &timer).await?;
    let (other_work, other_wait) = waiting_work(&runtime, &other_timer).await?;
    seed_live_waiting_work_execution(&runtime, &work, &wait.id).await;
    seed_live_waiting_work_execution(&runtime, &other_work, &other_wait.id).await;
    let before = observe_execution(&runtime, &work.id).await?;
    let other_before = observe_execution(&runtime, &other_work.id).await?;
    let before_protocol = protocol_counts(&runtime)?;
    let other_wait_before = runtime
        .runtime_db()
        .wait_conditions()
        .latest_for_agent("default")?
        .into_iter()
        .find(|candidate| candidate.id == other_wait.id)
        .unwrap();

    let cancelled = runtime.cancel_timer(&timer.id).await?;

    let published = runtime.inner.mutation_owner.capture()?.unwrap();
    let captured = published.input_evidence.as_ref().unwrap().capture()?;
    let selected = captured
        .lookup(&Key::new(Family::WorkExecution, &work.id))?
        .unwrap();
    assert_qualified_canonical(&runtime, selected)?;
    assert_eq!(selected.locator.revision, before.locator.revision + 1);
    let other = captured
        .lookup(&Key::new(Family::WorkExecution, &other_work.id))?
        .unwrap();
    assert_eq!(other.as_ref(), other_before.as_ref());
    assert_qualified_canonical(&runtime, other)?;
    assert_eq!(
        runtime.latest_work_item(&other_work.id).await?.unwrap(),
        other_work
    );
    assert_eq!(
        runtime
            .runtime_db()
            .wait_conditions()
            .latest_for_agent("default")?
            .into_iter()
            .find(|candidate| candidate.id == other_wait.id)
            .unwrap(),
        other_wait_before
    );
    assert_eq!(protocol_counts(&runtime)?, before_protocol);
    let execution = canonical_execution(&runtime, &work.id)?;
    let qualifications = execution_qualifications(&runtime)?;
    let events = runtime.storage().read_recent_events(100)?;

    assert_eq!(runtime.cancel_timer(&timer.id).await?, cancelled);

    assert_eq!(canonical_execution(&runtime, &work.id)?, execution);
    assert_eq!(execution_qualifications(&runtime)?, qualifications);
    assert_eq!(runtime.storage().read_recent_events(100)?, events);
    assert_eq!(protocol_counts(&runtime)?, before_protocol);
    let repeated = runtime.inner.mutation_owner.capture()?.unwrap();
    assert_eq!(
        repeated
            .input_evidence
            .as_ref()
            .unwrap()
            .capture()?
            .lookup(&Key::new(Family::WorkExecution, &work.id))?
            .unwrap()
            .as_ref(),
        selected.as_ref()
    );
    Ok(())
}

#[tokio::test]
async fn timer_cancel_input_without_work_preserves_empty_authority_and_repeat() -> Result<()> {
    for lifecycle_wait in [false, true] {
        let (_home, _workspace, runtime, timer) = fixture().await?;
        let _refresh = runtime.hold_resource_refresh_for_test().await?;
        let wait = if lifecycle_wait {
            Some(
                Box::pin(runtime.register_wait_for(
                    "default",
                    None,
                    WaitForWakeKind::Timer,
                    Some(timer.id.clone()),
                    "agent lifecycle timer wait".into(),
                    None,
                ))
                .await?
                .condition,
            )
        } else {
            None
        };
        let qualifications = execution_qualifications(&runtime)?;
        let protocol = protocol_counts(&runtime)?;
        let pending = runtime.agent_state().await?.pending;

        let cancelled = runtime.cancel_timer(&timer.id).await?;

        assert_eq!(protocol_counts(&runtime)?, protocol);
        assert_eq!(execution_qualifications(&runtime)?, qualifications);
        assert_eq!(runtime.agent_state().await?.pending, pending);
        let waits = runtime
            .runtime_db()
            .wait_conditions()
            .latest_for_agent("default")?;
        if let Some(wait) = wait {
            assert_eq!(
                waits
                    .iter()
                    .find(|candidate| candidate.id == wait.id)
                    .unwrap()
                    .status,
                WaitConditionStatus::Cancelled
            );
        } else {
            assert!(waits.is_empty());
        }
        let events = runtime.storage().read_recent_events(100)?;

        assert_eq!(runtime.cancel_timer(&timer.id).await?, cancelled);

        assert_eq!(protocol_counts(&runtime)?, protocol);
        assert_eq!(execution_qualifications(&runtime)?, qualifications);
        assert_eq!(runtime.storage().read_recent_events(100)?, events);
    }
    Ok(())
}

#[tokio::test]
async fn timer_cancel_input_raw_canonical_change_still_fails_closed() -> Result<()> {
    use crate::runtime_db::input_evidence_snapshot::{
        load_input_evidence_snapshot_tx, InputEvidenceLoadRequest, InputEvidenceSnapshotError,
    };

    let (_home, _workspace, runtime, timer) = fixture().await?;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (work, wait) = waiting_work(&runtime, &timer).await?;
    seed_live_waiting_work_execution(&runtime, &work, &wait.id).await;
    let before = observe_execution(&runtime, &work.id).await?;
    let qualifications = execution_qualifications(&runtime)?;
    let original_token = runtime.inner.mutation_owner.capture()?.unwrap().token;
    runtime.runtime_db().transaction(|tx| {
        assert_eq!(
            tx.execute(
                "UPDATE execution_protocol_work_items
                 SET payload_json=json_set(payload_json, '$.source_revision', ?1)
                 WHERE agent_id='default' AND work_item_id=?2",
                rusqlite::params![work.revision + 1, work.id],
            )?,
            1
        );
        Ok(())
    })?;

    let request = InputEvidenceLoadRequest {
        query: None,
        windows: Vec::new(),
        mandatory_roots: [Key::new(Family::WorkExecution, &work.id)]
            .into_iter()
            .collect(),
        interrupted_sources: Default::default(),
        selected_completion_brief_ids: Default::default(),
        record_limit: 512,
        byte_limit: 1024 * 1024,
    };
    // An already prepared resident request intentionally does no SQL. Use the
    // normal fresh qualified read to exercise the canonical mismatch guard.
    let error = runtime
        .runtime_db()
        .transaction(|tx| load_input_evidence_snapshot_tx(tx, &before.locator.scope, &request))
        .expect_err("raw canonical changes must not acquire Input authority");
    assert!(
        matches!(
            error.downcast_ref::<InputEvidenceSnapshotError>(),
            Some(InputEvidenceSnapshotError::ProjectionChanged(_))
        ) || error.root_cause().to_string() == "prepared evidence raw/normalized digest mismatch",
        "{error:#}"
    );
    assert_eq!(execution_qualifications(&runtime)?, qualifications);
    let published = runtime.inner.mutation_owner.capture()?.unwrap();
    assert_eq!(published.token, original_token);
    assert_eq!(
        published
            .input_evidence
            .as_ref()
            .unwrap()
            .capture()?
            .lookup(&Key::new(Family::WorkExecution, &work.id))?
            .unwrap()
            .as_ref(),
        before.as_ref()
    );
    Ok(())
}

#[tokio::test]
async fn timer_cancel_input_observed_legacy_work_execution_remains_qualified() -> Result<()> {
    use crate::{
        domain::execution_protocol::{
            ExecutionProtocolState, WaitReference, WorkItemExecutionState,
        },
        runtime_db::{
            evidence::{upsert_agent_identity_tx, upsert_agent_state_tx},
            migrations::{apply_migration, ensure_migration_table, MIGRATIONS},
            repositories::upsert_timer_tx,
            transitions::execution_protocol_repository::persist_state_tx,
        },
    };

    let (_source_home, _source_workspace, source, timer) = fixture().await?;
    let _source_refresh = source.hold_resource_refresh_for_test().await?;
    let (work, wait) = waiting_work(&source, &timer).await?;
    let identity = source.inner.mutation_owner.capture()?.unwrap();
    let home = tempdir()?;
    let workspace = tempdir()?;
    let state_dir = home.path().join(".holon/state");
    std::fs::create_dir_all(&state_dir)?;
    // Build a released historical database before the real continuity/Input
    // migrations. No live qualification is deleted or forged for adoption.
    {
        let mut connection = rusqlite::Connection::open(state_dir.join("runtime.sqlite"))?;
        ensure_migration_table(&connection)?;
        for migration in MIGRATIONS
            .iter()
            .filter(|migration| migration.version <= 89)
        {
            apply_migration(&mut connection, migration)?;
        }
        let tx = connection.transaction()?;
        upsert_agent_identity_tx(&tx, identity.identity.as_ref().unwrap())?;
        upsert_agent_state_tx(&tx, &AgentState::new("default"))?;
        tx.execute(
            "INSERT INTO storage_domains
             (domain,schema_version,import_status,canonical_source,updated_at)
             VALUES('context_episode_anchors',1,'complete','db',?1)",
            [work.updated_at.to_rfc3339()],
        )?;
        tx.execute(
            "INSERT INTO work_items
             (work_item_id,agent_id,state,objective,revision,created_at,updated_at,payload_json)
             VALUES(?1,?2,'open',?3,?4,?5,?6,?7)",
            rusqlite::params![
                work.id,
                work.agent_id,
                work.objective,
                work.revision,
                work.created_at.to_rfc3339(),
                work.updated_at.to_rfc3339(),
                serde_json::to_string(&work)?,
            ],
        )?;
        upsert_timer_tx(&tx, &timer)?;
        assert_eq!(wait.status, WaitConditionStatus::Active);
        assert_eq!(wait.kind, WaitConditionKind::Timer);
        // Current writers require payload columns added after this schema.
        tx.execute(
            "INSERT INTO wait_conditions
             (wait_condition_id,agent_id,work_item_id,status,kind,source,subject_ref,
              waiting_for,created_at,updated_at,payload_json)
             VALUES(?1,?2,?3,'active','timer',?4,?5,?6,?7,?8,?9)",
            rusqlite::params![
                wait.id,
                wait.agent_id,
                wait.work_item_id,
                wait.source,
                wait.subject_ref,
                wait.waiting_for,
                wait.created_at.to_rfc3339(),
                wait.updated_at.to_rfc3339(),
                serde_json::to_string(&wait)?,
            ],
        )?;
        let mut execution = ExecutionProtocolState::empty("default");
        execution.work_items.insert(
            work.id.clone(),
            WorkItemExecutionRecord {
                source_revision: work.revision,
                state: WorkItemExecutionState::Waiting {
                    generation: work.revision.max(1),
                    wait: WaitReference {
                        wait_id: wait.id.clone(),
                    },
                },
            },
        );
        persist_state_tx(&tx, &execution)?;
        tx.commit()?;
    }
    let runtime = RuntimeHandle::new(
        "default",
        home.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(crate::provider::StubProvider::new("unused")),
        "default".into(),
        context_config(),
    )?;
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let mode: String = runtime.runtime_db().transaction(|tx| {
        Ok(tx.query_row(
            "SELECT mode FROM agent_episode_continuity
             WHERE agent_id='default' AND identity_incarnation=?1 AND policy_version=1",
            [runtime.inner.identity_incarnation],
            |row| row.get(0),
        )?)
    })?;
    assert_eq!(mode, "legacy_adopted");
    let before = observe_execution(&runtime, &work.id).await?;
    assert!(execution_qualifications(&runtime)?
        .iter()
        .any(|row| row.0 == work.id && row.3 == "legacy_prepared"));
    let original = runtime.inner.mutation_owner.capture()?.unwrap();
    let before_protocol = protocol_counts(&runtime)?;
    let before_pending = runtime.agent_state().await?.pending;

    runtime.cancel_timer(&timer.id).await?;

    assert_eq!(protocol_counts(&runtime)?, before_protocol);
    assert_eq!(runtime.agent_state().await?.pending, before_pending);
    let published = runtime.inner.mutation_owner.capture()?.unwrap();
    let captured = published.input_evidence.as_ref().unwrap().capture()?;
    let after = captured
        .lookup(&Key::new(Family::WorkExecution, &work.id))?
        .unwrap();
    let message = runtime.enqueue(operator_input()).await?;
    runtime
        .begin_interactive_turn_with_provenance(Some(&message), None, None, None)
        .await?;
    assert_qualified_canonical(&runtime, after)?;
    assert_eq!(after.locator.revision, before.locator.revision + 1);
    assert_ne!(after.locator.content_digest, before.locator.content_digest);
    assert!(execution_qualifications(&runtime)?
        .iter()
        .any(|row| row.0 == work.id && row.3 == "committed_writer"));
    assert_eq!(
        original
            .input_evidence
            .as_ref()
            .unwrap()
            .capture()?
            .lookup(&Key::new(Family::WorkExecution, &work.id))?
            .unwrap()
            .as_ref(),
        before.as_ref()
    );
    assert!(runtime
        .latest_work_item(&work.id)
        .await?
        .unwrap()
        .blocked_by
        .as_deref()
        .unwrap()
        .starts_with("timer_cancelled:"));
    Ok(())
}
