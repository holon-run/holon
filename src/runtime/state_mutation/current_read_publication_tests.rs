//! Runtime publication uses actual CurrentRead ACK facts, never a later SQL view.
use super::*;
use crate::runtime::{
    current_read_facts::{CurrentReadRequest, CURRENT_READ_MAX_BYTES, CURRENT_READ_MAX_RECORDS},
    mutation_owner::MutationOwnerError,
    tests::support::{context_config, CountingProvider},
};
use crate::runtime_db::transitions::{
    AgentPostureTransitionCommand, AgentStateMutation, PreparedInputTransition,
};

fn request() -> CurrentReadRequest {
    CurrentReadRequest::current(CURRENT_READ_MAX_BYTES, CURRENT_READ_MAX_RECORDS)
}
fn runtime() -> Result<(RuntimeHandle, tempfile::TempDir, tempfile::TempDir)> {
    let home = tempfile::tempdir()?;
    let workspace = tempfile::tempdir()?;
    let runtime = RuntimeHandle::new(
        "default",
        home.path().to_owned(),
        workspace.path().to_owned(),
        "http://127.0.0.1:7878".into(),
        Arc::new(CountingProvider {
            calls: Mutex::new(0),
            reply: "unused",
        }),
        "default".into(),
        context_config(),
    )?;
    Ok((runtime, home, workspace))
}
fn state_only(
    mutation: &RuntimeMutation,
    nonce: &str,
    fault: Option<TransitionFaultPoint>,
) -> Result<PreparedInputTransition> {
    let mut after = mutation.base.state.clone();
    after.last_wake_reason = Some(nonce.into());
    PreparedInputTransition::posture(
        mutation.input_evidence_scope(),
        nonce.into(),
        "2026-10-14T00:00:00.123456789Z".parse()?,
        AgentPostureTransitionCommand {
            agent_id: after.id.clone(),
            expected_incarnation: Some(mutation.runtime.inner.identity_incarnation),
            agent_state: AgentStateMutation {
                expected: Some(Box::new(mutation.base.last_persisted_state.clone())),
                record: Box::new(after),
            },
            audit_events: Vec::new(),
            fault,
        },
        None,
    )
}

#[tokio::test]
async fn bootstrap_and_actual_posture_publish_same_view_current_read_facts() -> Result<()> {
    let (runtime, _home, _workspace) = runtime()?;
    let first = runtime.inner.mutation_owner.capture()?.unwrap();
    let current = first
        .current_reads
        .as_ref()
        .context("bootstrap must prepare CurrentRead")?;
    current.validate_state(&first.state)?;
    current.validate_identity(first.active_identity()?)?;
    assert_eq!(
        current.scope().identity_incarnation,
        first.token.identity_incarnation
    );
    let initial_revision = current.coverage().revision;
    let mut mutation = runtime.begin_mutation().await?;
    mutation.candidate.state.last_wake_reason = Some("actual-posture-current-read".into());
    mutation.persist_state(runtime.storage()).await?;
    drop(mutation);
    let published = runtime.inner.mutation_owner.capture()?.unwrap();
    let current = published.current_reads.as_ref().unwrap();
    current.validate_state(&published.state)?;
    current.validate_identity(published.active_identity()?)?;
    assert_eq!(current.coverage().revision, initial_revision + 1);
    let capture = current.capture(&request())?;
    assert!(capture.events.iter().any(|event| match &event.fact {
        crate::runtime::current_read_facts::CurrentReadFact::Event(event) =>
            event.kind == "agent_state_mutated",
        _ => false,
    }));
    Ok(())
}

