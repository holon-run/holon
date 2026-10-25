// Included in host::tests to reuse real host/provider fixture utilities.
struct ReplyGateProvider {
    first: AtomicBool,
    started: Notify,
    release: Notify,
    recovery_started: Notify,
    recovery_release: Notify,
    answer: &'static str,
    wait: bool,
    recover: bool,
}

#[async_trait]
impl AgentProvider for Arc<ReplyGateProvider> {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        if !self.first.swap(true, Ordering::SeqCst) {
            self.started.notify_one();
            self.release.notified().await;
            if self.recover {
                return crate::runtime::tests::support::DeferredFallbackProvider
                    .complete_turn(request)
                    .await;
            }
            if self.wait {
                return Ok(ProviderTurnResponse {
                    blocks: vec![crate::provider::ModelBlock::ToolUse {
                        id: "reply-fixture-wait".into(),
                        name: "WaitFor".into(),
                        kind: Default::default(),
                        provider_data: None,
                        input: json!({
                            "wake": "operator_input", "delivery": "final",
                            "reason": "fixture intentionally remains waiting",
                        }),
                    }],
                    stop_reason: Some("tool_use".into()),
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_usage: None,
                    provider_message_id: None,
                    provider_request_id: None,
                    request_diagnostics: None,
                });
            }
        } else if self.recover {
            self.recovery_started.notify_one();
            self.recovery_release.notified().await;
        }
        StubProvider::new(self.answer).complete_turn(request).await
    }

    fn select_model_lineage(
        &self,
        _: &crate::config::ModelRouteRef,
    ) -> Option<Arc<dyn AgentProvider>> {
        Some(Arc::new(self.clone()))
    }
}

async fn reply_gate_fixture(
    timeout_ms: u64,
    answer: &'static str,
    wait: bool,
) -> (
    tempfile::TempDir,
    RuntimeHost,
    RuntimeHandle,
    RuntimeHandle,
    Arc<ReplyGateProvider>,
    crate::types::AgentInvocationReceipt,
) {
    reply_gate_recovery_fixture(timeout_ms, answer, wait, false).await
}

async fn reply_gate_recovery_fixture(
    timeout_ms: u64,
    answer: &'static str,
    wait: bool,
    recover: bool,
) -> (
    tempfile::TempDir,
    RuntimeHost,
    RuntimeHandle,
    RuntimeHandle,
    Arc<ReplyGateProvider>,
    crate::types::AgentInvocationReceipt,
) {
    let home = tempdir().unwrap();
    write_test_model_config(home.path());
    let mut config = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
    config.user_home_dir = Some(home.path().to_path_buf());
    config.default_agent_id = "default".into();
    let provider = Arc::new(ReplyGateProvider {
        first: AtomicBool::new(false),
        started: Notify::new(),
        release: Notify::new(),
        recovery_started: Notify::new(),
        recovery_release: Notify::new(),
        answer,
        wait,
        recover,
    });
    let host = RuntimeHost::new_with_provider(config, Arc::new(provider.clone())).unwrap();
    let parent = host.default_runtime().await.unwrap();
    parent
        .agent_creation_service()
        .create(CreateAgentRequest {
            agent_id: "reply-gate-peer".into(),
            name: None,
            template: None,
            initial_message: None,
            authority_class: AuthorityClass::OperatorInstruction,
            model_resolution: Some(inherited_model_resolution("openai", "gpt-5.4")),
            lineage_parent_agent_id: None,
            inherit_parent_runtime: true,
        })
        .await
        .unwrap();
    let receipt = parent
        .agent_invocation_service()
        .invoke(InvokeAgentRequest {
            target: InvokeAgentTarget::ExistingAgent {
                agent_id: "reply-gate-peer".into(),
                reply_timeout_ms: Some(timeout_ms),
            },
            message: "return the answer for this exact request".into(),
            authority_class: AuthorityClass::OperatorInstruction,
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), provider.started.notified())
        .await
        .unwrap();
    let peer = host.get_public_agent("reply-gate-peer").await.unwrap();
    (home, host, parent, peer, provider, receipt)
}

