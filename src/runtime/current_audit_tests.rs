use super::*;
use crate::{
    runtime::mutation_owner::{KnownCommit, MutationReservation},
    runtime_db::current_read_writes::{CurrentFamily, CurrentRecord},
};

#[tokio::test]
async fn message_delivery_mutation_entry_is_pointer_sized_and_lazy() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let pending = runtime.begin_message_delivery_mutation(None);
    assert_eq!(
        std::mem::size_of_val(&pending),
        std::mem::size_of::<usize>()
    );
    drop(pending);
    let (_mutation, scope) = runtime.begin_message_delivery_mutation(None).await?;
    assert!(scope.is_none());
    Ok(())
}

#[tokio::test]
async fn independent_audit_entry_is_pointer_sized_and_lazy() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let event = AuditEvent::legacy("independent_audit_lazy", serde_json::json!({}));
    let pending = runtime.append_original_audit_event(event.clone());
    assert_eq!(
        std::mem::size_of_val(&pending),
        std::mem::size_of::<usize>()
    );
    assert!(!runtime
        .runtime_db()
        .audit_events()
        .has_event_by_id(&event.id)?);
    drop(pending);
    assert!(!runtime
        .runtime_db()
        .audit_events()
        .has_event_by_id(&event.id)?);
    let writer = runtime
        .runtime_db()
        .reader_pool_writer_weak_for_tests()
        .upgrade()
        .unwrap();
    let admissions = writer.write_channel_admissions_for_tests()?;
    runtime.append_original_audit_event(event.clone()).await?;
    assert_eq!(writer.write_channel_admissions_for_tests()?, admissions + 1);
    assert!(runtime
        .runtime_db()
        .audit_events()
        .has_event_by_id(&event.id)?);
    Ok(())
}

#[tokio::test]
async fn independent_audit_failure_preserves_prior_state_ack_and_owns_its_event() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    {
        let mut mutation = runtime.begin_mutation().await?;
        mutation.state.total_model_rounds += 1;
        mutation.persist_state(&runtime.inner.storage).await?;
    }
    let accepted = runtime.inner.mutation_owner.capture()?.unwrap();
    runtime
        .inner
        .storage
        .enable_event_bus(crate::storage::EventBus::new(16))?;
    let mut notifications = runtime
        .inner
        .storage
        .subscribe_events()?
        .context("Audit event bus")?;
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch(
            "CREATE TRIGGER reject_independent_audit BEFORE INSERT ON audit_events
             BEGIN SELECT RAISE(ABORT,'independent Audit rejected'); END;",
        )?;
        Ok(())
    })?;
    let event = AuditEvent::legacy("independent_audit_after_state", serde_json::json!({}));
    let error = runtime
        .append_original_audit_event(event.clone())
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("independent Audit rejected"),
        "{error:#}"
    );
    assert_eq!(
        runtime.inner.mutation_owner.capture()?.unwrap().state,
        accepted.state
    );
    assert!(!runtime
        .runtime_db()
        .audit_events()
        .has_event_by_id(&event.id)?);
    assert!(matches!(
        notifications.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch("DROP TRIGGER reject_independent_audit;")?;
        Ok(())
    })?;
    runtime.append_original_audit_event(event.clone()).await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    assert_eq!(root.state, accepted.state);
    let ack = root
        .current
        .as_ref()
        .context("Audit original Current ACK")?;
    let selected = ack
        .record(&(CurrentFamily::Event, event.id))
        .context("Audit body")?;
    let CurrentRecord::Event(actual) = selected.record() else {
        panic!("expected Event");
    };
    assert_eq!(actual.kind, event.kind);
    assert!(actual.event_seq > 0);
    let published = notifications.try_recv()?;
    assert_eq!(published.agent_id.as_deref(), Some(root.state.id.as_str()));
    assert_eq!(&published.event, actual);
    assert!(matches!(
        notifications.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    Ok(())
}

#[tokio::test]
async fn independent_audit_known_install_uses_original_body_without_canonical_readback(
) -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let event = AuditEvent::legacy(
        "independent_audit_known",
        serde_json::json!({"value": "original"}),
    );
    runtime.inject_next_transition_fault(TransitionFaultPoint::BeforeCacheUpdate);
    let error = runtime
        .append_original_audit_event(event.clone())
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("committed but publication"),
        "{error:#}"
    );
    let mut reservations = vec![runtime.inner.mutation_owner.reserve().await?];
    let known = MutationReservation::validate_group_known(&reservations)?;
    let saved = known[0].as_ref().context("Known Audit commit")?;
    let KnownCommit::State {
        runtime: Some(facts),
        ..
    } = saved
    else {
        panic!("original Audit installation facts missing");
    };
    let original = facts
        .current
        .as_ref()
        .context("Known Audit Current ACK")?
        .clone();
    let selected = original
        .record(&(CurrentFamily::Event, event.id))
        .context("original body")?
        .clone();
    let material = selected.weak_material();
    assert!(facts.external_effects().current_read_writes.is_none());
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch("ALTER TABLE audit_events RENAME TO audit_readback_forbidden;")?;
        Ok(())
    })?;
    runtime
        .install_saved_mutation(&reservations[0], saved)
        .await?;
    MutationReservation::reconcile_all(&mut reservations, known)?;
    drop(reservations);
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    assert!(Arc::ptr_eq(
        &original,
        root.current.as_ref().context("installed Audit")?
    ));
    let old_root = Arc::downgrade(&root);
    drop(root);
    drop(original);
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch("ALTER TABLE audit_readback_forbidden RENAME TO audit_events;")?;
        Ok(())
    })?;
    runtime
        .append_audit_event("independent_audit_next", serde_json::json!({}))
        .await?;
    assert!(old_root.upgrade().is_none());
    assert!(material.upgrade().is_some());
    drop(selected);
    assert!(material.upgrade().is_none());
    Ok(())
}