#[tokio::test]
async fn state_only_runtime_ack_shares_original_event_and_has_no_old_root_chain() -> Result<()> {
    let (runtime, _home, _workspace) = runtime()?;
    let mut seed = runtime.begin_mutation().await?;
    seed.candidate.state.last_wake_reason = Some("seed-original-event".into());
    seed.persist_state(runtime.storage()).await?;
    drop(seed);
    let old = runtime.inner.mutation_owner.capture()?.unwrap();
    let source = old.current_reads.as_ref().unwrap().clone();
    let source_weak = Arc::downgrade(&source);
    let selected = source.capture(&request())?;
    let event = selected
        .events
        .last()
        .context("actual seed Audit event")?
        .clone();
    let revision = source.coverage().revision;
    let mut mutation = runtime.begin_mutation().await?;
    let prepared = state_only(&mutation, "pure-runtime-state-only", None)?;
    mutation.commit_prepared_input_transition(prepared).await?;
    drop(mutation);
    let published = runtime.inner.mutation_owner.capture()?.unwrap();
    let current = published.current_reads.as_ref().unwrap();
    assert_eq!(current.coverage().revision, revision + 1);
    let next = current.capture(&request())?;
    assert!(next.events.iter().any(|value| Arc::ptr_eq(value, &event)));
    drop((source, old, selected));
    assert!(
        source_weak.upgrade().is_none(),
        "new facts cannot retain their source domain/root"
    );
    // Ordinary immutable capture remains usable with every original body source disabled.
    runtime.inner.runtime_db.connection()?.execute_batch(
        "ALTER TABLE queue_entries RENAME TO current_queue_read_forbidden;
         ALTER TABLE audit_events RENAME TO current_events_read_forbidden;
         ALTER TABLE agent_states RENAME TO current_state_read_forbidden;
         ALTER TABLE agent_identities RENAME TO current_identity_read_forbidden;",
    )?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let captured = root.current_reads.as_ref().unwrap().capture(&request())?;
    assert!(captured
        .events
        .iter()
        .any(|value| Arc::ptr_eq(value, &event)));
    root.current_reads
        .as_ref()
        .unwrap()
        .validate_identity(root.active_identity()?)?;
    Ok(())
}

#[tokio::test]
async fn known_state_only_ack_recovers_original_current_arc_without_body_sql() -> Result<()> {
    let (runtime, _home, _workspace) = runtime()?;
    let mut mutation = runtime.begin_mutation().await?;
    let prepared = state_only(
        &mutation,
        "state-only-known-fault",
        Some(TransitionFaultPoint::BeforeCacheUpdate),
    )?;
    assert!(mutation
        .commit_prepared_input_transition(prepared)
        .await
        .is_err());
    drop(mutation);
    assert!(matches!(
        runtime.inner.mutation_owner.capture(),
        Err(MutationOwnerError::NeedsReconcile(_))
    ));
    let reservation = runtime.inner.mutation_owner.reserve().await?;
    let saved = reservation.recovery()?.unwrap().known.unwrap();
    let KnownCommit::State {
        current_reads: Some(actual),
        state,
        ..
    } = &saved
    else {
        panic!("actual ACK retains CurrentRead facts")
    };
    actual.validate_state(state)?;
    let actual = actual.clone();
    drop(reservation);
    runtime.inner.runtime_db.connection()?.execute_batch(
        "ALTER TABLE queue_entries RENAME TO known_current_queue_forbidden;
         ALTER TABLE audit_events RENAME TO known_current_events_forbidden;",
    )?;
    let recovered = runtime.begin_mutation().await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    assert!(Arc::ptr_eq(root.current_reads.as_ref().unwrap(), &actual));
    assert_eq!(
        root.state.last_wake_reason.as_deref(),
        Some("state-only-known-fault")
    );
    drop(recovered);
    Ok(())
}

