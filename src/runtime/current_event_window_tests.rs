use super::*;
use crate::runtime::{
    current_read_facts::{CurrentReadFact, CurrentReadRequest},
    mutation_owner::MutationOwnerRegistry,
    tests::support::context_config,
    RuntimeHandle,
};

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "finite-event-window".into(),
        identity_incarnation: 1,
    }
}
fn limits() -> (usize, usize) {
    (CURRENT_READ_MAX_BYTES, CURRENT_READ_MAX_RECORDS)
}
#[test]
fn finite_window_role_uses_max_live_window_and_keeps_native_min_cursor_independent() -> Result<()> {
    let owners = MutationOwnerRegistry::default();
    let owner = owners.claim(&scope().agent_id, 1)?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let registry = owner.current_event_queries()?;
    let native = registry.register(&scope(), owner.owner_epoch(), 7, &ledger, limits())?;
    let smaller = registry.register_ordinary_window(
        &scope(),
        owner.owner_epoch(),
        128,
        200,
        &ledger,
        limits(),
    )?;
    let larger = registry.register_ordinary_window(
        &scope(),
        owner.owner_epoch(),
        512,
        512,
        &ledger,
        limits(),
    )?;
    let roles = registry
        .snapshot(&scope(), owner.owner_epoch(), &ledger, limits())?
        .unwrap();
    assert_eq!(roles.birth_after(), Some(7));
    assert_eq!(roles.window_limits(), Some((512, 512)));
    let mut request = CurrentReadRequest::current(CURRENT_READ_MAX_BYTES, CURRENT_READ_MAX_RECORDS);
    roles.prepare_request(&scope(), &ledger, &mut request)?;
    assert_eq!(
        (
            request.event_limit,
            request.event_count_limit,
            request.event_birth_after
        ),
        (512, 512, Some(7))
    );
    drop((roles, larger));
    let roles = registry
        .snapshot(&scope(), owner.owner_epoch(), &ledger, limits())?
        .unwrap();
    assert_eq!(roles.window_limits(), Some((128, 200)));
    assert_eq!(roles.birth_after(), Some(7));
    drop((roles, native));
    let roles = registry
        .snapshot(&scope(), owner.owner_epoch(), &ledger, limits())?
        .unwrap();
    assert!(
        roles.birth_after().is_none(),
        "ordinary history cannot manufacture native birth zero"
    );
    assert_eq!(roles.window_limits(), Some((128, 200)));
    drop((roles, smaller));
    assert!(registry
        .snapshot(&scope(), owner.owner_epoch(), &ledger, limits())?
        .is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn finite_window_role_rejects_oversize_foreign_ledger_epoch_and_retains_only_actual_pins(
) -> Result<()> {
    let owners = MutationOwnerRegistry::default();
    let owner = owners.claim(&scope().agent_id, 1)?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let registry = owner.current_event_queries()?;
    for (body, headers) in [(0, 512), (513, 513), (128, 64)] {
        let error = registry
            .register_ordinary_window(
                &scope(),
                owner.owner_epoch(),
                body,
                headers,
                &ledger,
                limits(),
            )
            .err()
            .context("invalid finite Event role accepted")?;
        assert!(matches!(
            error.downcast_ref::<CurrentReadError>(),
            Some(CurrentReadError::WindowExpansion)
        ));
    }
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let lease = registry.register_ordinary_window(
        &scope(),
        owner.owner_epoch(),
        512,
        512,
        &ledger,
        limits(),
    )?;
    let foreign = InputEvidenceBudgetLedger::new(Default::default())?;
    assert!(registry
        .validate_ordinary_lease(&lease, &scope(), owner.owner_epoch(), &foreign)
        .is_err());
    assert!(registry
        .validate_ordinary_lease(&lease, &scope(), owner.owner_epoch() + 1, &ledger)
        .is_err());
    let before = ledger.report()?.observed_bytes;
    let error = registry
        .snapshot(
            &scope(),
            owner.owner_epoch(),
            &ledger,
            lease.material_estimate(),
        )
        .err()
        .context("joint snapshot ignored its retained lease")?;
    assert!(matches!(
        error.downcast_ref::<CurrentReadError>(),
        Some(CurrentReadError::ResourceExhausted)
    ));
    assert_eq!(ledger.report()?.observed_bytes, before);
    // A live metadata lease and selected snapshot remain observed through their last actual holder.
    let held = registry
        .snapshot(&scope(), owner.owner_epoch(), &ledger, limits())?
        .unwrap();
    let weak = Arc::downgrade(&lease.0);
    drop(lease);
    assert!(weak.upgrade().is_some());
    assert!(ledger.report()?.observed_bytes > 0);
    drop(held);
    assert!(weak.upgrade().is_none());
    assert!(registry
        .snapshot(&scope(), owner.owner_epoch(), &ledger, limits())?
        .is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

async fn runtime_fixture() -> Result<(tempfile::TempDir, RuntimeHandle)> {
    let home = tempfile::tempdir()?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "ordinary-event-window-owner",
        home.path().into(),
        context_config(),
    )?;
    runtime.refresh_context_resources().await?;
    Ok((home, runtime))
}
fn current_request() -> CurrentReadRequest {
    CurrentReadRequest::current(CURRENT_READ_MAX_BYTES, CURRENT_READ_MAX_RECORDS)
}
fn window_request() -> CurrentReadRequest {
    let mut request = current_request();
    request.event_limit = 512;
    request.event_count_limit = 512;
    request
}
async fn seed_actual_events(runtime: &RuntimeHandle, range: std::ops::Range<usize>) -> Result<()> {
    use crate::runtime_db::transitions::{
        AgentPostureTransitionCommand, AgentStateMutation, PreparedInputTransition,
    };
    for batch in range.collect::<Vec<_>>().chunks(32) {
        let mut mutation = runtime.begin_mutation().await?;
        let root = mutation.capture_published_root()?;
        let at = runtime.now();
        // Original Posture no-op intentionally omits Audit writes. This setup
        // must make a real State change for every original batch ACK.
        let mut after = root.state.as_ref().clone();
        after.last_wake_reason = Some(format!("original-event-batch-{}", batch[0]));
        let events = batch
            .iter()
            .map(|index| {
                let mut event =
                    crate::types::AuditEvent::legacy("runtime_error", serde_json::Value::Null);
                event.id = format!("window-event-{index:04}");
                event.created_at = at;
                event
            })
            .collect();
        let prepared = PreparedInputTransition::posture(
            mutation.input_evidence_scope(),
            format!("original-event-batch-{}", batch[0]),
            at,
            AgentPostureTransitionCommand {
                agent_id: root.state.id.clone(),
                expected_incarnation: Some(root.token.identity_incarnation),
                agent_state: AgentStateMutation {
                    expected: Some(Box::new(root.state.as_ref().clone())),
                    record: Box::new(after),
                },
                audit_events: events,
                fault: None,
            },
            None,
        )?;
        let commit = mutation.commit_prepared_input_transition(prepared).await?;
        assert!(
            commit.applied,
            "original batch must be an actual State/Audit ACK"
        );
        let accepted: usize = runtime.runtime_db().connection()?.query_row(
            "SELECT COUNT(*) FROM current_read_members WHERE family='event' AND record_id LIKE 'window-event-%'", [], |row|row.get(0),
        )?;
        assert_eq!(
            accepted,
            batch.last().unwrap() + 1,
            "each original batch must install the full exact accepted prefix"
        );
    }
    Ok(())
}
async fn state_only(runtime: &RuntimeHandle, nonce: &str) -> Result<()> {
    use crate::runtime_db::transitions::{
        AgentPostureTransitionCommand, AgentStateMutation, PreparedInputTransition,
    };
    let mut mutation = runtime.begin_mutation().await?;
    let root = mutation.capture_published_root()?;
    let mut state = root.state.as_ref().clone();
    state.last_wake_reason = Some(nonce.into());
    let prepared = PreparedInputTransition::posture(
        mutation.input_evidence_scope(),
        nonce.into(),
        runtime.now(),
        AgentPostureTransitionCommand {
            agent_id: state.id.clone(),
            expected_incarnation: Some(root.token.identity_incarnation),
            agent_state: AgentStateMutation {
                expected: Some(Box::new(root.state.as_ref().clone())),
                record: Box::new(state),
            },
            audit_events: Vec::new(),
            fault: None,
        },
        None,
    )?;
    mutation.commit_prepared_input_transition(prepared).await?;
    Ok(())
}
#[tokio::test]
async fn actual_continuation_preparation_keeps_accepted_top_512_and_ordinary_capture_never_hydrates(
) -> Result<()> {
    let (_home, runtime) = runtime_fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    seed_actual_events(&runtime, 0..531).await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    assert_eq!(
        root.current_reads
            .as_ref()
            .unwrap()
            .coverage()
            .event_body_limit,
        64
    );
    assert!(matches!(
        root.current_reads
            .as_ref()
            .unwrap()
            .capture(&window_request())
            .err()
            .unwrap()
            .downcast_ref::<CurrentReadError>(),
        Some(CurrentReadError::WindowExpansion)
    ));
    drop(root);
    // A canonical late row has no accepted current/fixed authority. The named
    // finite role expands only the accepted heads, never this newer raw tail.
    let mut raw = crate::types::AuditEvent::legacy("runtime_error", serde_json::Value::Null);
    raw.id = "window-raw-late".into();
    runtime.runtime_db().transaction(|tx| {
        crate::runtime_db::evidence::append_audit_event_tx(
            tx,
            Some(runtime.instance_agent_id()),
            &raw,
        )
        .map(|_| ())
    })?;
    let mut mutation = runtime.begin_mutation().await?;
    let read = mutation
        .prepare_scheduler_continuation_read(runtime.now())
        .await?;
    drop(mutation);
    assert_eq!(read.current.events.len(), 512);
    assert_eq!(
        read.current.events.first().unwrap().locator.key.id,
        "window-event-0019"
    );
    assert_eq!(
        read.current.events.last().unwrap().locator.key.id,
        "window-event-0530"
    );
    assert!(read
        .current
        .events
        .iter()
        .all(|record| record.locator.key.id != raw.id));
    let absent: usize = runtime.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM current_read_members WHERE family='event' AND record_id=?1",
        [&raw.id],
        |row| row.get(0),
    )?;
    assert_eq!(absent, 0);
    let selected_weak = Arc::downgrade(read.current.events.first().unwrap());
    state_only(&runtime, "live-window-state-only").await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let current = root.current_reads.as_ref().unwrap();
    assert_eq!(
        (
            current.coverage().event_body_limit,
            current.coverage().event_header_limit
        ),
        (512, 512)
    );
    assert_eq!(current.capture(&window_request())?.events.len(), 512);
    let mut small = window_request();
    small.byte_limit = read
        .current
        .records
        .iter()
        .map(|record| record.resident_bytes)
        .sum();
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let error = current
        .capture(&small)
        .err()
        .context("selected body-only allowance ignored its actual selector metadata")?;
    assert!(
        matches!(
            error.downcast_ref::<CurrentReadError>(),
            Some(CurrentReadError::ResourceExhausted)
        ),
        "{error:#}"
    );
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );

    runtime.runtime_db().connection()?.execute_batch("ALTER TABLE audit_events RENAME TO window_event_sql_forbidden; ALTER TABLE queue_entries RENAME TO window_queue_sql_forbidden")?;
    // Both the in-flight carrier and a later ordinary same-Root capture are pure.
    assert_eq!(read.current.events.len(), 512);
    assert_eq!(current.capture(&window_request())?.events.len(), 512);
    runtime.runtime_db().connection()?.execute_batch("ALTER TABLE window_queue_sql_forbidden RENAME TO queue_entries; ALTER TABLE window_event_sql_forbidden RENAME TO audit_events")?;
    drop((read, root));
    state_only(&runtime, "released-window-state-only").await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let current = root.current_reads.as_ref().unwrap();
    assert_eq!(
        (
            current.coverage().event_body_limit,
            current.coverage().event_header_limit
        ),
        (64, 100)
    );
    assert_eq!(current.capture(&current_request())?.events.len(), 64);
    assert!(
        selected_weak.upgrade().is_none(),
        "released ordinary role must not pin older bodies"
    );
    assert!(matches!(
        current
            .capture(&window_request())
            .err()
            .unwrap()
            .downcast_ref::<CurrentReadError>(),
        Some(CurrentReadError::WindowExpansion)
    ));
    Ok(())
}
#[tokio::test]
async fn released_exhaustive_finite_window_keeps_count_frontier_incomplete_and_native_role_independent(
) -> Result<()> {
    let (_home, runtime) = runtime_fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    seed_actual_events(&runtime, 0..5).await?;
    let owner = runtime.inner.mutation_owner.reserve().await?;
    let root = owner.capture_ready_snapshot()?.unwrap();
    let session = runtime.register_current_event_birth_session_from_snapshot(&owner, &root)?;
    drop((owner, root));
    seed_actual_events(&runtime, 5..130).await?;
    let mut mutation = runtime.begin_mutation().await?;
    let read = mutation
        .prepare_scheduler_continuation_read(runtime.now())
        .await?;
    drop(mutation);
    assert_eq!(read.current.events.len(), 130);
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    assert!(
        root.current_reads
            .as_ref()
            .unwrap()
            .coverage()
            .events_exhaustive
    );
    drop((read, root));
    state_only(&runtime, "only-native-role-left").await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let current = root.current_reads.as_ref().unwrap();
    assert_eq!(
        (
            current.coverage().event_body_limit,
            current.coverage().event_header_limit
        ),
        (64, 100)
    );
    assert!(
        !current.coverage().events_exhaustive,
        "discarded finite headers cannot prove larger count absence"
    );
    let births = runtime.capture_current_event_birth_session_from_snapshot(&session, &root)?;
    assert_eq!(
        births.selected.events.len(),
        125,
        "window release must preserve the independent native birth role"
    );
    assert!(births.selected.events.iter().all(|record|matches!(&record.fact,CurrentReadFact::Event(event) if event.id.starts_with("window-event-") && event.event_seq>session.cursor)));
    let mut expanded_count = current_request();
    expanded_count.event_count_limit = 512;
    assert!(matches!(
        current
            .capture(&expanded_count)
            .err()
            .unwrap()
            .downcast_ref::<CurrentReadError>(),
        Some(CurrentReadError::WindowExpansion)
    ));
    drop((births, session, root));
    state_only(&runtime, "all-event-roles-released").await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    assert!(root
        .current_reads
        .as_ref()
        .unwrap()
        .coverage()
        .event_query
        .born_event_seqs
        .is_empty());
    assert_eq!(
        root.current_reads
            .as_ref()
            .unwrap()
            .capture(&current_request())?
            .events
            .len(),
        64
    );
    Ok(())
}