#[tokio::test]
async fn independent_audit_cancelled_owner_wait_never_starts_a_write() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let reservation = runtime.inner.mutation_owner.reserve().await?;
    let event = AuditEvent::legacy("independent_audit_cancelled", serde_json::json!({}));
    let writer = {
        let runtime = runtime.clone();
        let event = event.clone();
        tokio::spawn(async move { runtime.append_original_audit_event(event).await })
    };
    tokio::task::yield_now().await;
    assert!(!writer.is_finished());
    writer.abort();
    assert!(writer.await.unwrap_err().is_cancelled());
    drop(reservation);
    assert!(!runtime
        .runtime_db()
        .audit_events()
        .has_event_by_id(&event.id)?);
    runtime.append_original_audit_event(event.clone()).await?;
    assert!(runtime
        .runtime_db()
        .audit_events()
        .has_event_by_id(&event.id)?);
    Ok(())
}

#[tokio::test]
async fn independent_audit_guard_keeps_state_ack_on_failure_and_rejects_dirty_candidate(
) -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let bus = crate::storage::EventBus::new(8);
    runtime.storage().enable_event_bus(bus.clone())?;
    let mut rx = bus.subscribe();
    let mut mutation = runtime.begin_mutation().await?;
    let event = AuditEvent::legacy("guarded_audit", serde_json::json!({}));
    let before = runtime.inner.mutation_owner.capture()?.unwrap();
    mutation.state.total_model_rounds += 1;
    let error = mutation
        .append_original_audit_event(event.clone())
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("original acknowledged State"),
        "{error:#}"
    );
    assert_eq!(
        runtime.inner.mutation_owner.capture()?.unwrap().state,
        before.state
    );
    assert!(!runtime
        .runtime_db()
        .audit_events()
        .has_event_by_id(&event.id)?);
    mutation.persist_state(runtime.storage()).await?;
    let accepted = runtime.inner.mutation_owner.capture()?.unwrap();
    let persisted = rx.try_recv()?;
    assert_eq!(persisted.event.kind, "agent_state_mutated");
    let qualified = persisted
        .qualified
        .as_ref()
        .context("State ACK Audit was raw")?;
    qualified.validate(
        &resident_input_evidence::InputEvidenceScope {
            agent_id: accepted.state.id.clone(),
            identity_incarnation: runtime.inner.identity_incarnation,
        },
        &runtime.inner.input_evidence_budget,
    )?;
    assert!(std::ptr::eq(qualified.event(), &*persisted.event));
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch(
            "CREATE TRIGGER reject_guarded_audit BEFORE INSERT ON audit_events
             BEGIN SELECT RAISE(ABORT,'guarded Audit rejected'); END;",
        )?;
        Ok(())
    })?;
    let error = mutation
        .append_original_audit_event(event.clone())
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("guarded Audit rejected"),
        "{error:#}"
    );
    assert_eq!(
        runtime.inner.mutation_owner.capture()?.unwrap().state,
        accepted.state
    );
    assert!(!runtime
        .runtime_db()
        .audit_events()
        .has_event_by_id(&event.id)?);
    assert!(matches!(
        rx.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch("DROP TRIGGER reject_guarded_audit;")?;
        Ok(())
    })?;
    mutation.append_original_audit_event(event).await?;
    let published = rx.try_recv()?;
    let qualified = published
        .qualified
        .as_ref()
        .context("guarded Audit qualification")?;
    qualified.validate(
        &mutation.input_evidence_scope(),
        &runtime.inner.input_evidence_budget,
    )?;
    assert!(std::ptr::eq(qualified.event(), &*published.event));
    assert_eq!(&mutation.state, accepted.state.as_ref());
    Ok(())
}

