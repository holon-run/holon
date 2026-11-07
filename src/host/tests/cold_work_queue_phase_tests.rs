use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    runtime_db::cold_fixture::{ColdFixtureEvidence, ColdFixtureWriter},
    types::{
        BriefContentSource, BriefContentSourceRelation, ControlAction, QueueEntryRecord, TodoItem,
        TodoItemState, TurnOwner, TurnRecord, TurnTerminalKind, TurnTerminalSummary,
        TurnTriggerSummary, WaitConditionKind, WaitConditionRecord, WaitConditionStatus,
        WakeSource, WorkItemContinuationFrame, WorkItemPlanStatus, WorkItemRecord, WorkItemState,
    },
};

#[path = "cold_scheduler_work_phase_tests.rs"]
mod scheduler_work_tests;

#[derive(Default)]
struct ColdDispatchProvider {
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl AgentProvider for ColdDispatchProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        use crate::provider::{ConversationMessage, ModelBlock, ModelToolCallKind};
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if call % 2 == 1 {
            let last = request
                .conversation
                .iter()
                .rev()
                .find(|message| !matches!(message, ConversationMessage::UserBlocks(_)));
            assert!(
                matches!(last,
                    Some(ConversationMessage::UserToolResults(results))
                        if results.len() == 1
                            && results[0].tool_use_id == format!("cold-get-agent-{}", call - 1)
                            && !results[0].is_error),
                "cold dispatch must return one successful GetAgent result: {last:?}"
            );
        }
        let mut response = StubProvider::new("cold dispatch result")
            .complete_turn(request)
            .await?;
        if call % 2 == 0 {
            response.blocks = vec![ModelBlock::ToolUse {
                id: format!("cold-get-agent-{call}"),
                name: "GetAgent".into(),
                input: json!({"detail": "compact"}),
                kind: ModelToolCallKind::Function,
                provider_data: None,
            }];
        }
        Ok(response)
    }
}

