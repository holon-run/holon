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
    inject_episode_lost_ack(runtime).await?;
    Ok((scheduled, session))
}

async fn inject_episode_lost_ack(runtime: &RuntimeHandle) -> Result<()> {
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
            audit_events: Vec::new(),
            outcome,
        },
        runtime.now(),
    )?
    .with_fault(EpisodeMemoryFaultPoint::AfterCommitBeforeAck);
    let error = Box::pin(mutation.commit_episode_memory(&command))
        .await
        .unwrap_err();
    assert_eq!(
        crate::runtime_db::persistence_certainty(&error),
        Some(crate::runtime_db::PersistenceCertainty::Indeterminate)
    );
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
    let (_home, runtime, _scheduled, _session) = episode_lost_ack_fixture(false).await?;
    // This is actual receipt reconciliation, not a manually narrowed snapshot.
    drop(runtime.begin_mutation().await?);
    let published = runtime.inner.mutation_owner.capture()?.unwrap();
    // Positive recovery completes the publication cut: State plus live domains.
    assert!(published.input_evidence.is_some() && published.work.is_some());
    assert!(published.tasks.is_some() && published.scheduling.is_some());
    let observed = runtime
        .capture_observer_root_async(runtime.runtime_db())
        .await?;
    let projection = runtime.operator_state_from_root(&observed, 10, 10);
    let current = runtime.inner.mutation_owner.capture()?.unwrap();
    assert!(Arc::ptr_eq(&published, &current));
    projection?;
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
