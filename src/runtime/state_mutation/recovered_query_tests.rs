async fn episode_lost_ack_fixture(
    live_birth: bool,
) -> Result<(
    tempfile::TempDir,
    RuntimeHandle,
    crate::runtime::scheduler_executor::ScheduledMessage,
    Option<crate::runtime::task_queries::TaskBirthSession>,
)> {
    let (home, runtime) = fixture();
    let (scheduled, session) = episode_lost_ack_on(&runtime, live_birth).await?;
    Ok((home, runtime, scheduled, session))
}

async fn episode_lost_ack_on(
    runtime: &RuntimeHandle,
    live_birth: bool,
) -> Result<(
    crate::runtime::scheduler_executor::ScheduledMessage,
    Option<crate::runtime::task_queries::TaskBirthSession>,
)> {
    episode_commit_on(runtime, live_birth, true, Vec::new()).await
}

async fn episode_commit_on(
    runtime: &RuntimeHandle,
    live_birth: bool,
    lose_ack: bool,
    audit_events: Vec<crate::types::AuditEvent>,
) -> Result<(
    crate::runtime::scheduler_executor::ScheduledMessage,
    Option<crate::runtime::task_queries::TaskBirthSession>,
)> {
    use crate::runtime::scheduler_executor::{RunLoopPoll, SchedulerDecisionExecutor};
    runtime
        .enqueue(MessageEnvelope::new(
            runtime.instance_agent_id(),
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "recover original Episode receipt".into(),
            },
        ))
        .await?;
    let RunLoopPoll::Message(scheduled) = SchedulerDecisionExecutor::new(runtime).poll().await?
    else {
        anyhow::bail!("source was not claimed")
    };
    runtime
        .begin_interactive_turn(Some(&scheduled.message), None, None)
        .await?;
    let session = if live_birth {
        Some(runtime.begin_task_birth_session().await?.1)
    } else {
        None
    };
    commit_episode_fixture(runtime, lose_ack, audit_events).await?;
    Ok((scheduled, session))
}

async fn inject_episode_lost_ack(runtime: &RuntimeHandle) -> Result<()> {
    commit_episode_fixture(runtime, true, Vec::new()).await
}

async fn commit_episode_fixture(
    runtime: &RuntimeHandle,
    lose_ack: bool,
    audit_events: Vec<crate::types::AuditEvent>,
) -> Result<()> {
    use crate::{
        memory::episode_transition::{EpisodeMemoryOutcome, EpisodeMemoryPlan},
        runtime_db::episode_memory::{
            EpisodeMemoryFaultPoint, EpisodeMemoryInvocation, EpisodeMemoryKey,
            EpisodeMemoryTransitionCommand,
        },
    };
    let mut mutation = runtime.begin_mutation().await?;
    let state = mutation.state.clone();
    let binding = state
        .current_execution_binding
        .as_ref()
        .context("actual binding missing")?;
    let memory = &state.working_memory;
    let outcome = EpisodeMemoryOutcome {
        episode_memory_changed: false,
        business_fields_changed: false,
        boundary_reason: None,
        finalized_episode_id: None,
        previous_builder_id: memory
            .active_episode_builder
            .as_ref()
            .map(|value| value.id.clone()),
        active_builder_id: memory
            .active_episode_builder
            .as_ref()
            .map(|value| value.id.clone()),
        compression_epoch_before: memory.compression_epoch,
        compression_epoch_after: memory.compression_epoch,
        archived_episode_count_before: memory.archived_episode_count,
        archived_episode_count_after: memory.archived_episode_count,
    };
    let command = EpisodeMemoryTransitionCommand::new(
        EpisodeMemoryKey {
            agent_id: state.id.clone(),
            identity_incarnation: mutation.input_evidence_scope().identity_incarnation,
            actual_dispatch_turn_id: binding.turn_id.clone(),
        },
        EpisodeMemoryInvocation {
            message_id: binding.source_message_id.clone(),
            turn_index: state.turn_index,
        },
        state.clone(),
        EpisodeMemoryPlan {
            candidate: state.clone(),
            finalized_episode: None,
            audit_events,
            outcome,
        },
        runtime.now(),
    )?;
    if lose_ack {
        let command = command.with_fault(EpisodeMemoryFaultPoint::AfterCommitBeforeAck);
        let error = Box::pin(mutation.commit_episode_memory(&command))
            .await
            .unwrap_err();
        assert_eq!(
            crate::runtime_db::persistence_certainty(&error),
            Some(crate::runtime_db::PersistenceCertainty::Indeterminate)
        );
    } else {
        Box::pin(mutation.commit_episode_memory(&command)).await?;
    }
    drop(mutation);
    Ok(())
}