#[tokio::test]
async fn independent_audit_turn_barriers_publish_original_qualified_notifications() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (_, session) = runtime.begin_task_birth_session().await?;
    let bus = crate::storage::EventBus::new(16);
    runtime.storage().enable_event_bus(bus.clone())?;
    let mut rx = bus.subscribe();
    let (turn_id, _) = runtime
        .begin_interactive_turn_with_provenance(None, None, None, None)
        .await?;
    let started = runtime
        .inner
        .mutation_owner
        .capture()?
        .unwrap()
        .state
        .clone();
    runtime
        .mark_current_turn_external_side_effect_possible(crate::tool::names::EXEC_COMMAND, None)
        .await?;
    let turn = runtime
        .runtime_db()
        .turn_records()
        .by_id(Some("default"), &turn_id)?
        .unwrap();
    assert!(turn.external_side_effect_possible);
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let captured = root
        .input_evidence
        .as_ref()
        .context("barrier Input domain")?
        .capture()?;
    let input = captured
        .lookup(
            &crate::runtime::resident_input_evidence::InputEvidenceKey::new(
                crate::runtime::resident_input_evidence::InputEvidenceFamily::Turn,
                &turn_id,
            ),
        )?
        .context("barrier qualified Turn")?;
    assert!(matches!(
        &input.record,
        crate::runtime::resident_input_evidence::PreparedInputEvidence::Turn(record)
            if record == &turn
    ));
    drop(captured);
    drop(root);
    runtime
        .mark_current_turn_external_side_effect_possible(crate::tool::names::EXEC_COMMAND, None)
        .await?;
    runtime
        .clear_current_turn_external_side_effect_possible(
            crate::tool::names::EXEC_COMMAND,
            &ToolExecutionStatus::Success,
        )
        .await?;
    let turn = runtime
        .runtime_db()
        .turn_records()
        .by_id(Some("default"), &turn_id)?
        .unwrap();
    assert!(!turn.external_side_effect_possible);
    assert_eq!(
        runtime.inner.mutation_owner.capture()?.unwrap().state,
        started
    );
    let scope = resident_input_evidence::InputEvidenceScope {
        agent_id: "default".into(),
        identity_incarnation: runtime.inner.identity_incarnation,
    };
    let mut kinds = Vec::new();
    while let Ok(published) = rx.try_recv() {
        let original = published
            .qualified
            .as_ref()
            .context("production Audit was raw")?;
        original.validate(&scope, &runtime.inner.input_evidence_budget)?;
        assert!(std::ptr::eq(original.event(), &*published.event));
        kinds.push(published.event.kind.clone());
    }
    assert_eq!(
        kinds,
        [
            "agent_state_mutated",
            "agent_state_changed",
            "external_command_no_replay_barrier",
            "external_command_checkpoint_durable",
        ]
    );
    drop(session);
    Ok(())
}

