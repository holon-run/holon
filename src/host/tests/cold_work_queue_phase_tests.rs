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
        TurnTriggerSummary, WorkItemPlanStatus, WorkItemRecord, WorkItemState,
    },
};

#[path = "cold_scheduler_work_phase_tests.rs"]
mod scheduler_work_tests;

const COLD_DISPATCH_ROUNDS: usize = 24;

#[derive(Default)]
struct ColdDispatchProvider {
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl AgentProvider for ColdDispatchProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        use crate::provider::{ConversationMessage, ModelBlock, ModelToolCallKind};
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if call >= COLD_DISPATCH_ROUNDS * 2 {
            return std::future::pending().await;
        }
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
            let settled = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let queue = cold
                        .runtime_db()
                        .queue_entries()
                        .recent(Some(agent_id), pending + 1)?;
                    let entries = pending_ids
                        .iter()
                        .take(COLD_DISPATCH_ROUNDS)
                        .map(|id| {
                            queue
                                .iter()
                                .find(|entry| &entry.message_id == id)
                                .context("original cold Queue entry")
                        })
                        .collect::<Result<Vec<_>>>()?;
                    if entries
                        .iter()
                        .all(|entry| entry.status == QueueEntryStatus::Processed)
                    {
                        return Ok::<_, anyhow::Error>(());
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await;
            let state = running.agent_state().await?;
            let queue_statuses = pending_ids
                .iter()
                .take(COLD_DISPATCH_ROUNDS + 2)
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
                "cold consecutive GetAgent/Result dispatches did not commit QueueTerminal: calls={}, queue={queue_statuses:?}, errors={errors:?}, status={:?}, pending={}, turn={:?}, binding={:?}, audits={audit_kinds:?}, execution_protocol={execution_protocol:?}",
                provider.calls.load(std::sync::atomic::Ordering::SeqCst),
                state.status,
                state.pending,
                state.current_turn_id,
                state.current_execution_binding,
            );
            settled??;
            assert!(
                provider.calls.load(std::sync::atomic::Ordering::SeqCst)
                    >= COLD_DISPATCH_ROUNDS * 2
            );
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

fn queue_wire(
    queue: &crate::work_item_scheduling::WorkQueueReadModel,
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