#[tokio::test]
async fn typed_cold_fixture_requires_business_stop_and_restores_pending_preview() -> Result<()> {
    for (stop, pending, history, rounds) in [(false, 1, 0, 1), (true, 1, 0, 1), (true, 100, 64, 5)]
    {
        let (home, host) = empty_test_host();
        let agent_id = "cold-typed-bootstrap";
        host.create_named_agent(agent_id, None).await?;
        if stop {
            host.control_public_agent(agent_id, ControlAction::Stop)
                .await
                .map_err(anyhow::Error::new)?;
        }
        host.shutdown().await?;
        let writer = ColdFixtureWriter::new(host.runtime_db(), agent_id);
        if !stop {
            assert!(writer
                .err()
                .context("service shutdown must not imply business Stop")?
                .to_string()
                .contains("stopped, drained"));
            continue;
        }
        let writer = writer?;
        let mut work = WorkItemRecord::new(agent_id, "cold typed restoration", WorkItemState::Open);
        work.plan_status = WorkItemPlanStatus::Ready;
        writer.insert_work(&work)?;
        let mut message = MessageEnvelope::new(
            agent_id,
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: Some("synthetic".into()),
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "cold typed pending root".into(),
            },
        );
        message.work_item_id = Some(work.id.clone());
        for index in 0..history {
            let mut historical = message.clone();
            historical.id = format!("history-{index}");
            historical.message_seq = Some(index as u64 + 1);
            let turn_id = format!("turn-{index}");
            let transcript_id = format!("transcript-{index}");
            historical.turn_id = Some(turn_id.clone());
            writer.insert(ColdFixtureEvidence::Message(&historical))?;
            let mut brief = BriefRecord::new(
                agent_id,
                BriefKind::Result,
                format!("historical observation {index}"),
                Some(historical.id.clone()),
                None,
            );
            brief.turn_id = Some(turn_id.clone());
            brief.turn_index = Some(index as u64 + 1);
            brief.work_item_id = Some(work.id.clone());
            brief.content_source = BriefContentSource::TranscriptEntry {
                entry_id: transcript_id.clone(),
                relation: BriefContentSourceRelation::Finalizes,
            };
            writer.insert(ColdFixtureEvidence::Brief(&brief))?;
            let mut turn = TurnRecord::new(agent_id, turn_id.clone(), index as u64 + 1);
            turn.current_work_item_id = Some(work.id.clone());
            turn.owner = Some(TurnOwner::WorkItem {
                work_item_id: work.id.clone(),
            });
            turn.trigger = Some(TurnTriggerSummary::from_message(&historical));
            turn.input_message_ids = vec![historical.id.clone()];
            turn.produced_brief_ids = vec![brief.id.clone()];
            turn.terminal = Some(TurnTerminalSummary {
                kind: TurnTerminalKind::Completed,
                reason: None,
                no_brief_reason: None,
                completed_at: historical.created_at,
                duration_ms: 1,
            });
            writer.insert(ColdFixtureEvidence::Turn(&turn))?;
            let mut transcript = TranscriptEntry::new(
                agent_id,
                TranscriptEntryKind::AssistantRound,
                Some(1),
                Some(historical.id),
                json!({"turn_id": turn_id, "blocks": [{"type": "text", "text": brief.text}]}),
            );
            transcript.id = transcript_id;
            writer.insert(ColdFixtureEvidence::Transcript(&transcript))?;
        }
        let mut pending_ids = Vec::new();
        for index in 0..pending {
            message.id = format!("pending-{index}");
            message.message_seq = Some((history + index + 1) as u64);
            if pending == 100 {
                message.body = MessageBody::Text {
                    text: "p".repeat(16 * 1024),
                };
            }
            writer.insert_pending(
                &message,
                &QueueEntryRecord {
                    message_id: message.id.clone(),
                    agent_id: agent_id.into(),
                    priority: message.priority.clone(),
                    status: QueueEntryStatus::Queued,
                    created_at: message.created_at,
                    updated_at: message.created_at,
                },
            )?;
            pending_ids.push(message.id.clone());
        }
        let mut state = host
            .runtime_db()
            .agent_states()
            .latest(agent_id)?
            .context("stopped setup State")?;
        state.current_work_item_id = Some(work.id.clone());
        state.pending = pending;
        state.total_message_count = history + pending;
        state.turn_index = history as u64;
        writer.finish_state(&state)?;
        let mut config = AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
        config.user_home_dir = Some(home.path().to_path_buf());
        let provider = Arc::new(ColdDispatchProvider::default());
        let cold = RuntimeHost::new_with_provider(config, provider.clone())?;
        let runtime = cold.get_or_create_agent(agent_id).await?;
        let restored = runtime.agent_state().await?;
        assert_eq!(restored.status, AgentStatus::Stopped);
        assert_eq!(restored.pending, pending);
        assert_eq!(
            restored.current_work_item_id.as_deref(),
            Some(work.id.as_str())
        );
        let trigger_ids = runtime
            .latest_external_triggers()
            .await?
            .into_iter()
            .map(|record| record.external_trigger_id.clone())
            .collect::<Vec<_>>();
        for _ in 0..rounds {
            let prompt = runtime
                .preview_prompt(
                    "cold typed preview root".into(),
                    AuthorityClass::OperatorInstruction,
                )
                .await?;
            assert!(prompt
                .rendered_context_attachment
                .contains("cold typed preview root"));
            assert_eq!(runtime.agent_state().await?.pending, pending);
            assert_eq!(runtime.agent_state().await?.status, AgentStatus::Stopped);
            for id in &pending_ids {
                assert_eq!(
                    cold.runtime_db()
                        .queue_entries()
                        .latest(id)?
                        .context("preview must preserve pending Queue")?
                        .status,
                    QueueEntryStatus::Queued
                );
            }
        }
        assert_eq!(
            runtime
                .latest_external_triggers()
                .await?
                .into_iter()
                .map(|record| record.external_trigger_id.clone())
                .collect::<Vec<_>>(),
            trigger_ids,
            "stopped preview must not create an external ingress"
        );
        if pending == 100 {
            let mut discarded = message.clone();
            discarded.id = "cold-stopped-enqueue".into();
            discarded.work_item_id = None;
            runtime.enqueue(discarded).await?;
            assert_eq!(runtime.agent_state().await?.pending, pending);
            assert_eq!(
                cold.runtime_db()
                    .queue_entries()
                    .latest("cold-stopped-enqueue")?
                    .context("stopped admission must publish Queue disposition")?
                    .status,
                QueueEntryStatus::Aborted
            );
            drop(runtime);
            let running = cold
                .control_public_agent(agent_id, ControlAction::Start)
                .await
                .map_err(anyhow::Error::new)?;
            // Verify one resumed tool turn, not a fixed dispatch throughput.
            // The timeout only guards against a stuck functional test.
            let resumed_id = pending_ids
                .first()
                .context("original cold pending message")?;
            let settled = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let entry = cold
                        .runtime_db()
                        .queue_entries()
                        .latest(resumed_id)?
                        .context("original cold Queue entry")?;
                    if entry.status == QueueEntryStatus::Processed {
                        return Ok::<_, anyhow::Error>(());
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await;
            let state = running.agent_state().await?;
            let queue_statuses = pending_ids
                .iter()
                .take(2)
                .map(|id| {
                    Ok((
                        id,
                        cold.runtime_db()
                            .queue_entries()
                            .latest(id)?
                            .map(|entry| entry.status),
                    ))
                })
                .collect::<Result<Vec<_>>>()?;
            let errors = cold
                .runtime_db()
                .audit_events()
                .recent(Some(agent_id), 128)?
                .into_iter()
                .filter(|event| event.kind == "runtime_error")
                .collect::<Vec<_>>();
            let audit_kinds = cold
                .runtime_db()
                .audit_events()
                .recent(Some(agent_id), 128)?
                .into_iter()
                .map(|event| (event.id, event.kind))
                .collect::<Vec<_>>();
            let execution_protocol = cold
                .runtime_db()
                .transitions()
                .load_execution_protocol_state_if_initialized(agent_id)?;
            cold.control_public_agent(agent_id, ControlAction::Stop)
                .await
                .map_err(anyhow::Error::new)?;
            cold.shutdown().await?;
            assert!(
                settled.is_ok(),
                "cold resumed GetAgent/Result dispatch did not commit QueueTerminal: calls={}, queue={queue_statuses:?}, errors={errors:?}, status={:?}, pending={}, turn={:?}, binding={:?}, audits={audit_kinds:?}, execution_protocol={execution_protocol:?}",
                provider.calls.load(std::sync::atomic::Ordering::SeqCst),
                state.status,
                state.pending,
                state.current_turn_id,
                state.current_execution_binding,
            );
            settled??;
            assert!(provider.calls.load(std::sync::atomic::Ordering::SeqCst) >= 2);
        } else {
            cold.shutdown().await?;
        }
    }
    Ok(())
}

