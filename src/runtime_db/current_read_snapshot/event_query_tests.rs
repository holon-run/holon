use super::*;

fn event(id: &str) -> AuditEvent {
    let mut event = AuditEvent::legacy("native-query", serde_json::Value::Null);
    event.id = id.into();
    event.created_at = at();
    event
}
fn commit(
    tx: &Transaction<'_>,
    state: &AgentState,
    budget: &InputEvidenceBudgetLedger,
    events: &[AuditEvent],
) -> Result<()> {
    let keys = events
        .iter()
        .map(|event| CurrentReadKey::event(&event.id))
        .collect();
    let mut proof = prepare_current_read_write_tx(tx, &scope(), state, &keys, &request(), budget)?;
    let mut actual = Vec::new();
    for event in events {
        actual.push(append_current_read_event_tx(&mut proof, event)?.1);
    }
    register_current_read_writes_tx(proof, &actual, state)?;
    Ok(())
}

#[test]
fn actual_event_query_keeps_complete_531_native_bodies_beyond_ordinary_window_and_pure_pins(
) -> Result<()> {
    let (_home, db, state) = legacy()?;
    let budget = ledger()?;
    db.transaction(|tx| {
        prepare_current_read_snapshot_tx(tx, &scope(), &state, &request(), &budget)?;
        Ok(())
    })?;
    for batch in (0..531).collect::<Vec<_>>().chunks(32) {
        let events = batch
            .iter()
            .map(|n| event(&format!("session-event-{n:03}")))
            .collect::<Vec<_>>();
        db.transaction(|tx| commit(tx, &state, &budget, &events))?;
    }
    let not_prepared = db.transaction(|tx| {
        prepare_current_read_snapshot_tx(tx, &scope(), &state, &request(), &budget)
    })?;
    let cursor = not_prepared
        .resident
        .coverage()
        .event_query
        .birth_cursor
        .context("native source missing")?;
    assert!(not_prepared
        .resident
        .capture_event_births(0, &request())
        .is_err());
    assert!(not_prepared
        .resident
        .capture_event_births(cursor, &request())?
        .events
        .is_empty());
    drop(not_prepared);
    // A later raw event never receives an original native birth or body grant.
    db.transaction(|tx| {
        crate::runtime_db::evidence::append_audit_event_tx(
            tx,
            Some(&scope().agent_id),
            &event("zz-raw-late"),
        )?;
        Ok(())
    })?;
    let mut role = request();
    role.event_birth_after = Some(0);
    let snapshot = db
        .transaction(|tx| prepare_current_read_snapshot_tx(tx, &scope(), &state, &role, &budget))?;
    assert_eq!(
        snapshot
            .resident
            .coverage()
            .event_query
            .born_event_seqs
            .len(),
        531
    );
    assert!(snapshot.resident.coverage().events.len() <= 100);
    let capture = snapshot.resident.capture_event_births(0, &role)?;
    assert_eq!(capture.events.len(), 531);
    assert_eq!(capture.birth_cursor, cursor);
    let weak = capture
        .events
        .iter()
        .map(Arc::downgrade)
        .collect::<Vec<_>>();
    let ids = capture
        .events
        .iter()
        .map(|r| r.locator.key.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids.first(), Some(&"session-event-000"));
    assert_eq!(ids.last(), Some(&"session-event-530"));
    assert!(!ids.contains(&"zz-raw-late"));
    drop(ids);
    drop(snapshot);
    drop(db);
    assert!(weak.iter().all(|item| item.upgrade().is_some()));
    assert!(budget.report()?.observed_bytes > 0);
    drop(capture);
    assert!(weak.iter().all(|item| item.upgrade().is_none()));
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn native_event_query_pure_pruned_capture_and_old_coverage_defaults_incomplete() -> Result<()> {
    let (_home, db, state) = legacy()?;
    let budget = ledger()?;
    db.transaction(|tx| {
        prepare_current_read_snapshot_tx(tx, &scope(), &state, &request(), &budget)?;
        commit(tx, &state, &budget, &[event("pruned-native")])?;
        Ok(())
    })?;
    let mut role = request();
    role.event_birth_after = Some(0);
    let snapshot = db
        .transaction(|tx| prepare_current_read_snapshot_tx(tx, &scope(), &state, &role, &budget))?;
    // Exercise the pure published tombstone contract. This fixture does not
    // claim the separately owned retention writer has been integrated.
    let selected = snapshot.resident.capture(&role)?;
    let mut coverage = snapshot.resident.coverage().clone();
    coverage.events.retain(|key| key.id != "pruned-native");
    for observed in &mut coverage.observations {
        if observed.key == CurrentReadKey::event("pruned-native") {
            observed.availability = CurrentReadAvailability::Pruned;
        }
    }
    let records = selected
        .records
        .iter()
        .filter(|r| r.locator.key.id != "pruned-native")
        .cloned()
        .collect();
    let raw: String = db.connection()?.query_row(
        "SELECT payload_json FROM agent_identities WHERE agent_id=?1",
        [&scope().agent_id],
        |r| r.get(0),
    )?;
    let identity: AgentIdentityRecord = serde_json::from_str(&raw)?;
    let tombstone = ResidentCurrentReadFacts::bootstrap(
        scope(),
        &identity,
        coverage,
        records,
        budget.clone(),
        &role,
    )?;
    let error = tombstone
        .capture_event_births(0, &role)
        .err()
        .context("Pruned native Event was accepted")?;
    assert!(
        matches!(error.downcast_ref::<CurrentReadError>(),Some(CurrentReadError::Pruned(key)) if key.id=="pruned-native")
    );
    let mut old = serde_json::to_value(snapshot.resident.coverage())?;
    old.as_object_mut().unwrap().remove("event_query");
    let old: CurrentReadCoverage = serde_json::from_value(old)?;
    assert!(old.event_query.birth_cursor.is_none());
    drop((tombstone, selected, snapshot));
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn actual_event_query_selected_body_and_output_metadata_use_one_joint_allowance() -> Result<()> {
    let (_home, db, state) = legacy()?;
    let budget = ledger()?;
    db.transaction(|tx| {
        prepare_current_read_snapshot_tx(tx, &scope(), &state, &request(), &budget)?;
        commit(tx, &state, &budget, &[event("joint-native")])?;
        Ok(())
    })?;
    let mut role = request();
    role.event_birth_after = Some(0);
    let snapshot = db
        .transaction(|tx| prepare_current_read_snapshot_tx(tx, &scope(), &state, &role, &budget))?;
    let capture = snapshot.resident.capture_event_births(0, &role)?;
    let (total, _) = capture.material_estimate()?;
    let body = capture
        .events
        .iter()
        .map(|r| r.resident_bytes)
        .sum::<usize>();
    let metadata = usize::try_from(total)? - body;
    let limit = usize::try_from(total)? - 1;
    assert!(body < limit && metadata < limit);
    drop(capture);
    let before = budget.report()?.observed_bytes;
    role.byte_limit = limit;
    let error = snapshot
        .resident
        .capture_event_births(0, &role)
        .err()
        .context("joint allowance failed open")?;
    assert!(matches!(
        error.downcast_ref::<CurrentReadError>(),
        Some(CurrentReadError::ResourceExhausted)
    ));
    assert_eq!(budget.report()?.observed_bytes, before);
    drop(snapshot);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn actual_event_query_rejects_birth_sequence_upshift_downshift_and_cursor_understatement(
) -> Result<()> {
    for mode in ["up", "down", "cursor"] {
        let (_home, db, state) = legacy()?;
        let budget = ledger()?;
        db.transaction(|tx| {
            prepare_current_read_snapshot_tx(tx, &scope(), &state, &request(), &budget)?;
            commit(
                tx,
                &state,
                &budget,
                &[event("first-native"), event("second-native")],
            )?;
            Ok(())
        })?;
        let (first,second):(u64,u64)=db.connection()?.query_row("SELECT (SELECT birth_seq FROM current_event_native_births WHERE event_id='first-native'),(SELECT birth_seq FROM current_event_native_births WHERE event_id='second-native')",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
        assert!(first > 1 && second > first);
        db.transaction(|tx| {
            match mode {
                "up"=>{tx.execute("UPDATE current_event_native_births SET birth_seq=?1 WHERE event_id='first-native'",[second+100])?;tx.execute("UPDATE current_event_birth_cursors SET high_water=?1",[second+100])?;},
                "down"=>{tx.execute("UPDATE current_event_native_births SET birth_seq=1 WHERE event_id='second-native'",[])?;},
                _=>{tx.execute("UPDATE current_event_birth_cursors SET high_water=0",[])?;},
            }
            Ok(())
        })?;
        let before: String = db.connection()?.query_row(
            "SELECT coverage_json FROM current_read_current",
            [],
            |r| r.get(0),
        )?;
        let mut role = request();
        role.event_birth_after = Some(if mode == "up" { second } else { first });
        let error = db
            .transaction(|tx| {
                prepare_current_read_snapshot_tx(tx, &scope(), &state, &role, &budget)
            })
            .err()
            .context("corrupt native sequence source was accepted")?;
        assert!(
            matches!(
                error.downcast_ref::<CurrentReadError>(),
                Some(CurrentReadError::CanonicalChanged)
            ),
            "{mode}: {error:#}"
        );
        assert_eq!(
            db.connection()?.query_row(
                "SELECT coverage_json FROM current_read_current",
                [],
                |r| r.get::<_, String>(0)
            )?,
            before
        );
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn actual_state_source_narrows_event_role_without_body_sql_and_releases_only_query_bodies(
) -> Result<()> {
    use super::super::writes::prepare_original_state_source_tx;
    let (_home, db, state) = legacy()?;
    let budget = ledger()?;
    db.transaction(|tx| {
        prepare_current_read_snapshot_tx(tx, &scope(), &state, &request(), &budget)?;
        Ok(())
    })?;
    for batch in (0..90).collect::<Vec<_>>().chunks(30) {
        let events = batch
            .iter()
            .map(|n| event(&format!("shrink-event-{n:03}")))
            .collect::<Vec<_>>();
        db.transaction(|tx| commit(tx, &state, &budget, &events))?;
    }
    let mut role = request();
    role.event_birth_after = Some(0);
    let before = db
        .transaction(|tx| prepare_current_read_snapshot_tx(tx, &scope(), &state, &role, &budget))?;
    let source = Arc::downgrade(&before.resident);
    let selected = before.resident.capture_event_births(0, &role)?;
    assert_eq!(selected.events.len(), 90);
    let old_body = Arc::downgrade(&selected.events[0]);
    let retained_body = Arc::downgrade(&selected.events[89]);
    let CurrentReadFact::Event(after_event) = &selected.events[25].fact else {
        unreachable!()
    };
    let after_seq = after_event.event_seq;
    role.event_birth_after = Some(after_seq);
    // The exact original metadata source suffices for narrowing; body SQL is denied.
    db.connection()?.execute_batch("ALTER TABLE queue_entries RENAME TO role_queue_forbidden; ALTER TABLE audit_events RENAME TO role_events_forbidden")?;
    let narrowed = db.transaction(|tx| {
        let proof = prepare_original_state_source_tx(
            tx,
            &scope(),
            &state,
            &role,
            &budget,
            (0, 0),
            Vec::new(),
            before.resident.clone(),
        )?;
        register_current_read_writes_tx(proof, &[], &state)
    })?;
    assert_eq!(narrowed.revision, before.resident.coverage().revision);
    assert_eq!(
        narrowed
            .snapshot
            .resident
            .capture_event_births(after_seq, &role)?
            .events
            .len(),
        64
    );
    assert!(narrowed
        .snapshot
        .resident
        .capture_event_births(0, &role)
        .is_err());
    let ordinary = request();
    let released = db.transaction(|tx| {
        let proof = prepare_original_state_source_tx(
            tx,
            &scope(),
            &state,
            &ordinary,
            &budget,
            (0, 0),
            Vec::new(),
            narrowed.snapshot.resident.clone(),
        )?;
        register_current_read_writes_tx(proof, &[], &state)
    })?;
    assert_eq!(released.revision, before.resident.coverage().revision);
    assert!(released
        .snapshot
        .resident
        .coverage()
        .event_query
        .birth_after
        .is_none());
    assert!(released
        .snapshot
        .resident
        .coverage()
        .event_query
        .born_event_seqs
        .is_empty());
    assert_eq!(released.snapshot.resident.coverage().events.len(), 91);
    assert!(released
        .snapshot
        .resident
        .coverage()
        .observations
        .iter()
        .any(|o| o.key == CurrentReadKey::event("shrink-event-000")
            && o.availability == CurrentReadAvailability::Present));
    assert_eq!(
        released.snapshot.resident.capture(&ordinary)?.events.len(),
        64
    );
    assert!(released
        .snapshot
        .resident
        .capture_event_births(0, &ordinary)
        .is_err());
    let physical = budget.report()?.physical_records.bytes;
    drop((before, narrowed));
    assert!(source.upgrade().is_none());
    assert!(
        old_body.upgrade().is_some(),
        "the independently held selected query owns its body"
    );
    drop(selected);
    assert!(
        old_body.upgrade().is_none(),
        "new ordinary Root retains no header-only body"
    );
    assert!(retained_body.upgrade().is_some());
    assert!(budget.report()?.physical_records.bytes < physical);
    drop(released);
    assert!(retained_body.upgrade().is_none());
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn pure_state_source_cannot_expand_an_unprepared_or_narrowed_event_role() -> Result<()> {
    let (_home, db, state) = legacy()?;
    let budget = ledger()?;
    db.transaction(|tx| {
        prepare_current_read_snapshot_tx(tx, &scope(), &state, &request(), &budget)?;
        commit(tx, &state, &budget, &[event("role-expansion-native")])
    })?;
    let original = db.transaction(|tx| {
        prepare_current_read_snapshot_tx(tx, &scope(), &state, &request(), &budget)
    })?;
    let payload: String = db.connection()?.query_row(
        "SELECT payload_json FROM agent_identities WHERE agent_id=?1",
        [&scope().agent_id],
        |r| r.get(0),
    )?;
    let identity: AgentIdentityRecord = serde_json::from_str(&payload)?;
    let mut role = request();
    role.event_birth_after = Some(0);
    let before = budget.report()?.observed_bytes;
    let error = original
        .resident
        .rebind_state_digest(
            &original.resident.coverage().state_digest,
            &state,
            &identity,
            original.resident.coverage().revision,
            &role,
        )
        .err()
        .context("pure State source expanded missing Event coverage")?;
    assert!(matches!(
        error.downcast_ref::<CurrentReadError>(),
        Some(CurrentReadError::NeedsPreparation)
    ));
    assert_eq!(budget.report()?.observed_bytes, before);
    role.event_birth_after = original.resident.coverage().event_query.birth_cursor;
    let empty = original.resident.rebind_state_digest(
        &original.resident.coverage().state_digest,
        &state,
        &identity,
        original.resident.coverage().revision,
        &role,
    )?;
    assert!(empty
        .capture_event_births(role.event_birth_after.unwrap(), &role)?
        .events
        .is_empty());
    drop((empty, original));
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