#[tokio::test]
async fn independent_audit_runtime_failure_publishes_before_failure_state() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let bus = crate::storage::EventBus::new(8);
    runtime.storage().enable_event_bus(bus.clone())?;
    let mut rx = bus.subscribe();
    let before = runtime
        .inner
        .mutation_owner
        .capture()?
        .unwrap()
        .state
        .clone();
    runtime
        .record_runtime_loop_failure(&anyhow!("original loop failure"))
        .await;
    let published = rx.try_recv()?;
    assert_eq!(published.event.kind, "agent_runtime_loop_failed");
    let qualified = published
        .qualified
        .as_ref()
        .context("runtime failure qualification")?;
    qualified.validate(
        &resident_input_evidence::InputEvidenceScope {
            agent_id: before.id.clone(),
            identity_incarnation: runtime.inner.identity_incarnation,
        },
        &runtime.inner.input_evidence_budget,
    )?;
    assert!(std::ptr::eq(qualified.event(), &*published.event));
    let after = runtime
        .inner
        .mutation_owner
        .capture()?
        .unwrap()
        .state
        .clone();
    assert!(before.last_runtime_failure.is_none());
    assert!(after.last_runtime_failure.is_some());
    assert_eq!(after.total_model_rounds, before.total_model_rounds);
    let persisted = rx.try_recv()?;
    assert_eq!(persisted.event.kind, "agent_state_mutated");
    let qualified = persisted
        .qualified
        .as_ref()
        .context("failure State ACK Audit was raw")?;
    qualified.validate(
        &resident_input_evidence::InputEvidenceScope {
            agent_id: after.id.clone(),
            identity_incarnation: runtime.inner.identity_incarnation,
        },
        &runtime.inner.input_evidence_budget,
    )?;
    assert!(std::ptr::eq(qualified.event(), &*persisted.event));
    assert!(persisted.event.event_seq > published.event.event_seq);
    assert!(matches!(
        rx.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    Ok(())
}

#[tokio::test]
async fn independent_audit_turn_barrier_keeps_turn_ack_when_later_audit_fails() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (_, _session) = runtime.begin_task_birth_session().await?;
    let (turn_id, _) = runtime
        .begin_interactive_turn_with_provenance(None, None, None, None)
        .await?;
    let accepted = runtime
        .inner
        .mutation_owner
        .capture()?
        .unwrap()
        .state
        .clone();
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch(
            "CREATE TRIGGER reject_barrier_audit BEFORE INSERT ON audit_events
             WHEN NEW.kind='external_command_no_replay_barrier'
             BEGIN SELECT RAISE(ABORT,'barrier Audit rejected'); END;",
        )?;
        Ok(())
    })?;
    let error = runtime
        .mark_current_turn_external_side_effect_possible(crate::tool::names::EXEC_COMMAND, None)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("barrier Audit rejected"),
        "{error:#}"
    );
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    assert_eq!(root.state, accepted);
    let inputs = root
        .input_evidence
        .as_ref()
        .context("barrier Input")?
        .capture()?;
    let record = inputs
        .lookup(
            &crate::runtime::resident_input_evidence::InputEvidenceKey::new(
                crate::runtime::resident_input_evidence::InputEvidenceFamily::Turn,
                &turn_id,
            ),
        )?
        .context("original Turn ACK")?;
    assert!(matches!(
        &record.record,
        crate::runtime::resident_input_evidence::PreparedInputEvidence::Turn(turn)
            if turn.external_side_effect_possible
    ));
    assert!(
        runtime
            .runtime_db()
            .turn_records()
            .by_id(Some("default"), &turn_id)?
            .unwrap()
            .external_side_effect_possible
    );
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch("DROP TRIGGER reject_barrier_audit")?;
        Ok(())
    })?;
    runtime
        .clear_current_turn_external_side_effect_possible(
            crate::tool::names::EXEC_COMMAND,
            &ToolExecutionStatus::Success,
        )
        .await?;
    assert!(
        !runtime
            .runtime_db()
            .turn_records()
            .by_id(Some("default"), &turn_id)?
            .unwrap()
            .external_side_effect_possible
    );
    Ok(())
}

