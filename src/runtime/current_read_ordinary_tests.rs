//! Actual ordinary acquisition uses its original publication, never SQL recovery.
use super::*;
use crate::runtime::{
    current_read_facts::{CurrentReadError, CurrentReadFact},
    mutation_owner::{MutationOwnerError, MutationReservation},
    tests::support::{context_config, tempdir},
};
use crate::runtime_db::transitions::{
    AgentPostureTransitionCommand, AgentStateMutation, PreparedInputTransition,
    TransitionFaultPoint,
};

async fn fixture() -> Result<(tempfile::TempDir, RuntimeHandle)> {
    let home = tempdir()?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "ordinary-current-owner",
        home.path().into(),
        context_config(),
    )?;
    runtime.refresh_context_resources().await?;
    Ok((home, runtime))
}
fn posture(
    runtime: &RuntimeHandle,
    root: &super::super::resident_state::AgentSnapshot,
    nonce: &str,
    event: bool,
    fault: Option<TransitionFaultPoint>,
) -> Result<PreparedInputTransition> {
    let mut after = root.state.as_ref().clone();
    after.last_wake_reason = Some(nonce.into());
    PreparedInputTransition::posture(
        super::super::resident_input_evidence::InputEvidenceScope {
            agent_id: runtime.instance_agent_id().into(),
            identity_incarnation: root.token.identity_incarnation,
        },
        nonce.into(),
        runtime.now(),
        AgentPostureTransitionCommand {
            agent_id: runtime.instance_agent_id().into(),
            expected_incarnation: Some(root.token.identity_incarnation),
            agent_state: AgentStateMutation {
                expected: Some(Box::new(root.state.as_ref().clone())),
                record: Box::new(after),
            },
            audit_events: if event {
                vec![AuditEvent::legacy(
                    "runtime_error",
                    serde_json::json!({"actual":"ordinary-source"}),
                )]
            } else {
                Vec::new()
            },
            fault,
        },
        None,
    )
}
fn tables(runtime: &RuntimeHandle, forbidden: bool) -> Result<()> {
    let names = [
        "agent_identities",
        "agent_states",
        "queue_entries",
        "audit_events",
        "work_items",
        "wait_conditions",
        "work_item_continuations",
        "tasks",
        "task_result_settlements",
        "messages",
        "turn_records",
        "briefs",
        "transcript_entries",
        "tool_executions",
        "timers",
        "timer_wakes",
        "external_triggers",
    ];
    let connection = runtime.runtime_db().connection()?;
    for name in if forbidden {
        names.into_iter().collect::<Vec<_>>()
    } else {
        names.into_iter().rev().collect()
    } {
        let (from, to) = if forbidden {
            (name.into(), format!("ordinary_forbidden_{name}"))
        } else {
            (format!("ordinary_forbidden_{name}"), name.into())
        };
        connection.execute_batch(&format!("ALTER TABLE {from} RENAME TO {to}"))?;
    }
    Ok(())
}

#[tokio::test]
async fn ordinary_scheduler_summary_and_skills_capture_without_original_body_or_identity_sql(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let message = MessageEnvelope::new(
        runtime.instance_agent_id(),
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "actual ordinary Queue ACK".into(),
        },
    );
    runtime.enqueue(message).await?;
    let mut mutation = runtime.begin_mutation().await?;
    let root = mutation.capture_published_root()?;
    mutation
        .commit_prepared_input_transition(posture(
            &runtime,
            &root,
            "ordinary-real-audit",
            true,
            None,
        )?)
        .await?;
    drop((mutation, root));
    let _pause = runtime.pause_resource_observer_for_test().await;
    // Warm the same immutable source before measuring temporary holders.
    drop(runtime.capture_summary_projection().await?);
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let token = root.token;
    let expected_events = root
        .current_reads
        .as_ref()
        .unwrap()
        .coverage()
        .events
        .len()
        .min(100);
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    tables(&runtime, true)?;
    let read = runtime.capture_scheduler_projection().await?;
    assert_eq!(read.projection.queue_len, 1);
    assert!(read.current.has_queued());
    assert!(read.current.events.iter().any(|r| matches!(&r.fact,
        CurrentReadFact::Event(event) if event.kind == "runtime_error")));
    let weak = Arc::downgrade(&read._owned_budget);
    let SummaryProjectionRead {
        read: _read,
        resources,
        owned,
        trigger_prior: _prior,
        identity,
        model,
        model_material,
    } = runtime.capture_summary_projection().await?;
    assert_eq!(_read.current.recent_event_count, expected_events);
    assert_eq!(identity.agent_id, runtime.instance_agent_id());
    let skills = runtime.capture_agent_skills_read().await?;
    let (view, skills_material) = skills.into_parts();
    let wire = serde_json::to_value(&view)?;
    drop((
        wire,
        view,
        skills_material,
        read,
        _read,
        resources,
        owned,
        identity,
        model,
        model_material,
    ));
    assert!(weak.upgrade().is_none());
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    assert_eq!(
        runtime.inner.mutation_owner.capture()?.unwrap().token,
        token
    );
    tables(&runtime, false)?;
    Ok(())
}

