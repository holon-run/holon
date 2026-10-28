use super::*;
use crate::{
    runtime::mutation_owner::{KnownCommit, MutationReservation},
    runtime_db::current_read_writes::{CurrentFamily, CurrentRecord},
};

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
    runtime.append_original_audit_event(event.clone()).await?;
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
        .mark_current_turn_external_side_effect_possible(crate::tool::names::EXEC_COMMAND)
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
        .mark_current_turn_external_side_effect_possible(crate::tool::names::EXEC_COMMAND)
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
        .mark_current_turn_external_side_effect_possible(crate::tool::names::EXEC_COMMAND)
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