#[tokio::test]
async fn tool_result_has_one_receipt_and_publishes_both_audits_with_the_barrier() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (_, _session) = runtime.begin_task_birth_session().await?;
    let (turn_id, _) = runtime
        .begin_interactive_turn_with_provenance(None, None, None, None)
        .await?;
    runtime
        .mark_current_turn_external_side_effect_possible(crate::tool::names::EXEC_COMMAND, None)
        .await?;
    let bus = crate::storage::EventBus::new(16);
    runtime.storage().enable_event_bus(bus.clone())?;
    let mut rx = bus.subscribe();
    let state = runtime
        .inner
        .mutation_owner
        .capture()?
        .unwrap()
        .state
        .clone();
    let record = ToolExecutionRecord {
        id: "settled-tool".into(),
        agent_id: "default".into(),
        work_item_id: None,
        turn_id: Some(turn_id.clone()),
        turn_index: state.turn_index,
        tool_name: crate::tool::names::EXEC_COMMAND.into(),
        created_at: Utc::now(),
        completed_at: Some(Utc::now()),
        duration_ms: 1,
        authority_class: AuthorityClass::RuntimeInstruction,
        status: ToolExecutionStatus::Success,
        input: serde_json::json!({}),
        output: serde_json::json!({}),
        summary: "done".into(),
        call_binding: None,
        invocation_surface: None,
    };
    let event = AuditEvent::legacy(
        "tool_executed",
        serde_json::json!({
            "agent_id":"default", "tool_execution_id":record.id,
        }),
    );
    let receipt_count = || -> Result<i64> {
        Ok(runtime.runtime_db().connection()?.query_row(
            "SELECT COUNT(*) FROM input_mutation_receipts",
            [],
            |row| row.get(0),
        )?)
    };
    let before = receipt_count()?;
    runtime.persist_tool_result_evidence(&record, event).await?;
    assert_eq!(receipt_count()? - before, 1);
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    assert_eq!(root.state, state);
    assert!(
        !runtime
            .runtime_db()
            .turn_records()
            .by_id(Some("default"), &turn_id)?
            .unwrap()
            .external_side_effect_possible
    );
    let captured = root.input_evidence.as_ref().unwrap().capture()?;
    let tool = captured.lookup(&resident_input_evidence::InputEvidenceKey::new(
        resident_input_evidence::InputEvidenceFamily::ToolExecution,
        &record.id,
    ))?;
    assert!(tool.is_some());
    let mut kinds = Vec::new();
    while let Ok(published) = rx.try_recv() {
        assert!(published.qualified.is_some());
        kinds.push(published.event.kind.clone());
    }
    assert_eq!(
        kinds,
        ["tool_executed", "external_command_checkpoint_durable"]
    );
    Ok(())
}

#[tokio::test]
async fn exec_command_settlement_retries_its_original_result_after_audit_rollback() -> Result<()> {
    assert_external_command_settlement_reuses_original_result(crate::tool::names::EXEC_COMMAND)
        .await
}

#[tokio::test]
async fn exec_command_batch_registers_once_and_reuses_its_grouped_result() -> Result<()> {
    assert_external_command_settlement_reuses_original_result(
        crate::tool::names::EXEC_COMMAND_BATCH,
    )
    .await
}

