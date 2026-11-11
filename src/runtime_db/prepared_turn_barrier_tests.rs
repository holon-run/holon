use super::*;
use crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger;

fn start_turn(f: &Fixture, qualified: bool) -> Result<TurnRecord> {
    start_turn_with(f, qualified, |_| {})
}

fn start_turn_with(
    f: &Fixture,
    qualified: bool,
    initialize: impl FnOnce(&mut TurnRecord),
) -> Result<TurnRecord> {
    let mut record = TurnRecord::new(&f.scope.agent_id, "barrier-turn", 1);
    record.run_id = Some("barrier-run".into());
    record.created_at = f.at;
    initialize(&mut record);
    f.db.transaction(|tx| {
        let mut state = AgentState::new(&f.scope.agent_id);
        state.turn_index = record.turn_index;
        state.current_turn_id = Some(record.turn_id.clone());
        state.current_run_id = record.run_id.clone();
        upsert_agent_state_tx(tx, &state)?;
        if !qualified {
            upsert_turn_record_tx(tx, &record)?;
        }
        Ok(())
    })?;
    if qualified {
        let prepared = PreparedInputEvidence::turn(
            f.scope.clone(),
            "start-barrier-turn".into(),
            f.at,
            record.clone(),
        )?;
        f.db.evidence().commit_prepared_input_evidence(&prepared)?;
    }
    Ok(record)
}

fn prepare(f: &Fixture, prior: &TurnRecord) -> Result<PreparedInputEvidence> {
    PreparedInputEvidence::turn_barrier(
        f.scope.clone(),
        "turn-barrier-original".into(),
        f.at,
        prior,
        true,
        None,
        &InputEvidenceBudgetLedger::new(Default::default())?,
    )
}

#[test]
fn turn_barrier_command_admits_before_digest_and_retains_original_material() -> Result<()> {
    use crate::diagnostics::resource_io::ResourceReadMaterial;

    let f = Fixture::new()?;
    let mut prior = start_turn(&f, true)?;
    prior.tool_execution_ids = vec!["quoted \\\"{}[]:,".repeat(256); 8];
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &f.scope)?;
    let before = phase.finish()?;
    let prepared = phase.read(|| {
        PreparedInputEvidence::turn_barrier(
            f.scope.clone(),
            "admitted-barrier".into(),
            f.at,
            &prior,
            true,
            None,
            &ledger,
        )
    })?;
    let after = phase.finish()?;
    assert!(after.0 > before.0 && after.1 > before.1);
    let retained = prepared.original_material.as_ref().unwrap().clone();
    drop(phase);
    drop(prepared);
    assert!(ledger.report()?.observed_bytes > 0);
    drop(retained);
    assert_eq!(ledger.report()?.observed_bytes, 0);

    for limits in [(before.0, 65_536), (64 * 1024 * 1024, before.1)] {
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &f.scope, limits.0, limits.1)?;
        let error = phase
            .read(|| {
                PreparedInputEvidence::turn_barrier(
                    f.scope.clone(),
                    "refused-barrier".into(),
                    f.at,
                    &prior,
                    true,
                    None,
                    &ledger,
                )
            })
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("ResourceExhausted"),
            "{error:#}"
        );
        assert!(phase.finish().is_err());
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn turn_barrier_updates_only_flag_and_replays_original_after_later_turn_write() -> Result<()> {
    let f = Fixture::new()?;
    let prior = start_turn(&f, true)?;
    let prepared = prepare(&f, &prior)?;
    let InputMutationExecution::Committed { value, receipt } =
        f.db.evidence().commit_prepared_input_evidence(&prepared)?
    else {
        panic!("first barrier must commit")
    };
    assert!(matches!(
        value.record,
        PreparedEvidenceValue::TurnBarrierDone
    ));
    let mut expected = prior.clone();
    expected.external_side_effect_possible = true;
    let actual =
        f.db.turn_records()
            .by_id(Some(&f.scope.agent_id), &prior.turn_id)?
            .unwrap();
    assert_eq!(actual, expected);
    assert!(value.input_delta.query_advance.is_some());
    drop(value);
    expected.tool_execution_ids.push("later-ack".into());
    let later = PreparedInputEvidence::turn(
        f.scope.clone(),
        "later-turn-write".into(),
        f.at,
        expected.clone(),
    )?;
    f.db.evidence().commit_prepared_input_evidence(&later)?;
    let InputMutationExecution::Replayed {
        receipt: replay, ..
    } = f.db.evidence().commit_prepared_input_evidence(&prepared)?
    else {
        panic!("exact original must replay")
    };
    assert_eq!(replay, receipt);
    assert_eq!(
        f.db.turn_records()
            .by_id(Some(&f.scope.agent_id), &prior.turn_id)?
            .unwrap(),
        expected,
    );
    Ok(())
}