#[tokio::test]
async fn current_facts_cannot_publish_with_different_actual_identity_or_state() -> Result<()> {
    let (runtime, _home, _workspace) = runtime()?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let current = root.current_reads.as_ref().unwrap().clone();
    let mut wrong_state = root.state.as_ref().clone();
    wrong_state.total_message_count += 1;
    let mut reservation = runtime.inner.mutation_owner.reserve().await?;
    reservation.begin("reject-wrong-current-state".into())?;
    assert!(matches!(
        reservation.committed_prepared_with_current_read_domains(
            Arc::new(wrong_state),
            None,
            None,
            None,
            None,
            None,
            Some(current.clone())
        ),
        Err(MutationOwnerError::TicketMismatch)
    ));
    reservation.definitely_not_committed()?;
    let mut wrong_identity = root.active_identity()?.as_ref().clone();
    wrong_identity.visibility = crate::types::AgentVisibility::Private;
    let wrong_identity = Arc::new(IdentityMutationFacts::budgeted(
        Some(&wrong_identity),
        Some(root.state.clone()),
        &runtime.inner.input_evidence_budget,
    )?);
    // Only this test supplies metadata. The publication validator must still
    // reject a CurrentRead digest from a different actual identity allocation.
    reservation.begin("reject-wrong-current-identity".into())?;
    let facts = RuntimeMutationFacts::for_cold_external_input(
        reservation.owner_epoch(),
        root.state.clone(),
        None,
        None,
    );
    let mut facts = facts;
    facts.effects.agent_identities = vec![wrong_identity.identity.as_deref().unwrap().clone()];
    facts.identity_publication = Some(wrong_identity);
    let known = KnownCommit::State {
        state: root.state.clone(),
        episodes: None,
        input_evidence: None,
        work: None,
        tasks: None,
        scheduling: None,
        current_reads: Some(current),
        runtime: Some(Arc::new(facts)),
    };
    assert!(matches!(
        MutationReservation::committed_all(std::slice::from_mut(&mut reservation), vec![known]),
        Err(MutationOwnerError::TicketMismatch)
    ));
    reservation.definitely_not_committed()?;
    assert!(Arc::ptr_eq(
        &runtime.inner.mutation_owner.capture()?.unwrap(),
        &root
    ));
    Ok(())
}

#[tokio::test]
async fn original_input_writers_keep_live_event_role_across_70_births_and_release_after_last_consumer(
) -> Result<()> {
    use crate::runtime::current_read_facts::CurrentReadFact;
    let (runtime, _home, _workspace) = runtime()?;
    let reservation = runtime.inner.mutation_owner.reserve().await?;
    let original = reservation
        .capture_ready_snapshot()?
        .context("actual Ready root")?;
    let session =
        runtime.register_current_event_birth_session_from_snapshot(&reservation, &original)?;
    let cursor = session.cursor;
    drop((reservation, original));
    for batch in 0..7 {
        let mut mutation = runtime.begin_mutation().await?;
        let mut after = mutation.base.last_persisted_state.clone();
        after.last_wake_reason = Some(format!("actual-query-batch-{batch}"));
        let events = (0..10)
            .map(|index| {
                let mut event = AuditEvent::legacy(
                    "actual_query_role",
                    serde_json::json!({"batch":batch,"index":index}),
                );
                event.id = format!("actual-query-event-{batch}-{index}");
                event
            })
            .collect::<Vec<_>>();
        let prepared = PreparedInputTransition::posture(
            mutation.input_evidence_scope(),
            format!("actual-query-command-{batch}"),
            Utc::now(),
            AgentPostureTransitionCommand {
                agent_id: after.id.clone(),
                expected_incarnation: Some(runtime.inner.identity_incarnation),
                agent_state: AgentStateMutation {
                    expected: Some(Box::new(mutation.base.last_persisted_state.clone())),
                    record: Box::new(after),
                },
                audit_events: events,
                fault: None,
            },
            None,
        )?;
        mutation.commit_prepared_input_transition(prepared).await?;
    }
    let root = runtime
        .inner
        .mutation_owner
        .capture()?
        .context("actual query publication")?;
    let current = root
        .current_reads
        .as_ref()
        .context("actual Current facts")?;
    assert_eq!(current.coverage().event_query.birth_after, Some(cursor));
    assert_eq!(current.coverage().event_query.born_event_seqs.len(), 70);
    assert_eq!(current.capture(&request())?.events.len(), 64);
    let selected = runtime.capture_current_event_birth_session_from_snapshot(&session, &root)?;
    assert_eq!(selected.selected.events.len(), 70);
    assert!(selected
        .selected
        .events
        .iter()
        .all(|r| matches!(&r.fact,CurrentReadFact::Event(e) if e.kind=="actual_query_role")));
    let oldest = Arc::downgrade(&selected.selected.events[0]);
    let source = Arc::downgrade(current);
    drop((root, session));
    // Selected output, rather than the registry's Weak slot, still owns the lease.
    let mut mutation = runtime.begin_mutation().await?;
    let prepared = state_only(&mutation, "held-query-state-only", None)?;
    mutation.commit_prepared_input_transition(prepared).await?;
    drop(mutation);
    let held = runtime.inner.mutation_owner.capture()?.unwrap();
    assert_eq!(
        held.current_reads
            .as_ref()
            .unwrap()
            .coverage()
            .event_query
            .born_event_seqs
            .len(),
        70
    );
    drop(held);
    assert!(
        source.upgrade().is_none(),
        "the next publication cannot retain its old Root/source"
    );
    drop(selected);
    let mut mutation = runtime.begin_mutation().await?;
    let prepared = state_only(&mutation, "released-query-state-only", None)?;
    mutation.commit_prepared_input_transition(prepared).await?;
    drop(mutation);
    let released = runtime.inner.mutation_owner.capture()?.unwrap();
    let current = released.current_reads.as_ref().unwrap();
    assert!(current.coverage().event_query.birth_after.is_none());
    assert!(current.coverage().event_query.born_event_seqs.is_empty());
    assert!(
        oldest.upgrade().is_none(),
        "header-only Event body must release with last consumer"
    );
    runtime.inner.runtime_db.connection()?.execute_batch("ALTER TABLE audit_events RENAME TO role_read_events_forbidden; ALTER TABLE queue_entries RENAME TO role_read_queue_forbidden")?;
    assert_eq!(current.capture(&request())?.events.len(), 64);
    Ok(())
}