async fn assert_external_command_settlement_reuses_original_result(tool_name: &str) -> Result<()> {
    use crate::runtime_db::prepared_input_evidence::InputEvidenceWrite;
    use crate::tool::{spec::ToolExecutionContext, ToolCall};

    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (_, _session) = runtime.begin_task_birth_session().await?;
    let (turn_id, _) = runtime
        .begin_interactive_turn_with_provenance(None, None, None, None)
        .await?;
    let effect = runtime.workspace_root().join("settlement-effect");
    let command = serde_json::json!({
        "cmd": "printf x >> settlement-effect; printf original-result",
        "login": false,
    });
    let batch = tool_name == crate::tool::names::EXEC_COMMAND_BATCH;
    let call = ToolCall {
        id: "original-command-call".into(),
        name: tool_name.into(),
        input: if batch {
            serde_json::json!({"items": [command, {"cmd":"printf y >> settlement-effect","login":false}]})
        } else {
            command
        },
    };
    let assistant_round = crate::types::TranscriptEntry::new(
        "default",
        crate::types::TranscriptEntryKind::AssistantRound,
        Some(1),
        None,
        serde_json::json!({
            "turn_id": turn_id,
            "blocks": [{"type":"tool_use", "id":call.id, "name":call.name, "input":call.input}],
        }),
    );
    runtime
        .persist_transcript_evidence(&assistant_round)
        .await?;
    let context = ToolExecutionContext {
        turn_id: Some(turn_id.clone()),
        tool_call_id: Some(call.id.clone()),
        external_command_attempt: Some(Arc::new(crate::tool::spec::ExternalCommandAttempt {
            assistant_round_id: assistant_round.id,
            model_output_budget: 4096,
            submitted: Default::default(),
        })),
        ..Default::default()
    };
    let db = runtime.runtime_db();
    db.transaction(|tx| {
        tx.execute_batch(
            "CREATE TRIGGER reject_exec_command_begin BEFORE INSERT ON tool_executions
             WHEN json_extract(NEW.payload_json,'$.status')='pending'
             BEGIN SELECT RAISE(ABORT,'attempt registration unavailable'); END;",
        )?;
        Ok(())
    })?;
    let error = runtime
        .inner
        .tools
        .execute_with_context(
            runtime,
            "default",
            &AuthorityClass::OperatorInstruction,
            &call,
            &context,
        )
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("attempt registration unavailable"));
    assert!(
        !effect.exists(),
        "failed registration must prevent spawning"
    );
    db.transaction(|tx| {
        tx.execute_batch("DROP TRIGGER reject_exec_command_begin")?;
        Ok(())
    })?;
    let attempt = context.external_command_attempt.as_ref().unwrap();
    let context = ToolExecutionContext {
        external_command_attempt: Some(Arc::new(crate::tool::spec::ExternalCommandAttempt {
            assistant_round_id: attempt.assistant_round_id.clone(),
            model_output_budget: attempt.model_output_budget,
            submitted: Default::default(),
        })),
        ..context
    };
    let (result, record) = runtime
        .inner
        .tools
        .execute_with_context(
            runtime,
            "default",
            &AuthorityClass::OperatorInstruction,
            &call,
            &context,
        )
        .await?;
    assert!(!result.is_error());
    assert_eq!(
        std::fs::read(&effect)?,
        if batch { &b"xy"[..] } else { &b"x"[..] }
    );

    let prior = db.turn_records().by_id(Some("default"), &turn_id)?.unwrap();
    assert!(!prior.external_side_effect_possible);
    assert!(prior.external_side_effect_tool_call_id.is_none());
    let execution_id = record.id.clone();
    let pending = db
        .evidence()
        .tool_execution_by_id("default", &execution_id)?
        .unwrap();
    assert_eq!(pending.status, crate::types::ToolExecutionStatus::Pending);
    assert_eq!(pending.call_binding, record.call_binding);
    assert!(pending.output.is_null());
    db.read_for_preparation(|tx| {
        let count: u64 = tx.query_row(
            "SELECT COUNT(*) FROM tool_executions WHERE agent_id=?1 AND turn_id=?2",
            rusqlite::params!["default", turn_id],
            |row| row.get(0),
        )?;
        assert_eq!(count, 1);
        Ok(())
    })?;
    let original_output = record.output.clone();
    assert!(original_output.to_string().contains("original-result"));
    let event = AuditEvent::legacy(
        "tool_executed",
        serde_json::json!({
            "agent_id": "default",
            "tool_call_id": call.id,
            "tool_execution_id": record.id,
        }),
    );
    db.transaction(|tx| {
        tx.execute_batch(
            "CREATE TRIGGER reject_tool_settlement BEFORE INSERT ON audit_events
                          WHEN NEW.kind='tool_executed'
                          BEGIN SELECT RAISE(ABORT,'tool audit unavailable'); END;",
        )?;
        Ok(())
    })?;
    let error = runtime
        .persist_tool_result_evidence(&record, event.clone())
        .await
        .unwrap_err();
    let diagnostic = crate::runtime_error::describe_runtime_error(&error);
    assert_eq!(
        diagnostic.code,
        crate::runtime_error::TOOL_RESULT_SETTLEMENT_PENDING
    );
    assert_eq!(
        diagnostic.safe_context.get("status").map(String::as_str),
        Some("not_persisted")
    );
    assert!(!diagnostic.retryable);

    runtime
        .storage()
        .enable_event_bus(crate::storage::EventBus::new(16))?;
    let mut events = runtime
        .storage()
        .subscribe_events()?
        .context("tool result event bus")?;
    let mut mutation = runtime.begin_mutation().await?;
    let prepared = runtime.storage().prepare_input_evidence(
        mutation.input_evidence_scope(),
        "original-tool-settlement".into(),
        Utc::now(),
        InputEvidenceWrite::ToolResult {
            record,
            event: event.clone(),
            barrier: None,
        },
    )?;
    let descriptor = prepared.descriptor().clone();
    let mut settle = mutation.commit_prepared_evidence(
        prepared,
        Some(TransitionFaultPoint::ToolResultCommitAckLost),
    );
    assert!(futures_util::poll!(settle.as_mut()).is_pending());
    assert_eq!(
        db.turn_records().by_id(Some("default"), &turn_id)?,
        Some(prior)
    );
    assert!(!db.audit_events().has_event_by_id(&event.id)?);
    assert_eq!(
        db.evidence()
            .tool_execution_by_id("default", &execution_id)?,
        Some(pending),
    );
    db.transaction(|tx| {
        tx.execute_batch("DROP TRIGGER reject_tool_settlement")?;
        Ok(())
    })?;
    settle.await?;
    {
        let published = events.try_recv()?;
        assert_eq!(published.event.kind, "tool_executed");
        let qualified = published
            .qualified
            .context("recovered live Event qualification")?;
        assert_eq!(qualified.event(), &*published.event);
        qualified.validate(
            &mutation.input_evidence_scope(),
            &runtime.inner.input_evidence_budget,
        )?;
    }
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));

    let settled = db.turn_records().by_id(Some("default"), &turn_id)?.unwrap();
    assert!(!settled.external_side_effect_possible);
    assert!(settled.external_side_effect_tool_call_id.is_none());
    assert!(db.audit_events().has_event_by_id(&event.id)?);
    assert_eq!(
        db.evidence()
            .tool_execution_by_id("default", &execution_id)?
            .unwrap()
            .output,
        original_output
    );
    assert!(runtime.inner.mutation_owner.capture()?.is_some());
    db.read_for_preparation(|tx| {
        assert!(
            crate::runtime_db::input_mutation_receipts::lookup_input_mutation_receipt_tx(
                tx,
                &descriptor
            )?
            .is_some()
        );
        Ok(())
    })?;
    assert_eq!(
        std::fs::read(&effect)?,
        if batch { &b"xy"[..] } else { &b"x"[..] }
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn exec_command_spawn_failure_allows_correction_in_the_same_turn() -> Result<()> {
    use crate::provider::test_support::{ScriptedAgentProvider, ScriptedProviderStep};
    use crate::runtime::{tests::support::test_effective_prompt, turn::LoopControlOptions};
    use crate::tool::names::EXEC_COMMAND;

    let bad_command = |id, tty| {
        ScriptedProviderStep::tool_use(
            id,
            EXEC_COMMAND,
            serde_json::json!({
                "cmd": "printf x >> spawn-correction",
                "shell": "/definitely/not/a/real/shell",
                "login": false,
                "tty": tty,
            }),
        )
    };
    let provider = Arc::new(
        ScriptedAgentProvider::new([
            bad_command("missing-shell", false),
            bad_command("missing-pty-shell", true),
            ScriptedProviderStep::tool_use(
                "corrected-command",
                EXEC_COMMAND,
                serde_json::json!({"cmd": "printf x >> spawn-correction", "login": false}),
            ),
            ScriptedProviderStep::text("Corrected command completed."),
        ])
        .with_configured_model_refs(["default"]),
    );
    let harness = LifecycleHarness::with_provider(provider.clone());
    let runtime = harness.runtime();
    let effect = runtime.workspace_root().join("spawn-correction");

    let outcome = runtime
        .run_agent_loop(
            "default",
            AuthorityClass::OperatorInstruction,
            test_effective_prompt(),
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await?;
    assert_eq!(outcome.final_text, "Corrected command completed.");
    assert_eq!(provider.request_count(), 4);
    assert_eq!(std::fs::read(effect)?, b"x");

    let events = runtime.storage().read_recent_events(100)?;
    let completed = events
        .iter()
        .find(|event| {
            event.kind == "tool_executed" && event.data["tool_call_id"] == "corrected-command"
        })
        .context("corrected command did not execute")?;
    for id in ["missing-shell", "missing-pty-shell"] {
        let failed = events
            .iter()
            .find(|event| event.kind == "tool_execution_failed" && event.data["tool_call_id"] == id)
            .context("original spawn failure missing")?;
        assert_eq!(failed.data["error_kind"], "command_spawn_failed");
        assert_eq!(failed.data["turn_id"], completed.data["turn_id"]);
    }
    Ok(())
}

#[tokio::test]
async fn destructive_operation_checks_pending_registration_before_launch_and_keeps_its_fence(
) -> Result<()> {
    use crate::tool::{
        names::SCHEDULE_DESTRUCTIVE_OPERATION,
        spec::{ExternalCommandAttempt, ExternalCommandRegistrationFailed, ToolExecutionContext},
        ToolCall,
    };
    use crate::types::{TranscriptEntry, TranscriptEntryKind};
    use serde_json::json;

    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (_, _session) = runtime.begin_task_birth_session().await?;
    let (turn_id, _) = runtime
        .begin_interactive_turn_with_provenance(None, None, None, None)
        .await?;
    let call = ToolCall {
        id: "schedule-command".into(),
        name: SCHEDULE_DESTRUCTIVE_OPERATION.into(),
        input: json!({
            "operation_id":"schedule-fixture", "command":"printf fixture", "verification_target":"fixture",
        }),
    };
    let round = TranscriptEntry::new(
        "default",
        TranscriptEntryKind::AssistantRound,
        Some(1),
        None,
        json!({
            "turn_id":turn_id,
            "blocks":[{"type":"tool_use","id":call.id,"name":call.name,"input":call.input}],
        }),
    );
    runtime.persist_transcript_evidence(&round).await?;
    let context = || ToolExecutionContext {
        effective_work_item_id: Some("fixture-work".into()),
        turn_id: Some(turn_id.clone()),
        tool_call_id: Some(call.id.clone()),
        external_command_attempt: Some(Arc::new(ExternalCommandAttempt {
            assistant_round_id: round.id.clone(),
            model_output_budget: 4096,
            submitted: Default::default(),
        })),
        ..Default::default()
    };
    let db = runtime.runtime_db();
    db.transaction(|tx| {
        tx.execute_batch(
            "CREATE TRIGGER reject_destructive_attempt BEFORE INSERT ON tool_executions
             WHEN NEW.kind='ScheduleDestructiveOperation'
             BEGIN SELECT RAISE(ABORT, 'fixture rejects launcher registration'); END;",
        )?;
        Ok(())
    })?;
    let error = runtime
        .inner
        .tools
        .execute_with_context(
            runtime,
            "default",
            &AuthorityClass::OperatorInstruction,
            &call,
            &context(),
        )
        .await
        .unwrap_err();
    assert!(error.is::<ExternalCommandRegistrationFailed>(), "{error:#}");
    assert!(
        format!("{error:#}").contains("fixture rejects launcher registration"),
        "{error:#}"
    );
    let operation = db
        .destructive_operations()
        .by_id("schedule-fixture")?
        .unwrap();
    assert_eq!(
        operation.phase,
        crate::runtime_db::DestructiveOperationPhase::Scheduled
    );
    let (result, record) = runtime
        .inner
        .tools
        .execute_with_context(
            runtime,
            "default",
            &AuthorityClass::OperatorInstruction,
            &call,
            &context(),
        )
        .await?;
    assert_eq!(result.envelope.result.unwrap()["already_scheduled"], true);
    assert!(record.call_binding.is_none());
    let turn = db.turn_records().by_id(Some("default"), &turn_id)?.unwrap();
    assert!(!turn.external_side_effect_possible);
    assert_eq!(
        turn.destructive_operation_id.as_deref(),
        Some("schedule-fixture")
    );
    Ok(())
}