#[tokio::test]
async fn episode_recovery_prepares_secondary_loaded_owner_before_group_publication() -> Result<()> {
    struct HeldProvider {
        started: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }
    #[async_trait::async_trait]
    impl crate::provider::AgentProvider for HeldProvider {
        async fn complete_turn(
            &self,
            request: crate::provider::ProviderTurnRequest,
        ) -> Result<crate::provider::ProviderTurnResponse> {
            self.started.notify_one();
            self.release.notified().await;
            crate::provider::AgentProvider::complete_turn(&StubProvider::new("done"), request).await
        }
    }
    let home = tempdir()?;
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )?;
    let mut config = crate::config::AppConfig::load_with_home(Some(home.path().to_owned()))?;
    config.user_home_dir = Some(home.path().to_owned());
    let provider = Arc::new(HeldProvider {
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let host = crate::host::RuntimeHost::new_with_provider(config, provider.clone())?;
    let parent = host.default_runtime().await?;
    host.create_named_agent("recovery-trigger", None).await?;
    let trigger = host.get_public_agent("recovery-trigger").await?;
    parent.wait_for_bootstrap().await?;
    trigger.wait_for_bootstrap().await?;
    parent
        .enqueue(MessageEnvelope::new(
            parent.instance_agent_id(),
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "hold the original dispatch during group recovery".into(),
            },
        ))
        .await?;
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        provider.started.notified(),
    )
    .await?;
    let _session = parent.begin_task_birth_session().await?.1;
    inject_episode_lost_ack(&parent).await?;
    // The same multi-owner boundary used by supervised invocation: the owner
    // with the original lost ACK is not the runtime initiating reconciliation.
    drop(
        trigger
            .begin_mutation_with_owners(vec![
                trigger.inner.mutation_owner.clone(),
                parent.inner.mutation_owner.clone(),
            ])
            .await?,
    );
    let root = parent.inner.mutation_owner.capture()?.unwrap();
    assert!(root.input_evidence.is_some() && root.work.is_some());
    assert!(root.tasks.is_some() && root.scheduling.is_some());
    let observed = parent
        .capture_observer_root_async(parent.runtime_db())
        .await?;
    parent.operator_state_from_root(&observed, 10, 10)?;
    provider.release.notify_one();
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn episode_recovery_publishes_live_domains_before_observer_capture() -> Result<()> {
    let (_home, runtime) = fixture();
    let event = crate::types::AuditEvent::legacy("episode_state_facts", serde_json::json!({}));
    let event_id = event.id.clone();
    let (_scheduled, _session) = episode_commit_on(&runtime, false, true, vec![event]).await?;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    // This is actual receipt reconciliation, not a manually narrowed snapshot.
    drop(runtime.begin_mutation().await?);
    let published = runtime.inner.mutation_owner.capture()?.unwrap();
    // Positive recovery completes the publication cut: State plus live domains.
    assert!(published.input_evidence.is_some() && published.work.is_some());
    assert!(published.tasks.is_some() && published.scheduling.is_some());
    assert!(published.queue_posture.is_some() && published.scheduler_events.is_some());
    let db = runtime.runtime_db();
    let reads = db.reader_pool_stats_for_tests().checkout_attempts;
    let preparations = db.preparation_pool_stats_for_tests().checkout_attempts;
    let writer = db.reader_pool_writer_weak_for_tests().upgrade().unwrap();
    let writes = writer.write_channel_admissions_for_tests()?;
    let projection = runtime.operator_state_read_projection(10, 10).await?;
    let current = runtime.inner.mutation_owner.capture()?.unwrap();
    assert!(Arc::ptr_eq(&published, &current));
    assert_eq!(db.reader_pool_stats_for_tests().checkout_attempts, reads);
    assert_eq!(db.preparation_pool_stats_for_tests().checkout_attempts, preparations);
    assert_eq!(writer.write_channel_admissions_for_tests()?, writes);
    drop(projection);
    assert_episode_event_history_only(
        &runtime,
        &event_id,
        published.scheduler_events.unwrap().event_cursor,
    )?;
    Ok(())
}