#[tokio::test]
async fn ordinary_capture_does_not_reconcile_known_ack_or_replace_its_original_pending_ticket(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let mut mutation = runtime.begin_mutation().await?;
    let root = mutation.capture_published_root()?;
    let command = posture(
        &runtime,
        &root,
        "ordinary-known-actual-ack",
        false,
        Some(TransitionFaultPoint::BeforeCacheUpdate),
    )?;
    assert!(mutation
        .commit_prepared_input_transition(command)
        .await
        .is_err());
    drop((mutation, root));
    let owner = runtime.inner.mutation_owner.reserve().await?;
    let before = owner.recovery()?.context("actual Known recovery")?;
    assert!(before.known.is_some());
    let key = before.key.clone();
    drop((before, owner));
    tables(&runtime, true)?;
    for error in [
        runtime.capture_scheduler_projection().await.err(),
        runtime.capture_summary_projection().await.err(),
        runtime.capture_agent_skills_read().await.err(),
    ] {
        let error = error.context("ordinary read silently recovered Known ACK")?;
        assert!(
            matches!(error.downcast_ref::<MutationOwnerError>(), Some(MutationOwnerError::NeedsReconcile(actual)) if actual==&key),
            "{error:#}"
        );
    }
    let owner = runtime.inner.mutation_owner.reserve().await?;
    let after = owner.recovery()?.unwrap();
    assert_eq!(after.key, key);
    assert!(after.known.is_some());
    drop((after, owner));
    tables(&runtime, false)?;
    // Explicit original mutation acquisition remains the recovery boundary.
    drop(runtime.begin_mutation().await?);
    assert_eq!(
        runtime
            .capture_scheduler_projection()
            .await?
            .state
            .last_wake_reason
            .as_deref(),
        Some("ordinary-known-actual-ack")
    );
    Ok(())
}

#[tokio::test]
async fn ordinary_unprepared_current_model_is_typed_without_hidden_named_preparation() -> Result<()>
{
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let mut owner = runtime.inner.mutation_owner.reserve().await?;
    owner.begin("test-only-original-current-none".into())?;
    // A deliberate publication model: all other original domains keep actual
    // Arcs; Current None cannot be interpreted as empty or hydrated by a read.
    owner.committed_prepared_with_current_read_domains(
        root.state.clone(),
        root.episodes.clone(),
        root.input_evidence.clone(),
        root.work.clone(),
        root.tasks.clone(),
        root.scheduling.clone(),
        None,
    )?;
    MutationReservation::publish_all_known(std::slice::from_mut(&mut owner))?;
    drop(owner);
    tables(&runtime, true)?;
    let error = runtime
        .capture_scheduler_projection()
        .await
        .err()
        .context("Current None became ready")?;
    assert!(
        matches!(
            error.downcast_ref::<CurrentReadError>(),
            Some(CurrentReadError::NeedsPreparation)
        ),
        "{error:#}"
    );
    let current = runtime.inner.mutation_owner.capture()?.unwrap();
    assert!(current.current_reads.is_none());
    assert!(Arc::ptr_eq(
        current.work.as_ref().unwrap(),
        root.work.as_ref().unwrap()
    ));
    tables(&runtime, false)?;
    Ok(())
}