#[test]
fn turn_barrier_rejects_changed_qualified_prior_raw_body_and_lost_current_turn() -> Result<()> {
    for mode in ["changed", "raw", "not_current"] {
        let f = Fixture::new()?;
        let prior = start_turn(&f, mode != "raw")?;
        let prepared = prepare(&f, &prior)?;
        if mode == "changed" {
            let mut changed = prior.clone();
            changed.tool_execution_ids.push("concurrent-ack".into());
            let command = PreparedInputEvidence::turn(
                f.scope.clone(),
                "concurrent-turn".into(),
                f.at,
                changed,
            )?;
            f.db.evidence().commit_prepared_input_evidence(&command)?;
        } else if mode == "not_current" {
            f.db.transaction(|tx| {
                let mut state = AgentState::new(&f.scope.agent_id);
                state.turn_index = 2;
                upsert_agent_state_tx(tx, &state)
            })?;
        }
        let error =
            f.db.evidence()
                .commit_prepared_input_evidence(&prepared)
                .expect_err("barrier must not grant or overwrite prior evidence");
        let message = format!("{error:#}");
        assert!(
            if mode == "changed" {
                message.contains("Turn barrier prior changed")
            } else if mode == "not_current" {
                message.contains("Turn barrier is no longer current")
            } else {
                matches!(
                    error.downcast_ref::<crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError>(),
                    Some(crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError::NeedsPreparation(id)
                        | crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError::ProjectionChanged(id))
                        if id == &prior.turn_id
                )
            },
            "{mode}: {message}",
        );
        assert!(
            !f.db
                .turn_records()
                .by_id(Some(&f.scope.agent_id), &prior.turn_id)?
                .unwrap()
                .external_side_effect_possible
        );
        assert!(f
            .db
            .transaction(|tx| lookup_input_mutation_receipt_tx(tx, prepared.descriptor()))?
            .is_none());
    }
    Ok(())
}

#[test]
fn turn_barrier_sql_failure_rolls_back_native_qualification_and_original_receipt() -> Result<()> {
    let f = Fixture::new()?;
    let prior = start_turn(&f, true)?;
    let prepared = prepare(&f, &prior)?;
    f.db.transaction(|tx| {
        tx.execute_batch("CREATE TRIGGER reject_turn_barrier BEFORE UPDATE ON turn_records BEGIN SELECT RAISE(ABORT,'barrier write rejected'); END;")?;
        Ok(())
    })?;
    let error =
        f.db.evidence()
            .commit_prepared_input_evidence(&prepared)
            .unwrap_err();
    assert!(
        format!("{error:#}").contains("barrier write rejected"),
        "{error:#}"
    );
    assert_eq!(
        f.db.turn_records()
            .by_id(Some(&f.scope.agent_id), &prior.turn_id)?
            .unwrap(),
        prior
    );
    f.db.transaction(|tx| {
        assert!(lookup_input_mutation_receipt_tx(tx, prepared.descriptor())?.is_none());
        validate_existing_input_evidence_tx(
            tx,
            &f.scope,
            &InputEvidenceKey::new(Family::Turn, &prior.turn_id),
        )?;
        tx.execute_batch("DROP TRIGGER reject_turn_barrier")?;
        Ok(())
    })?;
    assert!(matches!(
        f.db.evidence().commit_prepared_input_evidence(&prepared)?,
        InputMutationExecution::Committed { .. }
    ));
    Ok(())
}

fn prepared_tool_result(f: &Fixture, prior: Option<TurnRecord>) -> Result<PreparedInputEvidence> {
    let mut record = f.tool();
    if let Some(turn) = &prior {
        record.turn_index = turn.turn_index;
        record.turn_id = Some(turn.turn_id.clone());
    }
    let event = AuditEvent::legacy(
        "tool_executed",
        serde_json::json!({
            "agent_id": record.agent_id, "tool_execution_id": record.id,
        }),
    );
    PreparedInputEvidence::tool_result(
        f.scope.clone(),
        "tool-result-original".into(),
        f.at,
        record,
        Vec::new(),
        event,
        prior,
    )
}