fn fixture(
    populated: bool,
) -> Result<(
    tempfile::TempDir,
    RuntimeHost,
    AgentIdentityRecord,
    AgentState,
    AppStorage,
)> {
    let (home, host) = test_host();
    let identity = host.ensure_default_agent_identity()?;
    let mut state = AgentState::new(&identity.agent_id);
    state.status = AgentStatus::Stopped;
    host.runtime_db()
        .agent_states()
        .initialize_cold_for_fixture(&state, identity.incarnation)?;
    if populated {
        let mut work = WorkItemRecord::new(
            &identity.agent_id,
            "cold queue 汉🦀".repeat(3),
            WorkItemState::Open,
        );
        work.id = "work_cold_queue".into();
        work.plan_status = WorkItemPlanStatus::Ready;
        work.todo_list = vec![TodoItem {
            text: "nested queue 汉🦀".repeat(3),
            state: TodoItemState::InProgress,
        }];
        assert!(host
            .runtime_db()
            .work_items()
            .insert_new_qualified_for_fixture(&work, identity.incarnation)?);
        // The canonical focus trigger requires its Work row to exist first.
        state.current_work_item_id = Some(work.id.clone());
        host.runtime_db()
            .agent_states()
            .upsert_for_fixture(&state)?;
    }
    let storage = host
        .agent_storage_read_only(&identity.agent_id)
        .map_err(|error| anyhow!("{error:?}"))?;
    Ok((home, host, identity, state, storage))
}

fn scope(identity: &AgentIdentityRecord) -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: identity.agent_id.clone(),
        identity_incarnation: identity.incarnation,
    }
}

fn queue_wire<W: serde::Serialize>(
    queue: &crate::work_item_scheduling::WorkQueueReadModel<W>,
) -> Result<serde_json::Value> {
    Ok(serde_json::to_value((
        &queue.current,
        &queue.queued_blocked,
        &queue.items,
        &queue.current_runnable,
        &queue.triggered_blocked,
        &queue.queued_runnable,
        &queue.yielded,
        &queue.waiting_for_operator,
        &queue.blocked,
        &queue.completing,
        &queue.completed_recent,
    ))?)
}

