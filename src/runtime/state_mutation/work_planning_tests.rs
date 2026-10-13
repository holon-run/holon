use super::*;
use crate::runtime::{
    tests::support::{context_config, tempdir, StubProvider},
    WaitForWakeKind,
};

fn fixture() -> (tempfile::TempDir, RuntimeHandle) {
    let home = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "scheduler-resident",
        home.path().into(),
        None::<crate::types::WorkspaceEntry>,
        String::new(),
        Arc::new(StubProvider::new("unused")),
        "scheduler-resident".into(),
        context_config(),
    )
    .unwrap();
    (home, runtime)
}

#[tokio::test]
async fn scheduler_acquisition_matches_legacy_live_work_and_active_wait_projection() -> Result<()> {
    let (_home, runtime) = fixture();
    let work = runtime
        .create_work_item("waiting original Work".into(), None, None, Vec::new())
        .await?;
    runtime
        .register_wait_for(
            "scheduler-resident",
            Some(work.id.clone()),
            WaitForWakeKind::External,
            Some("github:holon-run/holon#2258".into()),
            "wait for external".into(),
            None,
        )
        .await?;
    let mut mutation = runtime.begin_mutation().await?;
    let now = runtime.now();
    let read = mutation
        .prepare_scheduler_read(None, Default::default(), Default::default(), now)
        .await?;
    let legacy = SchedulerProjection::from_state_with_queue_len_at(
        &runtime.inner.storage,
        &read.state,
        mutation.queue.len(),
        now,
    )?;
    assert_eq!(read.projection, legacy);
    assert_eq!(
        read.scheduling_posture,
        runtime.storage().agent_posture_projection(&read.state)?
    );
    assert_eq!(
        read.active_waits,
        runtime
            .storage()
            .active_wait_conditions_for_agent(&read.state.id)?
    );
    assert_eq!(
        read.advisories,
        scheduler::scheduling_advisories_with_queue_len(
            runtime.storage(),
            &read.state,
            mutation.queue.len(),
        )?
    );
    assert_eq!(read.work.live_work_items()?.len(), 1);
    assert_eq!(read.work.unresolved_wait_conditions()?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn scheduler_triggered_wait_capture_and_dispatch_survive_storage_removal() -> Result<()> {
    let (_home, runtime) = fixture();
    let registered = runtime
        .register_wait_for(
            "scheduler-resident",
            None,
            WaitForWakeKind::OperatorInput,
            None,
            "operator resume".into(),
            None,
        )
        .await?;
    let message = runtime
        .enqueue(MessageEnvelope::new(
            "scheduler-resident",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: Some("test".into()),
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "resume".into(),
            },
        ))
        .await?;
    let mut mutation = runtime.begin_mutation().await?;
    let now = runtime.now();
    let read = mutation
        .prepare_scheduler_read(Some(&message), Default::default(), Default::default(), now)
        .await?;
    let legacy = SchedulerProjection::from_state_with_queue_len_at(
        &runtime.inner.storage,
        &read.state,
        mutation.queue.len(),
        now,
    )?;
    assert_eq!(read.projection, legacy);
    let waits = read.work.waits_triggered_by(&message.id)?;
    assert_eq!(waits.len(), 1);
    assert_eq!(waits[0].id, registered.condition.id);
    assert_eq!(waits[0].status, WaitConditionStatus::Triggered);
    assert_eq!(read.projection.active_waiting_intents, 0);
    drop(mutation);
    // The decision, posture and advisories read only this selected capture.
    let expected_posture = runtime.storage().agent_posture_projection(&read.state)?;
    let expected_advisories = scheduler::scheduling_advisories_with_queue_len(
        runtime.storage(),
        &read.state,
        read.projection.queue_len,
    )?;
    std::fs::remove_file(runtime.inner.runtime_db.path())?;
    assert_eq!(read.scheduling_posture, expected_posture);
    assert_eq!(read.advisories, expected_advisories);
    assert!(read.active_waits.is_empty());
    let closure =
        RuntimeHandle::closure_decision_from_projection(&read.state, &read.projection, None);
    let dispatch = runtime.build_message_dispatch_plan_from_work(
        &message,
        closure,
        &read.state,
        &read.work,
    )?;
    let _decision = scheduler::decide_next_action(
        &read.projection,
        scheduler::SchedulerBoundary::RunLoop,
        scheduler::SchedulerInput::Message {
            message: &message,
            model_turn_allowed: dispatch.model_turn_allowed,
            continuation_resolution: dispatch.continuation_resolution.as_ref(),
        },
    );
    Ok(())
}

#[tokio::test]
async fn scheduler_dispatch_requires_explicit_trigger_relation_preparation() -> Result<()> {
    let (_home, runtime) = fixture();
    let mut mutation = runtime.begin_mutation().await?;
    let read = mutation
        .prepare_scheduler_read(None, Default::default(), Default::default(), runtime.now())
        .await?;
    drop(mutation);
    let message = MessageEnvelope::new(
        "scheduler-resident",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "not admitted/captured".into(),
        },
    );
    let closure =
        RuntimeHandle::closure_decision_from_projection(&read.state, &read.projection, None);
    let error = match runtime.build_message_dispatch_plan_from_work(
        &message,
        closure,
        &read.state,
        &read.work,
    ) {
        Ok(_) => anyhow::bail!("unprepared relation was silently hydrated"),
        Err(error) => error,
    };
    assert!(
        matches!(error.downcast_ref::<crate::runtime::resident_work::WorkReadError>(),
        Some(crate::runtime::resident_work::WorkReadError::TriggerWaitsNotPrepared(id)) if id == &message.id)
    );
    Ok(())
}