#[test]
fn tool_result_commits_evidence_audits_and_barrier_once_and_replays_after_later_turn() -> Result<()>
{
    let f = Fixture::new()?;
    let mut prior = start_turn(&f, true)?;
    f.db.evidence()
        .commit_prepared_input_evidence(&prepare(&f, &prior)?)?;
    prior.external_side_effect_possible = true;
    let prepared = prepared_tool_result(&f, Some(prior.clone()))?;
    let InputMutationExecution::Committed { value, receipt } =
        f.db.evidence().commit_prepared_input_evidence(&prepared)?
    else {
        panic!("original commit")
    };
    let PreparedEvidenceValue::ToolResult(events) = &value.record else {
        panic!("tool result ACK")
    };
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].kind, "tool_executed");
    assert_eq!(events[1].kind, "external_command_checkpoint_durable");
    assert!(events[0].event_seq < events[1].event_seq);
    assert!(receipt.outcome.applied);
    assert_eq!(receipt.outcome.scopes[0].audit_events.len(), 2);
    assert!(value
        .input_delta
        .records
        .iter()
        .any(|r| r.locator.key == InputEvidenceKey::new(Family::Turn, &prior.turn_id)));
    assert!(value
        .input_delta
        .records
        .iter()
        .any(|r| r.locator.key == InputEvidenceKey::new(Family::ToolExecution, "tool:original")));
    assert!(value.current_read_writes.is_some());
    let mut changed = prior.clone();
    changed.external_side_effect_possible = false;
    assert_eq!(
        f.db.turn_records()
            .by_id(Some(&f.scope.agent_id), &prior.turn_id)?,
        Some(changed.clone())
    );
    changed.tool_execution_ids.push("later-tool".into());
    f.db.evidence()
        .commit_prepared_input_evidence(&PreparedInputEvidence::turn(
            f.scope.clone(),
            "later-tool-turn".into(),
            f.at,
            changed.clone(),
        )?)?;
    let InputMutationExecution::Replayed {
        receipt: replay, ..
    } = f.db.evidence().commit_prepared_input_evidence(&prepared)?
    else {
        panic!("exact replay")
    };
    assert_eq!(replay, receipt);
    assert_eq!(f.count("tool_executions")?, 1);
    assert_eq!(f.count("audit_events")?, 2);
    assert_eq!(
        f.db.turn_records()
            .by_id(Some(&f.scope.agent_id), &prior.turn_id)?,
        Some(changed)
    );
    Ok(())
}

#[test]
fn tool_result_failures_roll_back_evidence_audit_barrier_and_receipt() -> Result<()> {
    for phase in ["audit_events", "turn_records", "input_mutation_receipts"] {
        let f = Fixture::new()?;
        let mut prior = start_turn(&f, true)?;
        f.db.evidence()
            .commit_prepared_input_evidence(&prepare(&f, &prior)?)?;
        prior.external_side_effect_possible = true;
        let prepared = prepared_tool_result(&f, Some(prior.clone()))?;
        let operation = if phase == "turn_records" {
            "UPDATE"
        } else {
            "INSERT"
        };
        f.db.transaction(|tx| { tx.execute_batch(&format!(
            "CREATE TRIGGER reject_tool_result BEFORE {operation} ON {phase} BEGIN SELECT RAISE(ABORT,'tool result rejected'); END;"))?; Ok(()) })?;
        let error =
            f.db.evidence()
                .commit_prepared_input_evidence(&prepared)
                .unwrap_err();
        assert!(
            format!("{error:#}").contains("tool result rejected"),
            "{error:#}"
        );
        assert_eq!(f.count("tool_executions")?, 0);
        assert_eq!(f.count("audit_events")?, 0);
        assert_eq!(
            f.db.turn_records()
                .by_id(Some(&f.scope.agent_id), &prior.turn_id)?,
            Some(prior)
        );
        f.db.transaction(|tx| {
            assert!(lookup_input_mutation_receipt_tx(tx, prepared.descriptor())?.is_none());
            tx.execute_batch("DROP TRIGGER reject_tool_result")?;
            Ok(())
        })?;
        assert!(matches!(
            f.db.evidence().commit_prepared_input_evidence(&prepared)?,
            InputMutationExecution::Committed { .. }
        ));
    }
    Ok(())
}