fn assert_episode_event_history_only(
    runtime: &RuntimeHandle,
    event_id: &str,
    event_cursor: u64,
) -> Result<()> {
    use crate::runtime_db::current_query::{prepare, prepare_event_page, CurrentSelection};
    let scope = crate::runtime::resident_input_evidence::InputEvidenceScope {
        agent_id: runtime.instance_agent_id().to_owned(),
        identity_incarnation: runtime.inner.identity_incarnation,
    };
    let selection = CurrentSelection {
        events_after: Some(event_cursor.checked_sub(1).context("Episode Event cursor missing")?),
        ..Default::default()
    };
    runtime.runtime_db().with_read(|connection| {
        let tx = connection.transaction()?;
        let ledger = &runtime.inner.input_evidence_budget;
        let page = prepare_event_page(
            &tx,
            &scope,
            ledger,
            None,
            selection.events_after,
            1,
            crate::storage::EventLogPageOrder::Asc,
            |_| true,
        )?;
        assert_eq!(page.events().map(|event| event.id.as_str()).collect::<Vec<_>>(), [event_id]);
        // A standalone Episode archive ACK is not live Current authority.
        let error = prepare(&tx, &scope, Some(&selection), ledger, (1024 * 1024, 1024))
            .err()
            .context("Episode history ACK granted live Current authority")?;
        assert!(error.to_string().contains("Current body needs original writer preparation"));
        Ok(())
    })
}

#[tokio::test]
async fn original_episode_event_facts_publish_without_current_preparation() -> Result<()> {
    let (_home, runtime) = fixture();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let event = crate::types::AuditEvent::legacy("episode_state_facts", serde_json::json!({}));
    let event_id = event.id.clone();
    let (_scheduled, _session) = episode_commit_on(&runtime, false, false, vec![event]).await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    assert!(root.queue_posture.is_some());
    let events = root.scheduler_events.context("Episode Event facts missing")?;
    assert!(events.event_cursor > 0);
    let reads = runtime.runtime_db().reader_pool_stats_for_tests().checkout_attempts;
    let preparations = runtime.runtime_db().preparation_pool_stats_for_tests().checkout_attempts;
    drop(runtime.operator_state_read_projection(10, 10).await?);
    assert_eq!(runtime.runtime_db().reader_pool_stats_for_tests().checkout_attempts, reads);
    assert_eq!(runtime.runtime_db().preparation_pool_stats_for_tests().checkout_attempts, preparations);
    assert!(Arc::ptr_eq(&root, &runtime.inner.mutation_owner.capture()?.unwrap()));
    assert_episode_event_history_only(&runtime, &event_id, events.event_cursor)?;
    Ok(())
}

#[tokio::test]
async fn stop_prepares_live_input_query_after_episode_recovery() -> Result<()> {
    let (_home, runtime, scheduled, _session) = episode_lost_ack_fixture(true).await?;
    crate::runtime::tests::support::finish_claimed_test_run(&runtime).await;
    drop(scheduled);
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        runtime.control(crate::types::ControlAction::Stop),
    )
    .await??;
    assert_eq!(
        runtime.agent_state().await?.status,
        crate::types::AgentStatus::Stopped
    );
    Ok(())
}