#[tokio::test]
async fn agent_message_wait_provider_fallback_returns_exact_final_without_explicit_reply() {
    let (_home, host, parent, peer, gate, receipt) =
        reply_gate_recovery_fixture(0, "request-bound fallback answer", false, true).await;
    gate.release.notify_one();
    tokio::time::timeout(Duration::from_secs(10), gate.recovery_started.notified())
        .await
        .unwrap();
    assert_provider_reply_rejects_unproven_sources(&host, &peer).await;
    gate.recovery_release.notify_one();
    wait_for_agent_inputs_settled(&peer).await;
    let task = wait_for_terminal_task(&parent, &receipt.task_handle.task_id).await;
    assert_eq!(task.status, TaskStatus::Completed, "{task:?}");
    assert_eq!(reply_outcome_count(&host, &task.id), 1);
    let message = parent
        .storage()
        .read_message_by_id(&format!("message:agent-final-brief-reply:{}", task.id))
        .unwrap()
        .unwrap();
    assert!(serde_json::to_string(&message)
        .unwrap()
        .contains("request-bound fallback answer"));
    let turns: Vec<crate::types::TurnRecord> = host
        .runtime_db()
        .with_read(|connection| {
            let mut statement = connection.prepare(
                "SELECT payload_json FROM turn_records WHERE agent_id = 'reply-gate-peer'",
            )?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
        })
        .unwrap();
    assert!(turns.iter().any(|turn| turn.terminal.as_ref().is_some_and(
        |terminal| terminal.kind == crate::types::TurnTerminalKind::DeferredToFallback
    )));
    assert!(turns.iter().any(|turn| turn
        .terminal
        .as_ref()
        .is_some_and(|terminal| terminal.kind == crate::types::TurnTerminalKind::Completed)));
}

async fn assert_provider_reply_rejects_unproven_sources(host: &RuntimeHost, peer: &RuntimeHandle) {
    use crate::runtime_db::agent_message_delivery::prepare_final_brief_reply_tx;
    use crate::types::{
        BriefKind, BriefRecord, TranscriptEntry, TranscriptEntryKind, TurnTerminalKind,
        TurnTerminalSummary,
    };
    let state = peer.agent_state().await.unwrap();
    let mut turn = peer
        .storage()
        .read_turn_by_id(state.current_turn_id.as_deref().unwrap())
        .unwrap()
        .unwrap();
    let source_id = state.current_execution_binding.unwrap().source_message_id;
    let message = peer
        .storage()
        .read_message_by_id(&source_id)
        .unwrap()
        .unwrap();
    let root_turn_id = message.source_refs["source_turn_id"].clone();
    let mut brief = BriefRecord::new(
        &turn.agent_id,
        BriefKind::Result,
        "bound recovery final",
        None,
        None,
    );
    brief.turn_id = Some(turn.turn_id.clone());
    brief.turn_index = Some(turn.turn_index);
    brief.work_item_id = turn.current_work_item_id.clone();
    let round = TranscriptEntry::new(
        &turn.agent_id,
        TranscriptEntryKind::AssistantRound,
        Some(0),
        None,
        json!({"turn_id": turn.turn_id,
            "round_purpose": "agent_response", "visibility": "operator_visible"}),
    );
    brief.finalizes_assistant_round_id = Some(round.id.clone());
    turn.produced_brief_ids = vec![brief.id.clone()];
    turn.terminal = Some(TurnTerminalSummary {
        kind: TurnTerminalKind::Completed,
        reason: None,
        no_brief_reason: None,
        completed_at: Utc::now(),
        duration_ms: 1,
    });
    host.runtime_db()
        .transaction(|tx| {
            assert!(
                prepare_final_brief_reply_tx(tx, &turn, &[brief.clone()], &[round.clone()])?
                    .is_some(),
                "the canonical resolver must prove the real provider fallback source"
            );
            for case in [
                "foreign_input",
                "ambiguous_root",
                "forged_reference",
                "wrong_consumption",
            ] {
                tx.execute_batch("SAVEPOINT recovery_reply_negative")?;
                let mut candidate = turn.clone();
                match case {
                    "foreign_input" => candidate.input_message_ids.push("unrelated-request".into()),
                    "ambiguous_root" => {
                        tx.execute(
                            "UPDATE turn_records SET payload_json = json_insert(payload_json,
                     '$.input_message_ids[#]', 'second-request') WHERE turn_id = ?1",
                            [&root_turn_id],
                        )?;
                    }
                    "forged_reference" => {
                        tx.execute(
                            "UPDATE messages SET payload_json = json_set(payload_json,
                     '$.source_refs.source_message_id', 'foreign-request') WHERE message_id = ?1",
                            [&source_id],
                        )?;
                    }
                    "wrong_consumption" => {
                        tx.execute(
                    "UPDATE agent_message_deliveries SET payload_json = json_set(payload_json,
                     '$.activation_id', 'foreign-activation') WHERE target_agent_id = ?1",
                    [&turn.agent_id])?;
                    }
                    _ => unreachable!(),
                }
                assert!(
                    prepare_final_brief_reply_tx(
                        tx,
                        &candidate,
                        &[brief.clone()],
                        &[round.clone()]
                    )?
                    .is_none(),
                    "{case} must not authorize an automatic reply"
                );
                tx.execute_batch(
                    "ROLLBACK TO recovery_reply_negative; RELEASE recovery_reply_negative",
                )?;
            }
            Ok(())
        })
        .unwrap();
}