#[tokio::test]
async fn actual_unknown_input_and_named_preparation_keep_the_original_live_event_role() -> Result<()>
{
    use crate::runtime::resident_input_evidence::InputEvidenceScope;
    use crate::runtime_db::{input_evidence_snapshot::InputEvidenceLoadRequest, input_preparation};
    let (runtime, _home, _workspace) = runtime()?;
    let reservation = runtime.inner.mutation_owner.reserve().await?;
    let original = reservation.capture_ready_snapshot()?.unwrap();
    let session =
        runtime.register_current_event_birth_session_from_snapshot(&reservation, &original)?;
    drop((reservation, original));
    let mut mutation = runtime.begin_mutation().await?;
    let mut after = mutation.base.last_persisted_state.clone();
    after.last_wake_reason = Some("original-event-recovery".into());
    let events = (0..70)
        .map(|index| {
            let mut event = AuditEvent::legacy(
                "original_event_recovery",
                serde_json::json!({"index":index}),
            );
            event.id = format!("original-event-recovery-{index:03}");
            event
        })
        .collect();
    let prepared = PreparedInputTransition::posture(
        mutation.input_evidence_scope(),
        "original-event-recovery-command".into(),
        Utc::now(),
        AgentPostureTransitionCommand {
            agent_id: after.id.clone(),
            expected_incarnation: Some(runtime.inner.identity_incarnation),
            agent_state: AgentStateMutation {
                expected: Some(Box::new(mutation.base.last_persisted_state.clone())),
                record: Box::new(after),
            },
            audit_events: events,
            fault: None,
        },
        None,
    )?;
    let reservation = &mutation.reservations[mutation.primary_index];
    let scope = mutation.input_evidence_scope();
    let ledger = &runtime.inner.input_evidence_budget;
    let roles = reservation
        .current_event_roles(
            &scope,
            ledger,
            (CURRENT_READ_MAX_BYTES, CURRENT_READ_MAX_RECORDS),
        )?
        .unwrap();
    let prepared = prepared
        .with_current_event_roles(roles, reservation.owner_epoch(), ledger)?
        .with_current_read_source(mutation.current_reads.as_ref().unwrap().clone())?;
    let witness = crate::runtime::mutation_witness::MutationWitness::input_mutation(
        prepared.descriptor().command_nonce().into(),
        crate::runtime_db::input_mutation_receipts::OriginalInputMutationAttempt::new(
            prepared.descriptor().clone(),
        ),
        Vec::new(),
    )?
    .with_input_runtime(vec![Arc::new(mutation.facts(Default::default())?)])?;
    let work = mutation.owner_work_load_request()?;
    let (tasks, _task_request_guard) = mutation.owner_task_load_request()?;
    let error = mutation
        .commit_transition_inner(
            prepared.descriptor().command_nonce().into(),
            Some(witness),
            || {
                let actual = runtime
                    .runtime_db()
                    .transitions_in_scope(&scope)
                    .commit_prepared_transition_with_work_request_and_memory_budget(
                        &prepared, &work, ledger, &tasks,
                    )?;
                assert!(
                    matches!(
                        actual,
                        crate::runtime_db::transitions::InputMutationExecution::Committed { .. }
                    ),
                    "original transaction must really COMMIT before its ACK is lost"
                );
                Err(anyhow::anyhow!(
                    "original Event role ACK deliberately lost after COMMIT"
                ))
            },
        )
        .await
        .err()
        .context("actual original ACK was not lost")?;
    assert!(format!("{error:#}").contains("deliberately lost after COMMIT"));
    drop(mutation);
    let pending = runtime.inner.mutation_owner.reserve().await?;
    let recovery = pending
        .recovery()?
        .context("actual lost ACK did not retain its original gate")?;
    assert!(
        recovery.known.is_none(),
        "this test must exercise original unknown receipt recovery"
    );
    drop((pending, recovery));
    let recovered = runtime.begin_mutation().await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    assert_eq!(
        root.state.last_wake_reason.as_deref(),
        Some("original-event-recovery")
    );
    assert_eq!(
        runtime
            .capture_current_event_birth_session_from_snapshot(&session, &root)?
            .selected
            .events
            .len(),
        70
    );
    let reservation = &recovered.reservations[recovered.primary_index];
    let scope = InputEvidenceScope {
        agent_id: root.state.id.clone(),
        identity_incarnation: root.token.identity_incarnation,
    };
    let policy = InputEvidenceLoadRequest {
        mandatory_roots: Default::default(),
        interrupted_sources: Default::default(),
        selected_completion_brief_ids: Default::default(),
        windows: Vec::new(),
        byte_limit: CURRENT_READ_MAX_BYTES,
        record_limit: CURRENT_READ_MAX_RECORDS,
    };
    let roles = reservation
        .current_event_roles(
            &scope,
            &runtime.inner.input_evidence_budget,
            (policy.byte_limit, policy.record_limit),
        )?
        .unwrap();
    let prepared = runtime.inner.runtime_db.transaction(|tx| {
        input_preparation::prepare_input_with_scheduling_and_event_roles_tx(
            tx,
            &scope,
            &root.state,
            &policy,
            12,
            &runtime.inner.input_evidence_budget,
            &crate::runtime::resident_scheduling::SchedulingCaptureRequest::current(),
            Some(&roles),
        )
    })?;
    assert_eq!(
        prepared.current_reads.coverage().event_query.birth_after,
        Some(session.cursor)
    );
    assert_eq!(
        prepared
            .current_reads
            .capture_event_births(session.cursor, &request())?
            .events
            .len(),
        70
    );
    drop(prepared);
    let mut small = policy.clone();
    small.byte_limit = roles.material_estimate().0 - 1;
    runtime
        .inner
        .runtime_db
        .connection()?
        .execute_batch("ALTER TABLE agent_states RENAME TO event_prepare_state_forbidden")?;
    let error = runtime
        .inner
        .runtime_db
        .transaction(|tx| {
            input_preparation::prepare_input_with_scheduling_and_event_roles_tx(
                tx,
                &scope,
                &root.state,
                &small,
                12,
                &runtime.inner.input_evidence_budget,
                &crate::runtime::resident_scheduling::SchedulingCaptureRequest::current(),
                Some(&roles),
            )
        })
        .err()
        .context("role material was not rejected before State body SQL")?;
    runtime
        .inner
        .runtime_db
        .connection()?
        .execute_batch("ALTER TABLE event_prepare_state_forbidden RENAME TO agent_states")?;
    assert!(
        matches!(
            error.downcast_ref::<crate::runtime::current_read_facts::CurrentReadError>(),
            Some(crate::runtime::current_read_facts::CurrentReadError::ResourceExhausted)
        ),
        "{error:#}"
    );
    let counts:(usize,usize)=runtime.inner.runtime_db.connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM audit_events WHERE kind='original_event_recovery'),(SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce='original-event-recovery-command')",
        [], |row| Ok((row.get(0)?,row.get(1)?)),
    )?;
    assert_eq!(counts, (70, 1));
    assert!(Arc::ptr_eq(
        &root,
        &runtime.inner.mutation_owner.capture()?.unwrap()
    ));
    Ok(())
}