#[test]
fn tool_result_without_command_barrier_still_has_one_original_audit() -> Result<()> {
    let f = Fixture::new()?;
    let prepared = prepared_tool_result(&f, None)?;
    let InputMutationExecution::Committed { value, receipt } =
        f.db.evidence().commit_prepared_input_evidence(&prepared)?
    else {
        panic!("original commit")
    };
    let PreparedEvidenceValue::ToolResult(events) = value.record else {
        panic!("tool ACK")
    };
    assert_eq!(events.len(), 1);
    assert!(receipt.outcome.applied);
    assert_eq!(receipt.outcome.scopes[0].audit_events.len(), 1);
    assert_eq!(f.count("tool_executions")?, 1);
    Ok(())
}

#[test]
fn exec_command_startup_recovers_committed_result_and_interrupts_pending_attempt() -> Result<()> {
    use crate::diagnostics::io_probe::IoProbe;
    use crate::runtime::resident_input_evidence::InputEvidenceKey as Key;
    use crate::runtime_db::input_evidence_snapshot::legacy_recovery::ExclusiveStartupInputRecovery;
    use crate::tool::names::EXEC_COMMAND;
    use crate::types::ToolExecutionCallBinding;
    use serde_json::json;

    let mut probe = IoProbe::start();
    let f = Fixture::new()?;
    let mut message = crate::types::MessageEnvelope::new(
        &f.scope.agent_id,
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "Run command".into(),
        },
    );
    let turn = start_turn_with(&f, true, |turn| {
        message.turn_id = Some(turn.turn_id.clone());
        turn.trigger = Some(crate::types::TurnTriggerSummary::from_message(&message));
        turn.input_message_ids.push(message.id.clone());
    })?;
    f.db.evidence()
        .commit_prepared_input_evidence(&PreparedInputEvidence::message(
            f.scope.clone(),
            "original-trigger".into(),
            f.at,
            message.clone(),
            Vec::new(),
        )?)?;
    f.db.queue_entries()
        .upsert(&crate::types::QueueEntryRecord {
            message_id: message.id,
            agent_id: f.scope.agent_id.clone(),
            priority: Priority::Normal,
            status: crate::types::QueueEntryStatus::Dequeued,
            created_at: f.at,
            updated_at: f.at,
        })?;
    let mut assistant_round = f.transcript();
    assistant_round.kind = TranscriptEntryKind::AssistantRound;
    assistant_round.round = Some(1);
    assistant_round.data = json!({
        "turn_id": turn.turn_id,
        "blocks": [
            {"type":"tool_use", "id":"settled", "name":EXEC_COMMAND},
            {"type":"tool_use", "id":"pending", "name":EXEC_COMMAND},
        ],
    });
    let db = &f.db;
    let repo = db.evidence();
    repo.commit_prepared_input_evidence(&PreparedInputEvidence::transcript(
        f.scope.clone(),
        "original-round".into(),
        f.at,
        assistant_round.clone(),
    )?)?;
    let begin = |call: &str| -> Result<ToolExecutionRecord> {
        let mut record = f.tool();
        record.id = format!("tool:{call}");
        record.turn_id = Some(turn.turn_id.clone());
        record.turn_index = turn.turn_index;
        record.status = ToolExecutionStatus::Pending;
        record.completed_at = None;
        record.output = serde_json::Value::Null;
        record.call_binding = Some(ToolExecutionCallBinding {
            incarnation: f.scope.identity_incarnation,
            assistant_round_id: assistant_round.id.clone(),
            tool_call_id: call.into(),
            model_output_budget: 4096,
        });
        repo.commit_prepared_input_evidence(&PreparedInputEvidence::tool(
            f.scope.clone(),
            format!("start:{call}"),
            f.at,
            record.clone(),
            Vec::new(),
        )?)?;
        Ok(record)
    };
    let legacy = Fixture::new()?;
    let mut legacy_turn = start_turn(&legacy, true)?;
    let legacy_begin = prepare(&legacy, &legacy_turn)?;
    legacy_turn.external_side_effect_possible = true;
    let legacy_result = prepared_tool_result(&legacy, Some(legacy_turn.clone()))?;
    probe.reset();
    let started = std::time::Instant::now();
    legacy
        .db
        .evidence()
        .commit_prepared_input_evidence(&legacy_begin)?;
    legacy.db.transaction(|tx| {
        let event = AuditEvent::legacy(
            "external_command_no_replay_barrier",
            json!({"turn_id": legacy_turn.turn_id}),
        );
        let (event, _) = crate::runtime_db::evidence::append_audit_event_tx(
            tx,
            Some(&legacy.scope.agent_id),
            &event,
        )?;
        crate::runtime_db::current_read_writes::append_event_history_ack_tx(
            tx,
            &legacy.scope,
            &event.id,
        )
    })?;
    legacy
        .db
        .evidence()
        .commit_prepared_input_evidence(&legacy_result)?;
    let legacy_elapsed = started.elapsed();
    let legacy_sql = probe.snapshot().counters;
    probe.reset();

    let started = std::time::Instant::now();
    let mut settled = begin("settled")?;
    let result = crate::tool::ToolResult::success(
        EXEC_COMMAND,
        json!({
            "disposition":"completed", "exit_status":0, "stdout_preview":"original stdout",
            "truncated":false, "output_ref":format!("tool_execution:{}:output", settled.id),
        }),
        None,
    );
    settled.output = serde_json::to_value(&result)?;
    settled.status = ToolExecutionStatus::Success;
    settled.completed_at = Some(f.at);
    let event = AuditEvent::legacy(
        "tool_executed",
        json!({
            "agent_id": f.scope.agent_id,
            "tool_execution_id": settled.id,
            "tool_call_id": "settled",
        }),
    );
    repo.commit_prepared_input_evidence(&PreparedInputEvidence::tool_result(
        f.scope.clone(),
        "original-settlement".into(),
        f.at,
        settled.clone(),
        Vec::new(),
        event,
        None,
    )?)?;
    let new_elapsed = started.elapsed();
    let new_sql = probe.snapshot().counters;
    drop(probe);
    let statements = |sql: &crate::diagnostics::io_probe::IoProbeCounters| {
        sql.sql_read_statements + sql.sql_write_statements + sql.sql_control_statements
    };
    eprintln!(
        "ExecCommand writer fixture old={} SQL ({legacy_elapsed:?}), new={} SQL ({new_elapsed:?})",
        statements(&legacy_sql),
        statements(&new_sql)
    );
    let pending = begin("pending")?;

    let path = f._dir.path().join("runtime-maintenance.lock");
    let lease = crate::runtime_db::RuntimeDbLock::try_lock(&path)?;
    let handoff = tokio::sync::Mutex::new(());
    let guard = handoff.try_lock()?;
    let capability =
        ExclusiveStartupInputRecovery::for_pre_activation_host(db, &lease, &path, &guard)?;
    let scopes = db.startup_input_recovery_scopes(std::slice::from_ref(&f.scope.agent_id))?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let recovered = db.recover_interrupted_runtime_state_at_startup_with_tasks_in_scopes(
        &scopes,
        &capability,
        &ledger,
        None,
    )?;
    assert_eq!(recovered.report.interrupted_turns, 1);
    assert_eq!(
        db.queue_entries().latest_all()?[0].status,
        crate::types::QueueEntryStatus::Aborted,
    );
    let interrupted = repo
        .tool_execution_by_id(&f.scope.agent_id, &pending.id)?
        .unwrap();
    assert_eq!(interrupted.status, ToolExecutionStatus::Interrupted);
    assert_eq!(interrupted.output["reason"], "external_outcome_unknown");
    let current = db
        .turn_records()
        .by_id(Some(&f.scope.agent_id), &turn.turn_id)?
        .unwrap();
    assert!(!current.external_side_effect_possible);
    assert_eq!(current.tool_execution_ids, [settled.id.clone()]);
    assert_eq!(
        current.terminal.unwrap().reason.as_deref(),
        Some("startup_recovery/external_side_effect_indeterminate"),
    );
    let recovered_id = format!("transcript:recovered-tool-result:{}", settled.id);
    db.transaction(|tx| {
        let entry = crate::runtime_db::evidence::committed_transcript_tx(
            tx,
            &f.scope.agent_id,
            &recovered_id,
        )?;
        assert_eq!(entry.round, assistant_round.round);
        let reference = &entry.data["refs"][0];
        assert_eq!(reference["tool_call_id"], "settled");
        assert_eq!(reference["tool_execution_id"], settled.id);
        assert_eq!(reference["is_error"], false);
        assert_eq!(
            reference["provider_visible_text"],
            crate::tool::tools::render_tool_result_for_model(&result)?
        );
        for key in [
            Key::new(Family::Transcript, &recovered_id),
            Key::new(Family::ToolExecution, &pending.id),
        ] {
            validate_existing_input_evidence_tx(tx, &f.scope, &key)?;
        }
        Ok(())
    })?;
    let repeated = db.recover_interrupted_runtime_state_at_startup_with_tasks_in_scopes(
        &scopes,
        &capability,
        &ledger,
        None,
    )?;
    assert_eq!(repeated.report.interrupted_turns, 0);
    assert_eq!(f.count("transcript_entries")?, 2);
    Ok(())
}
