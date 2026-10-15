use super::*;
use crate::domain::execution_protocol::*;
use rusqlite::Connection;
fn connection() -> Result<Connection> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch("CREATE TABLE execution_protocol_partitions(agent_id TEXT PRIMARY KEY,updated_at TEXT);
      CREATE TABLE execution_protocol_attempts(agent_id TEXT,attempt_id TEXT,lifecycle_state TEXT,terminal_outcome_id TEXT,payload_json TEXT,PRIMARY KEY(agent_id,attempt_id));
      CREATE TABLE execution_protocol_work_items(agent_id TEXT,work_item_id TEXT,lifecycle_state TEXT,payload_json TEXT,PRIMARY KEY(agent_id,work_item_id));
      CREATE TABLE episode_continuity_seeds(seed_id TEXT PRIMARY KEY);")?;
    connection.execute_batch(INPUT_EVIDENCE_SCHEMA)?;
    connection.execute("INSERT INTO episode_continuity_seeds VALUES('seed')", [])?;
    connection.execute(
        "INSERT INTO execution_protocol_partitions VALUES('agent','now')",
        [],
    )?;
    Ok(connection)
}
fn scope(inc: u64) -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "agent".into(),
        identity_incarnation: inc,
    }
}
fn attempt(id: &str, source: &str) -> ExecutionAttempt {
    ExecutionAttempt {
        attempt_id: id.into(),
        agent_id: "agent".into(),
        source_message_id: Some(source.into()),
        source: ExecutionSource {
            identity: ExecutionSourceIdentity::QueueMessage {
                message_id: source.into(),
            },
            generation: 1,
        },
        binding: ExecutionBinding::Conversation {
            interaction_id: source.into(),
        },
        provenance: ExecutionProvenance {
            origin: ExecutionOrigin::Operator,
            trust: ExecutionTrust::OperatorInstruction,
            priority: ExecutionPriority::Normal,
            correlation_id: None,
            causation_id: None,
        },
        admitted_fences: AdmittedFences {
            source_revision: 1,
            work_item_source_revision: None,
            work_item_generation: None,
            rejoin: None,
            agent_control_revision: 1,
            host_registry_revision: 1,
        },
        state: ExecutionAttemptState::Open,
        run_id: None,
        turn_id: Some(format!("turn:{id}")),
        recovery_of_attempt_id: None,
        terminal_outcome_id: None,
        admitted_at: "2026-10-12T00:00:00Z".into(),
        terminal_at: None,
    }
}
fn store(tx: &Transaction<'_>, attempt: &ExecutionAttempt) -> Result<()> {
    let lifecycle = serde_json::to_value(attempt.state)?
        .as_str()
        .unwrap()
        .to_owned();
    tx.execute("INSERT INTO execution_protocol_attempts VALUES(?1,?2,?3,?4,?5) ON CONFLICT(agent_id,attempt_id) DO UPDATE SET lifecycle_state=excluded.lifecycle_state,terminal_outcome_id=excluded.terminal_outcome_id,payload_json=excluded.payload_json",params![attempt.agent_id,attempt.attempt_id,lifecycle,attempt.terminal_outcome_id,serde_json::to_string(attempt)?])?;
    Ok(())
}
#[test]
fn fixed_source_metadata_does_not_grant_bodies_or_adopt_late_attempts() -> Result<()> {
    let mut db = connection()?;
    let tx = db.transaction()?;
    let mut first = attempt("activation:message:input", "input");
    first.state = ExecutionAttemptState::Settled;
    let mut fourth = attempt("activation:message:input:attempt:4", "input");
    fourth.state = ExecutionAttemptState::Settled;
    store(&tx, &first)?;
    store(&tx, &fourth)?;
    let roots = BTreeSet::from([Key::new(Family::Message, "input")]);
    capture_sources_tx(&tx, "seed", "upgrade", "agent", &roots, 1, 0)?;
    let fixed = fixed_seed_source_on(&tx, Some("seed"), "input", 100, 1024 * 1024)?.unwrap();
    assert_eq!(fixed.attempts.len(), 2);
    assert_eq!(
        fixed.next_attempt_id()?,
        "activation:message:input:attempt:5"
    );
    let eligible: i64 = tx.query_row(
        "SELECT count(*) FROM input_evidence_eligibility WHERE family='attempt'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(eligible, 0);
    store(&tx, &attempt("activation:message:input:attempt:7", "input"))?;
    capture_sources_tx(&tx, "seed", "first_import", "agent", &roots, 1, 0)?;
    assert_eq!(
        fixed_seed_source_on(&tx, Some("seed"), "input", 100, 1024 * 1024)?.unwrap(),
        fixed
    );
    assert!(assert_actual_source(&tx, "agent", &fixed, 100).is_err());
    Ok(())
}
#[test]
fn actual_writer_and_its_metadata_roll_back_in_the_same_transaction() -> Result<()> {
    let mut db = connection()?;
    let item = attempt("activation:message:input", "input");
    {
        let tx = db.transaction()?;
        store(&tx, &item)?;
        let delta = register_tx(
            &tx,
            &scope(1),
            &Binding { seed_id: None },
            &[CanonicalInputEvidence::Attempt(item.clone())],
        )?;
        assert_eq!(delta.sources["input"].allocation_highwater, 1);
        // Original repository fault/abort drops the same transaction.
    }
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM execution_protocol_attempts",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM input_evidence_execution_current_sources",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        0
    );
    let tx = db.transaction()?;
    store(&tx, &item)?;
    register_tx(
        &tx,
        &scope(1),
        &Binding { seed_id: None },
        &[CanonicalInputEvidence::Attempt(item)],
    )?;
    tx.commit()?;
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM input_evidence_execution_current_sources",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        1
    );
    Ok(())
}
#[test]
fn pruning_attempt_body_keeps_ordinal_highwater_and_exact_tombstone() -> Result<()> {
    let mut db = connection()?;
    let tx = db.transaction()?;
    let mut first = attempt("activation:message:input", "input");
    first.state = ExecutionAttemptState::Settled;
    let mut second = attempt("activation:message:input:attempt:2", "input");
    second.state = ExecutionAttemptState::Settled;
    store(&tx, &first)?;
    store(&tx, &second)?;
    register_tx(
        &tx,
        &scope(1),
        &Binding { seed_id: None },
        &[
            CanonicalInputEvidence::Attempt(first),
            CanonicalInputEvidence::Attempt(second.clone()),
        ],
    )?;
    tx.execute(
        "DELETE FROM execution_protocol_attempts WHERE attempt_id=?1",
        [&second.attempt_id],
    )?;
    let delta = prune_tx(
        &tx,
        &scope(1),
        &Binding { seed_id: None },
        &[Key::new(Family::Attempt, &second.attempt_id)],
    )?;
    let source = &delta.sources["input"];
    assert_eq!(source.allocation_highwater, 2);
    assert!(source.attempts[&second.attempt_id].pruned);
    assert_eq!(
        source.next_attempt_id()?,
        "activation:message:input:attempt:3"
    );
    assert_actual_source(&tx, "agent", source, 100)?;
    store(&tx, &second)?;
    assert!(register_tx(
        &tx,
        &scope(1),
        &Binding { seed_id: None },
        &[CanonicalInputEvidence::Attempt(second)]
    )
    .is_err());
    Ok(())
}
#[test]
fn new_incarnation_never_uses_old_source_order_metadata() -> Result<()> {
    let mut db = connection()?;
    let tx = db.transaction()?;
    let item = attempt("activation:message:input", "input");
    store(&tx, &item)?;
    register_tx(
        &tx,
        &scope(1),
        &Binding { seed_id: None },
        &[CanonicalInputEvidence::Attempt(item)],
    )?;
    assert!(source_on(&tx, &scope(2), None, "input", 100, 1024 * 1024)?.is_none());
    let observed = load_coverage_tx(
        &tx,
        &scope(2),
        &Binding { seed_id: None },
        &BTreeSet::from(["input".into()]),
        100,
        1024 * 1024,
        None,
    )?;
    assert!(observed.initialized); // structural container only
    assert!(observed.sources.is_empty());
    assert!(observed.source("input").is_err());
    Ok(())
}
#[test]
fn untracked_old_source_cannot_be_initialized_from_latest_history() -> Result<()> {
    let mut db = connection()?;
    let tx = db.transaction()?;
    let old = attempt("activation:message:input", "input");
    store(&tx, &old)?;
    let new = attempt("activation:message:input:attempt:2", "input");
    store(&tx, &new)?;
    assert!(register_tx(
        &tx,
        &scope(2),
        &Binding { seed_id: None },
        &[CanonicalInputEvidence::Attempt(new)]
    )
    .is_err());
    assert!(current_source_on(&tx, &scope(2), "input", 100, 1024 * 1024)?.is_none());
    Ok(())
}
#[test]
fn metadata_budget_rejects_whole_fixed_capture_instead_of_truncating() -> Result<()> {
    let mut db = connection()?;
    let tx = db.transaction()?;
    store(&tx, &attempt("activation:message:input", "input"))?;
    let roots = BTreeSet::from([Key::new(Family::Message, "input")]);
    assert!(capture_sources_tx(&tx, "seed", "upgrade", "agent", &roots, MAX_RECORDS, 0).is_err());
    assert_eq!(
        tx.query_row(
            "SELECT count(*) FROM input_evidence_execution_seed_sources",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        0
    );
    Ok(())
}

#[test]
fn unbound_recovery_updates_only_original_fixed_source_metadata() -> Result<()> {
    let mut db = connection()?;
    let tx = db.transaction()?;
    let mut item = attempt("activation:message:input", "input");
    store(&tx, &item)?;
    capture_sources_tx(
        &tx,
        "seed",
        "upgrade",
        "agent",
        &BTreeSet::from([Key::new(Family::Message, "input")]),
        1,
        0,
    )?;
    let original = fixed_seed_source_on(&tx, Some("seed"), "input", 100, 1024 * 1024)?.unwrap();
    // The original unbound recovery adapter separately verifies the canonical
    // command witness and writes this exact existing-member alias in this TX.
    tx.execute(
        "INSERT INTO input_evidence_eligibility VALUES('seed','upgrade','attempt',?1,'{}',1)",
        [&item.attempt_id],
    )?;
    tx.execute("INSERT INTO input_evidence_seed_recovery_current(seed_id,family,record_id,revision,fingerprint_json,origin_kind) VALUES('seed','attempt',?1,1,'{}','fixed_member')",[&item.attempt_id])?;
    item.state = ExecutionAttemptState::Interrupted;
    item.terminal_outcome_id = Some("outcome".into());
    item.terminal_at = Some("2026-10-12T00:01:00Z".into());
    store(&tx, &item)?;
    register_seed_recovery_tx(
        &tx,
        "seed",
        "agent",
        &[CanonicalInputEvidence::Attempt(item.clone())],
    )?;
    assert_eq!(
        fixed_seed_source_on(&tx, Some("seed"), "input", 100, 1024 * 1024)?.unwrap(),
        original
    );
    let recovered = seed_source_on(&tx, Some("seed"), "input", 100, 1024 * 1024)?.unwrap();
    assert_eq!(
        recovered.allocation_highwater,
        original.allocation_highwater
    );
    assert_eq!(
        recovered.attempts[&item.attempt_id].state,
        ExecutionAttemptState::Interrupted
    );
    assert_actual_source(&tx, "agent", &recovered, 100)?;
    let new = attempt("activation:message:input:attempt:2", "input");
    store(&tx, &new)?;
    assert!(register_seed_recovery_tx(
        &tx,
        "seed",
        "agent",
        &[CanonicalInputEvidence::Attempt(new)]
    )
    .is_err());
    Ok(())
}