#[tokio::test]
async fn scheduler_prepares_current_source_after_positive_episode_recovery_invalidates_domains(
) -> Result<()> {
    use crate::{
        memory::episode_transition::{EpisodeMemoryOutcome, EpisodeMemoryPlan},
        runtime::scheduler_executor::{RunLoopPoll, SchedulerDecisionExecutor},
        runtime_db::episode_memory::{
            EpisodeMemoryFaultPoint, EpisodeMemoryInvocation, EpisodeMemoryKey,
            EpisodeMemoryTransitionCommand,
        },
    };
    let (_home, runtime) = fixture();
    let source = runtime
        .enqueue(MessageEnvelope::new(
            "scheduler-resident",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "current source after lost ACK".into(),
            },
        ))
        .await?;
    let RunLoopPoll::Message(scheduled) = SchedulerDecisionExecutor::new(&runtime).poll().await?
    else {
        anyhow::bail!("source was not claimed");
    };
    runtime
        .begin_interactive_turn(Some(&scheduled.message), None, None)
        .await?;
    let mut mutation = runtime.begin_mutation().await?;
    let state = mutation.state.clone();
    let binding = state
        .current_execution_binding
        .clone()
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
            message_id: source.id.clone(),
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
    // begin_mutation performs real receipt reconciliation. Episode recovery
    // deliberately publishes current State/Episodes without guessing Inputs/Work.
    let mut mutation = runtime.begin_mutation().await?;
    let root = mutation.capture_published_root()?;
    assert!(root.input_evidence.is_none() && root.work.is_none());
    assert_eq!(
        root.state.current_execution_binding.as_ref(),
        Some(&binding)
    );
    drop(root);
    // Build only a smaller policy through the real named actors. Ordinary
    // capture must report the missing closure window instead of hydrating it.
    let (_, record_limit, byte_limit) = mutation.context_input_evidence_policy()?;
    let narrow = crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
        windows: vec![
            crate::runtime_db::input_evidence_snapshot::InputWindowRequest {
                selector: InputWindowSelector::RecentBriefs,
                limit: 1,
            },
        ],
        mandatory_roots: Default::default(),
        interrupted_sources: Default::default(),
        selected_completion_brief_ids: Default::default(),
        record_limit,
        byte_limit,
    };
    mutation
        .prepare_owner_input_request(&narrow, context_config().recent_episode_candidates)
        .await?;
    mutation
        .prepare_owner_work_request(&crate::runtime::resident_work::WorkCaptureRequest::current())
        .await?;
    let before = mutation.capture_published_root()?;
    let alias_rows = || -> Result<Vec<(String, String, i64, Option<String>, String, String)>> {
        let connection = runtime.inner.runtime_db.connection()?;
        let mut query = connection.prepare("SELECT family,record_id,revision,content_digest,availability,fingerprint_json FROM input_evidence_current WHERE agent_id=?1 ORDER BY family,record_id")?;
        let rows = query
            .query_map([&source.agent_id], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    };
    let aliases_before = alias_rows()?;
    let error = mutation
        .capture_scheduler_read(
            &crate::runtime::resident_work::WorkCaptureRequest::current(),
            runtime.now(),
        )
        .err()
        .context("ordinary read silently expanded the Brief window")?;
    assert_eq!(
        error.downcast_ref::<crate::runtime::resident_input_evidence::InputEvidenceReadError>(),
        Some(&crate::runtime::resident_input_evidence::InputEvidenceReadError::WindowNotPrepared)
    );
    assert_eq!(mutation.capture_published_root()?.token, before.token);
    assert_eq!(alias_rows()?, aliases_before);
    drop(before);
    let read = mutation
        .prepare_scheduler_read(None, Default::default(), Default::default(), runtime.now())
        .await?;
    assert_eq!(
        read.state.current_execution_binding.as_ref(),
        Some(&binding)
    );
    let inputs = mutation.capture_input_evidence()?;
    let actual = crate::runtime::provider_recovery::captured_message(&inputs, &source.id)?
        .context("source was not explicitly prepared")?;
    assert_eq!(actual.id, source.id);
    assert!(actual.message_seq.is_some());
    assert_eq!(
        inputs.message_turn_keys.get(&source.id),
        Some(&Some(binding.turn_id.clone()))
    );
    drop(scheduled);
    Ok(())
}

#[tokio::test]
async fn activation_prepares_original_closure_brief_policy_without_ordinary_hydration() -> Result<()>
{
    let (_home, runtime) = fixture();
    let mutation = runtime.begin_mutation().await?;
    let before = mutation.capture_published_root()?;
    let now = runtime.now();
    let read = mutation.capture_scheduler_read(
        &crate::runtime::resident_work::WorkCaptureRequest::current(),
        now,
    )?;
    let legacy = SchedulerProjection::from_state_with_queue_len_at(
        runtime.storage(),
        &read.state,
        mutation.queue.len(),
        now,
    )?;
    assert_eq!(read.projection, legacy);
    assert_eq!(
        read.scheduling_posture,
        runtime.storage().agent_posture_projection(&read.state)?
    );
    assert!(mutation
        .capture_input_evidence()?
        .window(
            &InputWindowSelector::RecentBriefs,
            scheduler::SCHEDULER_BRIEF_LIMIT,
        )?
        .is_empty());
    assert_eq!(mutation.capture_published_root()?.token, before.token);
    Ok(())
}