#[tokio::test]
async fn abort_prepares_live_input_query_after_episode_recovery() -> Result<()> {
    let (_home, runtime, scheduled, _session) = episode_lost_ack_fixture(true).await?;
    let abort = runtime
        .current_run_abort_token()
        .await
        .context("claimed run has no abort handle")?;
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        runtime.abort_current_run(crate::runtime::CurrentRunAbortRequest {
            run_id: Some(abort.run_id.clone()),
            mode: crate::runtime::CurrentRunAbortMode::StopAfterAbort,
        }),
    )
    .await??;
    assert_eq!(outcome.run_id, abort.run_id);
    assert!(abort.token.is_cancelled());
    assert_eq!(
        runtime.agent_state().await?.status,
        crate::types::AgentStatus::Stopped
    );
    crate::runtime::tests::support::finish_claimed_test_run(&runtime).await;
    drop(scheduled);
    Ok(())
}

#[tokio::test]
async fn loop_failure_persists_after_episode_recovery_with_live_input_query() -> Result<()> {
    let (_home, runtime, scheduled, _session) = episode_lost_ack_fixture(true).await?;
    crate::runtime::tests::support::finish_claimed_test_run(&runtime).await;
    drop(scheduled);
    runtime
        .record_runtime_loop_failure(&anyhow::anyhow!("original fixture loop failure"))
        .await;
    let raw: String = runtime.runtime_db().connection()?.query_row(
        "SELECT payload_json FROM agent_states WHERE agent_id='scheduler-resident'",
        [],
        |row| row.get(0),
    )?;
    let state: crate::types::AgentState = serde_json::from_str(&raw)?;
    assert!(state.current_run_id.is_none());
    let failure = state
        .last_runtime_failure
        .as_ref()
        .context("original loop failure must be durably recorded")?;
    assert!(failure.summary.contains("original fixture loop failure"));
    assert_eq!(
        runtime.agent_state().await?.last_runtime_failure,
        state.last_runtime_failure
    );
    Ok(())
}

#[tokio::test]
async fn episode_recovery_rejects_corrupt_live_input_without_replaying_commit() -> Result<()> {
    let (_home, runtime, scheduled, _session) = episode_lost_ack_fixture(true).await?;
    // Keep JSON indexes valid while injecting a malformed typed Message body.
    // This corruption is separate from the legal lost-ACK sequence above.
    let payload: String = {
        let connection = runtime.runtime_db().connection()?;
        let payload = connection.query_row(
            "SELECT payload_json FROM messages WHERE evidence_id=?1",
            [&scheduled.message.id],
            |row| row.get(0),
        )?;
        assert_eq!(
            connection.execute(
                "UPDATE messages SET payload_json='{}' WHERE evidence_id=?1",
                [&scheduled.message.id],
            )?,
            1
        );
        payload
    };
    for _ in 0..2 {
        assert!(
            runtime.begin_mutation().await.is_err(),
            "corrupt live Input must keep receipt recovery fenced"
        );
        assert!(runtime.inner.mutation_owner.capture().is_err());
        let count: i64 = runtime.runtime_db().connection()?.query_row(
            "SELECT COUNT(*) FROM episode_memory_results",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(count, 1, "recovery must not replay the Episode write");
    }
    assert_eq!(
        runtime.runtime_db().connection()?.execute(
            "UPDATE messages SET payload_json=?1 WHERE evidence_id=?2",
            rusqlite::params![payload, scheduled.message.id],
        )?,
        1
    );
    drop(runtime.begin_mutation().await?);
    runtime.agent_state().await?;
    crate::runtime::tests::support::finish_claimed_test_run(&runtime).await;
    drop(scheduled);
    Ok(())
}