#[tokio::test]
async fn cold_state_two_hundred_open_work_items_fit_default_material_budget() -> Result<()> {
    let (home, host, identity, state, _storage) = fixture(false)?;
    let mut expected = BTreeMap::new();
    for index in 0..200 {
        let mut work = WorkItemRecord::new(
            &identity.agent_id,
            format!("HTTP benchmark work item {index}"),
            WorkItemState::Open,
        );
        // Real create_work_item also retains the empty plan's artifact.
        work.plan_artifact = Some(crate::types::WorkItemPlanArtifact {
            owner_agent_id: identity.agent_id.clone(),
            workspace_id: work.workspace_id.clone(),
            workspace_alias: Some("agent_home".into()),
            relative_path: format!("work-items/{}/plan.md", work.id).into(),
            path: home.path().join(format!("work-items/{}/plan.md", work.id)),
            hash: "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into(),
            bytes: 0,
            updated_at: work.updated_at,
            preview: String::new(),
            preview_complete: true,
        });
        assert!(host
            .runtime_db()
            .work_items()
            .insert_new_qualified_for_fixture(&work, identity.incarnation)?);
        expected.insert(work.id.clone(), serde_json::to_value(&work)?);
    }
    let projection = host
        .operator_agent_state_projection(&identity.agent_id, 0, 0)
        .await
        .map_err(|error| anyhow!("{error:?}"))?;
    assert_eq!(projection.source, AgentStateProjectionSource::Storage);
    assert_eq!(projection.agent.agent, state);
    assert_eq!(projection.agent.work_queue.items.len(), expected.len());
    for item in &projection.agent.work_queue.items {
        assert_eq!(serde_json::to_value(&item.work_item)?, expected[&item.id],);
    }
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    assert!(host.inner.input_evidence_budget.report()?.observed_bytes > 0);
    drop(projection);
    assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn cold_state_scoped_real_created_work_items_fit_default_material_budget() -> Result<()> {
    scoped_real_created_work_items_with_briefs(0).await
}

#[tokio::test]
async fn cold_state_scoped_real_work_with_recent_briefs_fit_default_material_budget() -> Result<()>
{
    scoped_real_created_work_items_with_briefs(64).await
}

async fn scoped_real_created_work_items_with_briefs(recent_briefs: usize) -> Result<()> {
    let (home, host) = test_host();
    let identity = host.ensure_default_agent_identity()?;
    let runtime = host.get_or_create_agent(&identity.agent_id).await?;
    let mut expected = BTreeMap::new();
    for index in 0..200 {
        let work = runtime
            .create_work_item(
                format!("HTTP benchmark work item {index}"),
                None,
                None,
                Vec::new(),
            )
            .await?;
        expected.insert(work.id.clone(), serde_json::to_value(&work)?);
    }
    for index in 0..recent_briefs {
        host.runtime_db()
            .evidence()
            .append_brief(&BriefRecord::new(
                &identity.agent_id,
                BriefKind::Result,
                format!("recent scheduler observation {index} {{,:}} \"中文\""),
                None,
                None,
            ))?;
    }
    host.shutdown().await?;
    drop(runtime);
    drop(host);
    let mut config = AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
    config.user_home_dir = Some(home.path().to_path_buf());
    let cold = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done")))?;
    let db = cold
        .runtime_db()
        .with_read_scope(crate::runtime_db::RuntimeDbReadScope::new(
            std::time::Instant::now() + Duration::from_secs(30),
        ));
    let reader = cold.clone();
    let agent_id = identity.agent_id;
    let handle = tokio::runtime::Handle::current();
    let projection = tokio::task::spawn_blocking(move || {
        reader.operator_agent_state_projection_scoped(&agent_id, 0, 0, &db, &handle)
    })
    .await?
    .map_err(|error| anyhow!("{error:?}"))?;
    assert_eq!(projection.source, AgentStateProjectionSource::Storage);
    assert_eq!(projection.agent.work_queue.items.len(), expected.len());
    for item in &projection.agent.work_queue.items {
        assert_eq!(serde_json::to_value(&item.work_item)?, expected[&item.id]);
    }
    assert!(cold.inner.runtimes.read().await.agents.is_empty());
    assert!(cold.inner.input_evidence_budget.report()?.observed_bytes > 0);
    drop(projection);
    assert_eq!(cold.inner.input_evidence_budget.report()?.observed_bytes, 0);
    cold.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn cold_state_shared_storage_queue_matches_owned_groups_and_last_holder() -> Result<()> {
    let (_home, host, identity, state, storage) = fixture(true)?;
    let timestamp = "2026-10-01T00:00:00Z".parse()?;
    for id in ["queued_b", "task_wait", "completed", "yielded", "queued_a"] {
        let mut work = WorkItemRecord::new(
            &identity.agent_id,
            "shared queue 汉🦀 \"{}[],:\\",
            if id == "completed" {
                WorkItemState::Completed
            } else {
                WorkItemState::Open
            },
        );
        work.id = id.into();
        work.plan_status = WorkItemPlanStatus::Ready;
        work.created_at = timestamp;
        work.updated_at = timestamp;
        work.todo_list = vec![TodoItem {
            text: "nested retained Todo 汉🦀".into(),
            state: TodoItemState::InProgress,
        }];
        assert!(host
            .runtime_db()
            .work_items()
            .insert_new_qualified_for_fixture(&work, identity.incarnation)?);
    }
    host.runtime_db()
        .wait_conditions()
        .upsert(&WaitConditionRecord {
            id: "wait_task".into(),
            agent_id: identity.agent_id.clone(),
            work_item_id: Some("task_wait".into()),
            status: WaitConditionStatus::Active,
            kind: WaitConditionKind::Task,
            source: Some("test".into()),
            subject_ref: Some("task_fixture".into()),
            waiting_for: "task result 汉🦀".into(),
            wake_sources: vec![WakeSource::TaskResult {
                task_id: "task_fixture".into(),
            }],
            continuation: None,
            created_at: timestamp,
            updated_at: timestamp,
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
            turn_id: None,
            trigger_message_id: None,
            triggered_at: None,
        })?;
    host.runtime_db().work_item_continuations().upsert(
        &WorkItemContinuationFrame::new_on_completed(
            &identity.agent_id,
            "yielded",
            "work_cold_queue",
            None,
        ),
    )?;
    let owned = storage.work_queue_with_observed_scheduling(&state, &[])?;
    let expected = queue_wire(&owned)?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope(&identity))?;
    // Exercise the production storage adapter, not owned.into_state_queue.
    // It must enter the caller's acquisition even without an ambient entry.
    let queue = storage.state_work_queue_with_observed_scheduling(&state, &[], &phase)?;
    phase.finish()?;
    assert_eq!(queue_wire(&queue)?, expected);
    assert_eq!(queue.items.len(), 6);
    assert_eq!(queue.current.as_ref().unwrap().id, "work_cold_queue");
    assert_eq!(queue.yielded.len(), 1);
    assert_eq!(queue.yielded[0].work_item.id, "yielded");
    assert_eq!(queue.completed_recent.len(), 1);
    let waiting = queue
        .items
        .iter()
        .find(|item| item.id == "task_wait")
        .unwrap();
    assert_eq!(waiting.active_wait_conditions.len(), 1);
    let loan = waiting.work_item.clone();
    let scheduler =
        crate::runtime::SchedulerProjection::from_state_with_shared_scheduling_observation(
            &storage, &state, &queue, 0, &phase,
        )?;
    let expected_runnable = serde_json::to_value(
        queue
            .queued_runnable
            .iter()
            .map(|item| &item.work_item)
            .collect::<Vec<_>>(),
    )?;
    assert!(!scheduler.queued_runnable_work_items.is_empty());
    assert_eq!(
        serde_json::to_value(&scheduler.queued_runnable_work_items)?,
        expected_runnable,
    );
    assert_eq!(
        scheduler.queued_runnable_work_items.len(),
        queue.queued_runnable.len(),
    );
    for (retained, source) in scheduler
        .queued_runnable_work_items
        .iter()
        .zip(&queue.queued_runnable)
    {
        assert!(std::ptr::eq(retained, &*source.work_item));
    }
    drop(phase);
    let held = ledger.report()?.observed_bytes;
    assert!(held > 0);
    drop(queue);
    assert_eq!(ledger.report()?.observed_bytes, held);
    let last = loan.clone();
    drop(loan);
    assert_eq!(last.id, "task_wait");
    assert_eq!(ledger.report()?.observed_bytes, held);
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, held);
    assert_eq!(
        serde_json::to_value(&scheduler.queued_runnable_work_items)?,
        expected_runnable,
    );
    drop(scheduler);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn cold_state_direct_storage_exact_and_short_axes_without_ambient_material() -> Result<()> {
    for populated in [false, true] {
        let (_home, host, identity, state, storage) = fixture(populated)?;
        assert_direct_storage_boundaries(&storage, &identity, &state, &[])?;
        host.shutdown().await?;
    }
    Ok(())
}

fn assert_direct_storage_boundaries(
    storage: &AppStorage,
    identity: &AgentIdentityRecord,
    state: &AgentState,
    triggers: &[crate::types::ExternalTriggerRecord],
) -> Result<()> {
    let expected = queue_wire(&storage.work_queue_with_observed_scheduling(state, triggers)?)?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let measure = ResourceReadMaterial::new(&ledger, &scope(identity))?;
    // No ambient entry: production must charge the supplied phase itself.
    let queue = storage.state_work_queue_with_observed_scheduling(state, triggers, &measure)?;
    let total = measure.finish()?;
    assert_eq!(queue_wire(&queue)?, expected);
    drop((queue, measure));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    for (bytes, records, succeeds) in [
        (total.0, total.1, true),
        (total.0 - 1, total.1, false),
        (total.0, total.1 - 1, false),
    ] {
        let phase =
            ResourceReadMaterial::with_test_limits(&ledger, &scope(identity), bytes, records)?;
        let result = storage.state_work_queue_with_observed_scheduling(state, triggers, &phase);
        if succeeds {
            let queue = result?;
            assert_eq!(queue_wire(&queue)?, expected);
            assert_eq!(phase.finish()?, total);
        } else {
            let error = result.unwrap_err();
            assert_eq!(
                crate::runtime_error::describe_runtime_error(&error).code,
                "context_resource_exhausted",
            );
            assert!(phase.finish().is_err());
            assert!(storage
                .state_work_queue_with_observed_scheduling(state, triggers, &phase)
                .is_err());
        }
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[tokio::test]
async fn cold_state_direct_storage_preserves_all_groups_trigger_order_and_limits() -> Result<()> {
    use crate::types::{
        CallbackDeliveryMode, ExternalTriggerRecord, ExternalTriggerScope, ExternalTriggerStatus,
    };
    let (_home, host, identity, mut state, storage) = fixture(true)?;
    let timestamp: chrono::DateTime<Utc> = "2026-10-01T00:00:00Z".parse()?;
    let mut triggers = Vec::new();
    for group in [
        "queued",
        "blocked",
        "operator",
        "completing",
        "completed",
        "yielded",
        "triggered",
    ] {
        for index in (0..7).rev() {
            let mut work = WorkItemRecord::new(
                &identity.agent_id,
                format!("{group} retained 汉🦀"),
                match group {
                    "completing" => WorkItemState::Completing,
                    "completed" => WorkItemState::Completed,
                    _ => WorkItemState::Open,
                },
            );
            work.id = format!("{group}_{index:02}");
            work.created_at = timestamp;
            work.updated_at = timestamp;
            work.plan_status = if group == "operator" {
                WorkItemPlanStatus::NeedsInput
            } else {
                WorkItemPlanStatus::Ready
            };
            if matches!(group, "blocked" | "triggered") {
                work.blocked_by = Some("retained blocker".into());
            }
            assert!(host
                .runtime_db()
                .work_items()
                .insert_new_qualified_for_fixture(&work, identity.incarnation)?);
            if group == "yielded" {
                host.runtime_db().work_item_continuations().upsert(
                    &WorkItemContinuationFrame::new_on_completed(
                        &identity.agent_id,
                        &work.id,
                        "work_cold_queue",
                        None,
                    ),
                )?;
            }
            if group == "triggered" {
                let trigger_id = format!("trigger_{index:02}");
                host.runtime_db()
                    .wait_conditions()
                    .upsert(&WaitConditionRecord {
                        id: format!("wait_{index:02}"),
                        agent_id: identity.agent_id.clone(),
                        work_item_id: Some(work.id.clone()),
                        status: WaitConditionStatus::Active,
                        kind: WaitConditionKind::External,
                        source: Some("test".into()),
                        subject_ref: None,
                        waiting_for: "external delivery".into(),
                        wake_sources: vec![WakeSource::ExternalIngress {
                            external_trigger_id: Some(trigger_id.clone()),
                        }],
                        continuation: None,
                        created_at: timestamp,
                        updated_at: timestamp,
                        expires_at: None,
                        resolved_at: None,
                        cancelled_at: None,
                        turn_id: None,
                        trigger_message_id: None,
                        triggered_at: None,
                    })?;
                triggers.push(ExternalTriggerRecord {
                    external_trigger_id: trigger_id,
                    target_agent_id: identity.agent_id.clone(),
                    scope: ExternalTriggerScope::Agent,
                    delivery_mode: CallbackDeliveryMode::WakeHint,
                    token: None,
                    token_hash: "fixture-only".into(),
                    status: ExternalTriggerStatus::Active,
                    created_at: timestamp,
                    revoked_at: None,
                    last_delivered_at: Some(timestamp + chrono::Duration::seconds(index % 3)),
                    delivery_count: 1,
                });
            }
        }
    }
    // Current focus need not be CurrentRunnable.
    state.current_work_item_id = Some("operator_00".into());
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope(&identity))?;
    let queue = storage.state_work_queue_with_observed_scheduling(&state, &triggers, &phase)?;
    assert_eq!(queue.items.len(), 50);
    assert_eq!(queue.current.as_ref().unwrap().id, "operator_00");
    assert!(queue.current_runnable.is_none());
    let ids = |rows: &[crate::work_item_scheduling::WorkItemSchedulingProjection<
        crate::runtime::resident_work::StateWorkRecord,
    >]| rows.iter().map(|row| row.id.clone()).collect::<Vec<_>>();
    assert_eq!(
        ids(&queue.triggered_blocked),
        ["triggered_02", "triggered_05", "triggered_01"],
    );
    for (group, rows, limit) in [
        ("queued", &queue.queued_runnable, 5),
        ("yielded", &queue.yielded, 5),
        ("operator", &queue.waiting_for_operator, 3),
        ("blocked", &queue.blocked, 3),
        ("completing", &queue.completing, 3),
        ("completed", &queue.completed_recent, 3),
    ] {
        assert_eq!(
            ids(rows),
            (0..limit)
                .map(|index| format!("{group}_{index:02}"))
                .collect::<Vec<_>>(),
        );
    }
    assert_eq!(
        queue_wire(&queue)?,
        queue_wire(&storage.work_queue_with_observed_scheduling(&state, &triggers)?)?,
    );
    drop((queue, phase));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert_direct_storage_boundaries(&storage, &identity, &state, &triggers)?;
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn cold_state_queue_conversion_keeps_original_allowance_through_last_record() -> Result<()> {
    let (_home, host, identity, state, storage) = fixture(true)?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let measure = ResourceReadMaterial::new(&ledger, &scope(&identity))?;
    let owned = measure.read(|| storage.work_queue_with_observed_scheduling(&state, &[]))?;
    let expected = queue_wire(&owned)?;
    let source_usage = measure.finish()?;
    // This call deliberately has no ambient TLS entry: the adapter must enter
    // the caller's acquisition, rather than silently skipping its charges.
    let queue = owned.into_state_queue(&measure)?;
    let total = measure.finish()?;
    assert!(total.0 > source_usage.0 && total.1 > source_usage.1);
    assert_eq!(queue_wire(&queue)?, expected);
    drop(measure);
    let held = ledger.report()?.observed_bytes;
    assert!(held > 0);
    let loan = queue.items[0].work_item.clone();
    drop(queue);
    assert_eq!(ledger.report()?.observed_bytes, held);
    let last = loan.clone();
    drop(loan);
    assert_eq!(ledger.report()?.observed_bytes, held);
    assert_eq!(last.id, "work_cold_queue");
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, 0);

    for (bytes, records, succeeds) in [
        (total.0, total.1, true),
        (total.0 - 1, total.1, false),
        (total.0, total.1 - 1, false),
    ] {
        let phase =
            ResourceReadMaterial::with_test_limits(&ledger, &scope(&identity), bytes, records)?;
        let owned = phase.read(|| storage.work_queue_with_observed_scheduling(&state, &[]))?;
        let result = owned.into_state_queue(&phase);
        if succeeds {
            let queue = result?;
            assert_eq!(queue_wire(&queue)?, expected);
            assert_eq!(phase.finish()?, total);
        } else {
            let error = result.unwrap_err();
            assert_eq!(
                crate::runtime_error::describe_runtime_error(&error).code,
                "context_resource_exhausted",
            );
            assert!(phase.finish().is_err());
            assert!(crate::work_item_scheduling::WorkQueueReadModel::default()
                .into_state_queue(&phase)
                .is_err());
        }
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn cold_queue_phase_reuses_original_allowance_and_last_holder() -> Result<()> {
    for populated in [false, true] {
        let (_home, host, identity, state, storage) = fixture(populated)?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        // The original adapter's already-tested gates are the cost oracle.
        let oracle_phase = ResourceReadMaterial::new(&ledger, &scope(&identity))?;
        let expected = {
            let _entry = oracle_phase.enter();
            storage.work_queue_with_observed_scheduling(&state, &[])?
        };
        let expected_usage = oracle_phase.finish()?;
        let expected_wire = queue_wire(&expected)?;
        drop((expected, oracle_phase));
        assert_eq!(ledger.report()?.observed_bytes, 0);

        let read = host.acquire_cold_scheduling_query(&identity, 0).await?;
        let host_held = host.inner.input_evidence_budget.report()?.observed_bytes;
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope(&identity),
            expected_usage.0,
            expected_usage.1,
        )?;
        let (queue, material) = {
            let _entry = phase.enter();
            host.cold_work_queue_with_observed_scheduling(
                &read.identity,
                &storage,
                &state,
                &read.triggers,
                read.material,
            )?
        };
        assert_eq!(queue_wire(&queue)?, expected_wire);
        assert_eq!(phase.finish()?, expected_usage);
        assert_eq!(ledger.report()?.observed_bytes, expected_usage.0);
        // Reusing a foreign test ledger must not allocate another Host phase.
        assert_eq!(
            host.inner.input_evidence_budget.report()?.observed_bytes,
            host_held
        );
        // Release the acquisition caller before testing the response's holder.
        drop((read.acquisition, read.timers, read.triggers));
        drop(phase);
        let first = Arc::new((queue, material));
        let last = first.clone();
        drop(first);
        assert_eq!(queue_wire(&last.0)?, expected_wire);
        assert_eq!(ledger.report()?.observed_bytes, expected_usage.0);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
        drop(read.reservations);
        assert!(host.inner.runtimes.read().await.agents.is_empty());
        host.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn cold_queue_phase_short_limits_latch_and_do_not_reset_on_retry() -> Result<()> {
    let (_home, host, identity, state, storage) = fixture(true)?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let measure = ResourceReadMaterial::new(&ledger, &scope(&identity))?;
    {
        let _entry = measure.enter();
        storage.work_queue_with_observed_scheduling(&state, &[])?;
    }
    let total = measure.finish()?;
    drop(measure);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    for bytes in [true, false] {
        let read = host.acquire_cold_scheduling_query(&identity, 0).await?;
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope(&identity),
            if bytes { total.0 - 1 } else { total.0 },
            if bytes { total.1 } else { total.1 - 1 },
        )?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            let error = host
                .cold_work_queue_with_observed_scheduling(
                    &read.identity,
                    &storage,
                    &state,
                    &read.triggers,
                    read.material,
                )
                .err()
                .context("one-short queue allowance must reject")?;
            assert!(
                error.downcast_ref::<std::io::Error>().is_some(),
                "{error:#}"
            );
            assert!(phase.finish().is_err());
        }
        drop((read.acquisition, read.timers, read.triggers));
        assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
        drop(read.reservations);
        let prefix = ledger.report()?.observed_bytes;
        // Obtain original Scheduling material outside the failed queue entry.
        let retry = host.acquire_cold_scheduling_query(&identity, 0).await?;
        {
            let _entry = phase.enter();
            let error = host
                .cold_work_queue_with_observed_scheduling(
                    &retry.identity,
                    &storage,
                    &state,
                    &retry.triggers,
                    retry.material,
                )
                .err()
                .context("failed inherited phase must stay failed")?;
            assert_eq!(
                error
                    .downcast_ref::<crate::runtime_error::RuntimeError>()
                    .context("retry must report the original resource latch")?
                    .descriptor()
                    .code,
                "context_resource_exhausted",
            );
            assert_eq!(ledger.report()?.observed_bytes, prefix);
        }
        drop((
            retry.acquisition,
            retry.timers,
            retry.triggers,
            retry.reservations,
            phase,
        ));
        assert_eq!(ledger.report()?.observed_bytes, prefix);
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
    }
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn cold_queue_phase_decode_error_preserves_original_error_and_releases_material() -> Result<()>
{
    let (_home, host, identity, state, storage) = fixture(true)?;
    let read = host.acquire_cold_scheduling_query(&identity, 0).await?;
    host.runtime_db().transaction(|tx| {
        tx.execute(
            "UPDATE work_items SET payload_json=json_set(payload_json,'$.created_at','invalid timestamp')
             WHERE work_item_id='work_cold_queue'",
            [],
        )?;
        Ok(())
    })?;
    let oracle = storage
        .work_queue_with_observed_scheduling(&state, &[])
        .err()
        .context("invalid Work timestamp must fail")?;
    let error = host
        .cold_work_queue_with_observed_scheduling(
            &read.identity,
            &storage,
            &state,
            &read.triggers,
            read.material,
        )
        .err()
        .context("queue phase must not hide the original decode error")?;
    assert_eq!(format!("{error:#}"), format!("{oracle:#}"));
    assert_eq!(
        error
            .downcast_ref::<std::io::Error>()
            .map(std::io::Error::kind),
        oracle
            .downcast_ref::<std::io::Error>()
            .map(std::io::Error::kind),
    );
    drop((read.acquisition, read.timers, read.triggers));
    assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
    drop(read.reservations);
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn cold_queue_phase_three_real_callers_hold_standalone_material() -> Result<()> {
    for populated in [false, true] {
        let (_home, host, identity, state, storage) = fixture(populated)?;
        let expected = queue_wire(&storage.work_queue_with_observed_scheduling(&state, &[])?)?;
        let acquisition = host.acquire_cold_scheduling_query(&identity, 0).await?;
        let acquisition_bytes = host.inner.input_evidence_budget.report()?.observed_bytes;
        drop(acquisition);
        assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
        for entry in 0..3 {
            let holder: Box<dyn std::any::Any> = match entry {
                0 => {
                    let read = host
                        .local_agent_summary(&identity.agent_id)
                        .await
                        .map_err(|error| anyhow!("{error:?}"))?;
                    assert_eq!(read.agent, state);
                    Box::new(read)
                }
                1 => {
                    let read = host
                        .local_agent_lightweight_projection(&identity.agent_id)
                        .await
                        .map_err(|error| anyhow!("{error:?}"))?;
                    assert_eq!(read.agent, state);
                    assert_eq!(queue_wire(&read.work_queue)?, expected);
                    Box::new(read)
                }
                _ => {
                    let read = host
                        .operator_agent_state_projection(&identity.agent_id, 0, 0)
                        .await
                        .map_err(|error| anyhow!("{error:?}"))?;
                    assert_eq!(read.source, AgentStateProjectionSource::Storage);
                    assert_eq!(read.agent.agent, state);
                    assert_eq!(queue_wire(&read.agent.work_queue)?, expected);
                    Box::new(read)
                }
            };
            let held = host.inner.input_evidence_budget.report()?.observed_bytes;
            assert!(
                held > acquisition_bytes,
                "{entry}: queue material must outlive acquisition"
            );
            let first = Arc::new(holder);
            let last = first.clone();
            drop(first);
            assert_eq!(
                host.inner.input_evidence_budget.report()?.observed_bytes,
                held
            );
            assert!(host.inner.runtimes.read().await.agents.is_empty());
            drop(last);
            assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
        }
        host.shutdown().await?;
    }
    Ok(())
}
