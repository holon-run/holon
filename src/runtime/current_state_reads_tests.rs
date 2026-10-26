use super::*;
use crate::{provider::ProviderGenerateImageRequest, runtime::tests::support::LifecycleHarness};

#[path = "current_audit_tests.rs"]
mod current_audit_tests;
#[path = "current_lifecycle_audit_tests.rs"]
mod current_lifecycle_audit_tests;
#[path = "current_writer_audit_tests.rs"]
mod current_writer_audit_tests;

fn forbid_state_storage(runtime: &RuntimeHandle) -> Result<()> {
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch(
            "ALTER TABLE agent_states RENAME TO current_state_storage_forbidden;
             ALTER TABLE agent_identities RENAME TO current_identity_storage_forbidden;",
        )?;
        Ok(())
    })
}

#[tokio::test]
async fn current_queue_body_publishes_with_state_and_does_not_pin_old_root() -> Result<()> {
    use crate::runtime_db::current_read_writes::{CurrentFamily, CurrentRecord};
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let message = |text: &str| {
        MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text { text: text.into() },
        )
    };
    let first = message("original Current queue");
    runtime.enqueue(first.clone()).await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let current = root.current.as_ref().context("original Current ACK")?;
    assert_eq!(current.scope.agent_id, root.state.id);
    assert_eq!(
        current.scope.identity_incarnation,
        root.token.identity_incarnation
    );
    current.validate_binding(&current.scope, &runtime.inner.input_evidence_budget)?;
    let selected = current
        .record(&(CurrentFamily::Queue, first.id.clone()))
        .unwrap()
        .clone();
    let weak_root = Arc::downgrade(&root);
    let weak_ack = Arc::downgrade(current);
    let guard = selected.weak_material();
    let state = root.state.clone();
    drop(root);
    let second = message("next Current cut");
    runtime.enqueue(second.clone()).await?;
    assert!(weak_root.upgrade().is_none());
    assert!(weak_ack.upgrade().is_none());
    let next = runtime.inner.mutation_owner.capture()?.unwrap();
    assert_eq!(next.state.pending, state.pending + 1);
    let current = next.current.as_ref().context("next original Current ACK")?;
    assert!(current.record(&(CurrentFamily::Queue, second.id)).is_some());
    // Sparse ACK publication never claims the preceding address is absent.
    assert!(!current
        .facts()
        .contains_key(&(CurrentFamily::Queue, first.id.clone())));
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch(
            "ALTER TABLE queue_entries RENAME TO current_queue_body_forbidden;
             ALTER TABLE audit_events RENAME TO current_event_body_forbidden;",
        )?;
        Ok(())
    })?;
    let _agent = runtime.inner.agent.lock().await;
    let CurrentRecord::Queue(record) = selected.record() else {
        panic!("expected original Queue body");
    };
    assert_eq!(record.message_id, first.id);
    assert!(guard.upgrade().is_some());
    drop(selected);
    assert!(guard.upgrade().is_none());
    Ok(())
}

#[tokio::test]
async fn current_original_body_survives_known_install_recovery_without_readback() -> Result<()> {
    use crate::runtime::mutation_owner::{KnownCommit, MutationReservation};
    use crate::runtime_db::current_read_writes::CurrentFamily;
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "known Current original".into(),
        },
    );
    runtime.inject_next_transition_fault(TransitionFaultPoint::BeforeCacheUpdate);
    let error = runtime.enqueue(message.clone()).await.unwrap_err();
    assert!(format!("{error:#}").contains("committed but publication"));
    let mut reservations = vec![runtime.inner.mutation_owner.reserve().await?];
    let known = MutationReservation::validate_group_known(&reservations)?;
    let saved = known[0].as_ref().context("known original commit")?;
    let KnownCommit::State {
        runtime: Some(facts),
        ..
    } = saved
    else {
        panic!("missing original installation facts");
    };
    let original = facts.current.as_ref().context("known Current ACK")?.clone();
    assert!(facts.external_effects().current_read_writes.is_none());
    runtime
        .install_saved_mutation(&reservations[0], saved)
        .await?;
    MutationReservation::reconcile_all(&mut reservations, known)?;
    drop(reservations);
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let installed = root.current.as_ref().context("installed Current ACK")?;
    assert!(Arc::ptr_eq(&original, installed));
    assert!(installed
        .record(&(CurrentFamily::Queue, message.id))
        .is_some());
    assert_eq!(root.state.pending, 1);
    Ok(())
}

#[tokio::test]
async fn captured_state_is_storage_free_without_the_mutable_agent_lock() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    forbid_state_storage(runtime)?;
    let _agent = runtime.inner.agent.lock().await;
    let state = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        runtime.capture_committed_state(),
    )
    .await??;
    assert!(Arc::ptr_eq(&state, &root.state));
    Ok(())
}

#[tokio::test]
async fn captured_state_keeps_its_cut_without_retaining_the_old_root() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let weak_root = Arc::downgrade(&root);
    let before = runtime.capture_committed_state().await?;
    let expected = before.as_ref().clone();
    assert!(Arc::ptr_eq(&before, &root.state));
    drop(root);
    runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "publish another State cut".into(),
            },
        ))
        .await?;
    assert!(weak_root.upgrade().is_none());
    assert_eq!(*before, expected);
    let after = runtime.capture_committed_state().await?;
    assert!(!Arc::ptr_eq(&before, &after));
    assert_eq!(after.total_message_count, before.total_message_count + 1);
    Ok(())
}

#[tokio::test]
async fn captured_state_waits_for_the_original_unknown_attempt_instead_of_borrowing_sql(
) -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let before = runtime.capture_committed_state().await?;
    let mut owner = runtime.inner.mutation_owner.reserve().await?;
    owner.begin("current-state-unsettled".into())?;
    forbid_state_storage(runtime)?;
    tokio::select! {
        biased;
        result = runtime.capture_committed_state() => {
            anyhow::bail!("State escaped the unresolved original owner: {result:?}");
        }
        _ = std::future::ready(()) => {}
    }
    owner.definitely_not_committed()?;
    drop(owner);
    let after = runtime.capture_committed_state().await?;
    assert!(Arc::ptr_eq(&before, &after));
    Ok(())
}

#[tokio::test]
async fn generate_image_route_uses_captured_state_without_sql_or_mutable_agent_lock() -> Result<()>
{
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    forbid_state_storage(runtime)?;
    let _agent = runtime.inner.agent.lock().await;
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        runtime.generate_image(ProviderGenerateImageRequest {
            material: Default::default(),
            prompt: "no provider call".into(),
            size: None,
            background: None,
            output_format: None,
        }),
    )
    .await?;
    let error = result.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("no configured model supports image_generation"),
        "{error:#}"
    );
    Ok(())
}