fn reply_outcome_count(host: &RuntimeHost, task_id: &str) -> usize {
    host.runtime_db()
        .with_read(|connection| {
            Ok(connection.query_row(
                "SELECT count(*) FROM agent_message_wait_outcomes WHERE task_id = ?1",
                [task_id],
                |row| row.get::<_, usize>(0),
            )?)
        })
        .unwrap()
}

fn reply_result_count(host: &RuntimeHost, task_id: &str) -> usize {
    host.runtime_db()
        .with_read(|connection| {
            Ok(connection.query_row(
                "SELECT count(*) FROM messages WHERE agent_id = 'default' AND kind = 'task_result'
             AND json_extract(payload_json, '$.task_id') = ?1",
                [task_id],
                |row| row.get::<_, usize>(0),
            )?)
        })
        .unwrap()
}

#[tokio::test]
async fn agent_message_wait_explicit_reply_wins_without_a_derived_duplicate() {
    let (_home, host, parent, peer, gate, receipt) =
        reply_gate_fixture(0, "final answer", false).await;
    let response = send_agent_message(&peer, "default", "reply-winner", "explicit answer").await;
    let terminal = wait_for_terminal_task(&parent, &receipt.task_handle.task_id).await;
    assert_eq!(terminal.status, TaskStatus::Completed);
    assert_eq!(
        terminal.detail.as_ref().unwrap()["response_delivery_id"],
        response.delivery_id
    );
    gate.release.notify_one();
    wait_for_agent_inputs_settled(&peer).await;
    let derived = format!("message:agent-final-brief-reply:{}", terminal.id);
    assert!(parent
        .storage()
        .read_message_by_id(&derived)
        .unwrap()
        .is_none());
    assert_eq!(reply_outcome_count(&host, &terminal.id), 1);
    assert_eq!(reply_result_count(&host, &terminal.id), 1);
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn agent_message_wait_timeout_does_not_stop_target_or_revive_for_late_reply() {
    let (_home, host, parent, peer, gate, receipt) =
        reply_gate_fixture(200, "late final", false).await;
    let terminal = wait_for_terminal_task(&parent, &receipt.task_handle.task_id).await;
    assert_eq!(terminal.status, TaskStatus::Failed);
    assert_eq!(
        terminal.detail.as_ref().unwrap()["failure_reason"],
        "reply_timeout"
    );
    assert!(peer.agent_state().await.unwrap().current_run_id.is_some());
    let late = send_agent_message(&peer, "default", "late-explicit", "late explicit answer").await;
    assert_eq!(
        late.outcome,
        crate::types::AgentMessageDeliveryOutcome::Accepted
    );
    gate.release.notify_one();
    wait_for_agent_inputs_settled(&peer).await;
    assert_eq!(
        parent
            .storage()
            .latest_task_record(&terminal.id)
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::Failed
    );
    assert!(parent
        .storage()
        .read_message_by_id(&format!("message:agent-final-brief-reply:{}", terminal.id))
        .unwrap()
        .is_none());
    assert_eq!(reply_outcome_count(&host, &terminal.id), 1);
    assert_eq!(reply_result_count(&host, &terminal.id), 1);
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn agent_message_wait_recovered_outbox_beats_expired_deadline() {
    let (_home, host, parent, peer, gate, receipt) =
        reply_gate_fixture(1000, "durable final", false).await;
    let task = parent
        .storage()
        .latest_task_record(&receipt.task_handle.task_id)
        .unwrap()
        .unwrap();
    parent.abort_async_task_monitor_for_test(&task.id).await;
    gate.release.notify_one();
    wait_for_agent_inputs_settled(&peer).await;
    assert_eq!(reply_outcome_count(&host, &task.id), 1);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let (reattached, remaining) = parent
        .recover_supervised_child_tasks(vec![task.clone()])
        .await
        .unwrap();
    assert_eq!(reattached.len(), 1);
    assert!(remaining.is_empty());
    let terminal = wait_for_terminal_task(&parent, &task.id).await;
    assert_eq!(terminal.status, TaskStatus::Completed);
    assert_eq!(
        terminal.recovery, task.recovery,
        "recovery must not extend the deadline"
    );
    let result = parent
        .storage()
        .read_message_by_id(&format!("message:agent-message-wait:{}", task.id))
        .unwrap()
        .unwrap();
    assert_eq!(
        result.body,
        MessageBody::Text {
            text: "durable final".into()
        }
    );
    assert_eq!(reply_result_count(&host, &task.id), 1);
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn agent_message_wait_recovery_keeps_deadline_and_does_not_resend() {
    let (_home, host, parent, peer, gate, receipt) =
        reply_gate_fixture(0, "late final", false).await;
    let mut task = parent
        .storage()
        .latest_task_record(&receipt.task_handle.task_id)
        .unwrap()
        .unwrap();
    parent.abort_async_task_monitor_for_test(&task.id).await;
    // Replace the isolated orphan snapshot; production deadlines are immutable.
    let Some(crate::types::TaskRecoverySpec::AgentMessageWait {
        reply_deadline_at, ..
    }) = task.recovery.as_mut()
    else {
        panic!("expected agent message wait recovery");
    };
    *reply_deadline_at = Some(Utc::now() - chrono::Duration::seconds(1));
    host.runtime_db()
        .transaction(|tx| {
            assert_eq!(
                tx.execute(
                    "UPDATE tasks SET payload_json = ?1 WHERE task_id = ?2",
                    rusqlite::params![serde_json::to_string(&task)?, task.id],
                )?,
                1
            );
            Ok(())
        })
        .unwrap();
    parent
        .recover_supervised_child_tasks(vec![task.clone()])
        .await
        .unwrap();
    let terminal = wait_for_terminal_task(&parent, &task.id).await;
    assert_eq!(terminal.status, TaskStatus::Failed);
    assert_eq!(terminal.recovery, task.recovery);
    gate.release.notify_one();
    wait_for_agent_inputs_settled(&peer).await;
    assert_eq!(reply_result_count(&host, &task.id), 1);
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn agent_message_wait_retries_outbox_read_and_terminal_commit_without_synthetic_failure() {
    let (_home, host, parent, peer, gate, receipt) =
        reply_gate_fixture(0, "durable answer after storage repair", false).await;
    let task = parent
        .storage()
        .latest_task_record(&receipt.task_handle.task_id)
        .unwrap()
        .unwrap();
    parent.abort_async_task_monitor_for_test(&task.id).await;
    gate.release.notify_one();
    wait_for_agent_inputs_settled(&peer).await;
    let message_id = format!("message:agent-final-brief-reply:{}", task.id);
    let original: String = host
        .runtime_db()
        .transaction(|tx| {
            let original = tx.query_row(
                "SELECT payload_json FROM messages WHERE evidence_id = ?1",
                [&message_id],
                |row| row.get::<_, String>(0),
            )?;
            tx.execute(
                "UPDATE messages SET payload_json = '{}' WHERE evidence_id = ?1",
                [&message_id],
            )?;
            tx.execute_batch(&format!(
                "CREATE TRIGGER block_reply_terminal BEFORE UPDATE ON tasks
             WHEN NEW.task_id = '{}' AND NEW.status = 'completed'
             BEGIN SELECT RAISE(ABORT, 'fixture terminal commit unavailable'); END;",
                task.id,
            ))?;
            Ok(original)
        })
        .unwrap();
    parent
        .recover_supervised_child_tasks(vec![task.clone()])
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(reply_result_count(&host, &task.id), 0);
    assert_eq!(
        parent
            .storage()
            .latest_task_record(&task.id)
            .unwrap()
            .unwrap()
            .status,
        task.status
    );
    host.runtime_db()
        .transaction(|tx| {
            tx.execute(
                "UPDATE messages SET payload_json = ?1 WHERE evidence_id = ?2",
                rusqlite::params![original, message_id],
            )?;
            Ok(())
        })
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(reply_result_count(&host, &task.id), 0);
    assert_eq!(
        parent
            .storage()
            .latest_task_record(&task.id)
            .unwrap()
            .unwrap()
            .status,
        task.status
    );
    host.runtime_db()
        .transaction(|tx| {
            tx.execute_batch("DROP TRIGGER block_reply_terminal")?;
            Ok(())
        })
        .unwrap();
    let terminal = wait_for_terminal_task(&parent, &task.id).await;
    assert_eq!(terminal.status, TaskStatus::Completed);
    assert_eq!(reply_result_count(&host, &task.id), 1);
    assert_eq!(reply_outcome_count(&host, &task.id), 1);
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn agent_message_wait_durable_outbox_survives_a_real_host_restart_once() {
    let (_home, host, parent, peer, gate, receipt) =
        reply_gate_fixture(1000, "answer committed before restart", false).await;
    let task = parent
        .storage()
        .latest_task_record(&receipt.task_handle.task_id)
        .unwrap()
        .unwrap();
    parent.abort_async_task_monitor_for_test(&task.id).await;
    gate.release.notify_one();
    wait_for_agent_inputs_settled(&peer).await;
    assert_eq!(reply_outcome_count(&host, &task.id), 1);
    assert_eq!(reply_result_count(&host, &task.id), 0);
    let config = host.config().as_ref().clone();
    host.shutdown().await.unwrap();
    drop(peer);
    drop(parent);
    drop(host);
    tokio::time::sleep(Duration::from_millis(1100)).await;

    let restarted =
        RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("must not resend")))
            .unwrap();
    restarted
        .recover_orphaned_queue_claims_at_startup()
        .await
        .unwrap();
    let parent = restarted.default_runtime().await.unwrap();
    let terminal = wait_for_terminal_task(&parent, &task.id).await;
    assert_eq!(terminal.status, TaskStatus::Completed);
    assert_eq!(terminal.recovery, task.recovery);
    let result = parent
        .storage()
        .read_message_by_id(&format!("message:agent-message-wait:{}", task.id))
        .unwrap()
        .unwrap();
    assert_eq!(
        result.body,
        MessageBody::Text {
            text: "answer committed before restart".into()
        }
    );
    // A stale recovery snapshot and repeated reconciliation cannot publish twice.
    parent
        .recover_supervised_child_tasks(vec![task.clone(), task.clone()])
        .await
        .unwrap();
    assert_eq!(reply_outcome_count(&restarted, &task.id), 1);
    assert_eq!(reply_result_count(&restarted, &task.id), 1);
    let requests: usize = restarted
        .runtime_db()
        .with_read(|connection| {
            Ok(connection.query_row(
                "SELECT count(*) FROM agent_message_deliveries
             WHERE json_extract(payload_json, '$.caller.current_task_id') = ?1",
                [&task.id],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(
        requests, 1,
        "restart must never resend the business request"
    );
    restarted.shutdown().await.unwrap();
}

#[tokio::test]
async fn agent_message_wait_empty_or_waiting_final_is_not_a_fallback_reply() {
    for (answer, wait) in [("", false), ("intentionally waiting, not an answer", true)] {
        let (_home, host, parent, peer, gate, receipt) =
            reply_gate_fixture(500, answer, wait).await;
        gate.release.notify_one();
        wait_for_agent_inputs_settled(&peer).await;
        let terminal = wait_for_terminal_task(&parent, &receipt.task_handle.task_id).await;
        assert_eq!(terminal.status, TaskStatus::Failed, "wait={wait}");
        assert_eq!(
            terminal.detail.as_ref().unwrap()["failure_reason"],
            "reply_timeout"
        );
        assert!(parent
            .storage()
            .read_message_by_id(&format!("message:agent-final-brief-reply:{}", terminal.id))
            .unwrap()
            .is_none());
        assert_eq!(reply_result_count(&host, &terminal.id), 1);
        host.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn agent_message_wait_cancelled_task_fences_the_targets_later_final() {
    let (_home, host, parent, peer, gate, receipt) =
        reply_gate_fixture(0, "must not revive cancelled wait", false).await;
    let task_id = &receipt.task_handle.task_id;
    let cancelled = parent
        .stop_task(task_id, &AuthorityClass::OperatorInstruction)
        .await
        .unwrap();
    assert_eq!(cancelled.status, TaskStatus::Cancelled);
    gate.release.notify_one();
    wait_for_agent_inputs_settled(&peer).await;
    assert_eq!(reply_outcome_count(&host, task_id), 0);
    assert!(parent
        .storage()
        .read_message_by_id(&format!("message:agent-final-brief-reply:{task_id}"))
        .unwrap()
        .is_none());
    assert_eq!(
        parent
            .storage()
            .latest_task_record(task_id)
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::Cancelled
    );
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn agent_message_wait_final_requires_exact_request_turn_and_round_evidence() {
    use crate::runtime_db::agent_message_delivery::{
        commit_final_brief_reply_tx, prepare_final_brief_reply_tx,
    };
    use crate::types::{
        BriefKind, BriefRecord, TranscriptEntry, TranscriptEntryKind, TurnTerminalKind,
        TurnTerminalSummary,
    };

    let (_home, host, parent, peer, gate, receipt) =
        reply_gate_fixture(0, "real eventual final", false).await;
    let state = peer.agent_state().await.unwrap();
    let mut turn = peer
        .storage()
        .read_turn_by_id(state.current_turn_id.as_deref().unwrap())
        .unwrap()
        .unwrap();
    let mut brief = BriefRecord::new(
        &turn.agent_id,
        BriefKind::Result,
        "fixture final",
        None,
        None,
    );
    brief.turn_id = Some(turn.turn_id.clone());
    brief.turn_index = Some(turn.turn_index);
    brief.work_item_id = turn.current_work_item_id.clone();
    let round = TranscriptEntry::new(
        &turn.agent_id,
        TranscriptEntryKind::AssistantRound,
        Some(0),
        None,
        json!({"turn_id": turn.turn_id, "round_purpose": "agent_response", "visibility": "operator_visible"}),
    );
    brief.finalizes_assistant_round_id = Some(round.id.clone());
    turn.produced_brief_ids = vec![brief.id.clone()];
    turn.terminal = Some(TurnTerminalSummary {
        kind: TurnTerminalKind::Completed,
        reason: None,
        no_brief_reason: None,
        completed_at: Utc::now(),
        duration_ms: 1,
    });
    host.runtime_db().transaction(|tx| {
        assert!(prepare_final_brief_reply_tx(tx, &turn, &[brief.clone()], &[round.clone()])?.is_some(),
            "the settlement may commit its final round and brief together");
        assert!(prepare_final_brief_reply_tx(tx, &turn, &[brief.clone()], &[])?.is_none(),
            "a missing durable or same-transaction round is not attribution evidence");
        crate::runtime_db::evidence::append_transcript_entry_tx(tx, &round)?;
        let prepared = prepare_final_brief_reply_tx(tx, &turn, &[brief.clone()], &[])?;
        assert!(prepared.is_some(), "positive fixture must prove real admitted request ownership");
        assert!(commit_final_brief_reply_tx(tx, prepared)?.is_empty(),
            "an open execution cannot publish merely because it has a brief");
        let task_payload: String = tx.query_row(
            "SELECT payload_json FROM tasks WHERE task_id = ?1",
            [&receipt.task_handle.task_id], |row| row.get(0),
        )?;
        for mutation in ["different_request", "different_boundary", "legacy_incarnation",
            "replaced_requester", "terminal_wait"] {
            let mut task: Value = serde_json::from_str(&task_payload)?;
            match mutation {
                "different_request" => task["recovery"]["delivery_id"] = json!("another-request"),
                "different_boundary" => task["recovery"]["after_delivery_rowid"] = json!(-1),
                "legacy_incarnation" => {
                    task["recovery"].as_object_mut().unwrap().remove("requester_incarnation");
                }
                "replaced_requester" => task["recovery"]["requester_incarnation"] = json!(9999),
                "terminal_wait" => task["status"] = json!("cancelled"),
                _ => unreachable!(),
            }
            tx.execute("UPDATE tasks SET payload_json = ?1 WHERE task_id = ?2",
                rusqlite::params![serde_json::to_string(&task)?, receipt.task_handle.task_id])?;
            assert!(prepare_final_brief_reply_tx(tx, &turn, &[brief.clone()], &[])?.is_none(), "{mutation}");
        }
        tx.execute("UPDATE tasks SET payload_json = ?1 WHERE task_id = ?2",
            rusqlite::params![task_payload, receipt.task_handle.task_id])?;
        for mutation in ["other_turn", "other_agent", "other_work_item", "other_index", "empty",
            "missing_round", "other_round", "not_final"] {
            let mut candidate = brief.clone();
            match mutation {
                "other_turn" => candidate.turn_id = Some("unrelated-turn".into()),
                "other_agent" => candidate.agent_id = "unrelated-agent".into(),
                "other_work_item" => candidate.work_item_id = Some("unrelated-work".into()),
                "other_index" => candidate.turn_index = Some(turn.turn_index + 1),
                "empty" => candidate.text = " \n ".into(),
                "missing_round" => candidate.finalizes_assistant_round_id = None,
                "other_round" => candidate.finalizes_assistant_round_id = Some("unrelated-round".into()),
                "not_final" => candidate.kind = BriefKind::Ack,
                _ => unreachable!(),
            }
            assert!(prepare_final_brief_reply_tx(tx, &turn, &[candidate], &[])?.is_none(), "{mutation}");
        }
        for mutation in ["multiple_inputs", "waiting", "aborted", "no_final", "multiple_finals"] {
            let mut candidate = turn.clone();
            let mut briefs = vec![brief.clone()];
            match mutation {
                "multiple_inputs" => candidate.input_message_ids.push("other-request".into()),
                "waiting" => candidate.waiting_condition_ids.push("active-wait".into()),
                "aborted" => candidate.terminal.as_mut().unwrap().kind = TurnTerminalKind::Aborted,
                "no_final" => candidate.produced_brief_ids.clear(),
                "multiple_finals" => {
                    let mut another = brief.clone();
                    another.id = "second-final".into();
                    candidate.produced_brief_ids.push(another.id.clone());
                    briefs.push(another);
                }
                _ => unreachable!(),
            }
            assert!(prepare_final_brief_reply_tx(tx, &candidate, &briefs, &[])?.is_none(), "{mutation}");
        }
        // A round from another turn or with non-final/internal purpose is not
        // attributable output, even when the brief points at that round ID.
        for data in [
            json!({"turn_id": "other-turn", "round_purpose": "agent_response", "visibility": "operator_visible"}),
            json!({"turn_id": turn.turn_id, "round_purpose": "continuation", "visibility": "operator_visible"}),
            json!({"turn_id": turn.turn_id, "round_purpose": "agent_response", "visibility": "internal"}),
        ] {
            let mut invalid = round.clone();
            invalid.data = data;
            tx.execute("UPDATE transcript_entries SET payload_json = ?1 WHERE evidence_id = ?2",
                rusqlite::params![serde_json::to_string(&invalid)?, invalid.id])?;
            assert!(prepare_final_brief_reply_tx(tx, &turn, &[brief.clone()], &[])?.is_none());
        }
        tx.execute("UPDATE transcript_entries SET payload_json = ?1 WHERE evidence_id = ?2",
            rusqlite::params![serde_json::to_string(&round)?, round.id])?;
        use crate::domain::execution_protocol::{
            ConversationOutcome, ExecutionAttempt, ExecutionAttemptState, ExecutionOutcome,
            ExecutionOutcomeRecord,
        };
        // Exercise the outbox writer with the real admitted source. Every
        // simulated settlement rolls back, leaving the runtime free to finish.
        for (consumed, outcome, expected) in [
            (false, ConversationOutcome::Replied, false),
            (true, ConversationOutcome::Failed { policy: "fixture".into() }, false),
            (true, ConversationOutcome::Paused { reason: "fixture".into() }, false),
            (true, ConversationOutcome::Interrupted { reason: "fixture".into() }, false),
            (true, ConversationOutcome::Replied, true),
        ] {
            tx.execute_batch("SAVEPOINT final_reply_crash_boundary")?;
            let prepared = prepare_final_brief_reply_tx(tx, &turn, &[brief.clone()], &[])?;
            let payload: String = tx.query_row(
                "SELECT payload_json FROM execution_protocol_attempts
                 WHERE agent_id = ?1 AND lifecycle_state = 'open'",
                [&turn.agent_id], |row| row.get(0),
            )?;
            let mut attempt: ExecutionAttempt = serde_json::from_str(&payload)?;
            attempt.state = ExecutionAttemptState::Settled;
            attempt.terminal_at = Some(Utc::now().to_rfc3339());
            attempt.terminal_outcome_id = Some("fixture-settlement".into());
            let outcome = ExecutionOutcomeRecord {
                outcome_id: "fixture-settlement".into(), attempt_id: attempt.attempt_id.clone(),
                outcome: ExecutionOutcome::Conversation(outcome), created_at: Utc::now().to_rfc3339(),
            };
            tx.execute(
                "UPDATE execution_protocol_attempts SET lifecycle_state = 'settled',
                 terminal_outcome_id = ?1, payload_json = ?2 WHERE agent_id = ?3 AND attempt_id = ?4",
                rusqlite::params![outcome.outcome_id, serde_json::to_string(&attempt)?,
                    turn.agent_id, attempt.attempt_id],
            )?;
            tx.execute(
                "INSERT INTO execution_protocol_outcomes (agent_id, outcome_id, attempt_id, payload_json)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![turn.agent_id, outcome.outcome_id, attempt.attempt_id,
                    serde_json::to_string(&outcome)?],
            )?;
            if consumed {
                tx.execute(
                    "UPDATE agent_message_deliveries SET state = 'consumed',
                     payload_json = json_set(payload_json, '$.state', 'consumed',
                       '$.activation_id', ?1, '$.turn_id', ?2)
                     WHERE message_id = ?3",
                    rusqlite::params![attempt.attempt_id, turn.turn_id, turn.input_message_ids[0]],
                )?;
            }
            assert_eq!(!commit_final_brief_reply_tx(tx, prepared)?.is_empty(), expected);
            tx.execute_batch("ROLLBACK TO final_reply_crash_boundary; RELEASE final_reply_crash_boundary")?;
            let outcomes: usize = tx.query_row(
                "SELECT count(*) FROM agent_message_wait_outcomes WHERE task_id = ?1",
                [&receipt.task_handle.task_id], |row| row.get(0),
            )?;
            let content: usize = tx.query_row(
                "SELECT count(*) FROM messages WHERE evidence_id = ?1",
                [format!("message:agent-final-brief-reply:{}", receipt.task_handle.task_id)],
                |row| row.get(0),
            )?;
            assert_eq!((outcomes, content), (0, 0), "rollback must retain no partial winner or content");
        }
        Ok(())
    }).unwrap();
    assert_eq!(reply_outcome_count(&host, &receipt.task_handle.task_id), 0);
    gate.release.notify_one();
    let terminal = wait_for_terminal_task(&parent, &receipt.task_handle.task_id).await;
    assert_eq!(terminal.status, TaskStatus::Completed);
    host.shutdown().await.unwrap();
}
