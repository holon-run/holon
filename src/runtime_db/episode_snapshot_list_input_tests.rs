use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    types::{AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility},
};
use std::sync::Arc;

const AGENT: &str = "continuity-list";

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: AGENT.into(),
        identity_incarnation: 1,
    }
}

fn base() -> (u64, usize) {
    ((AGENT.len() as u64 + 256 + 31) * 8, 64 + 3 * 8)
}

fn payload(id: &str) -> Result<String> {
    Ok(serde_json::to_string(&serde_json::from_value::<
        ContextEpisodeRecord,
    >(serde_json::json!({
        "id": id, "agent_id": AGENT, "workspace_id": "界\0\"\\",
        "created_at": "2026-10-18T00:00:00Z",
        "finalized_at": "2026-10-18T00:01:00Z",
        "start_turn_index": 1, "end_turn_index": 2,
        "start_message_count": 0, "end_message_count": 1,
        "boundary_reason": "wait_boundary",
    }))?)?)
}

fn connection() -> Result<Connection> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(
        "CREATE TABLE context_episode_anchors (
             episode_id TEXT, agent_id TEXT, ended_at TEXT, started_at TEXT, payload_json TEXT
         );
         CREATE TABLE episode_memory_results (finalized_episode_id TEXT);
         CREATE TABLE input_markers (value TEXT);",
    )?;
    Ok(connection)
}

fn put(connection: &Connection, id: &str, end: &str, start: &str, body: &str) -> Result<()> {
    connection.execute(
        "INSERT INTO context_episode_anchors VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, AGENT, end, start, body],
    )?;
    Ok(())
}

fn json_cost(body: &str) -> (u64, usize) {
    // Independent lexical walk: only opening string quotes count as slots.
    let mut quoted = false;
    let mut escaped = false;
    let mut slots = 0usize;
    for byte in body.bytes() {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else {
            slots += usize::from(b"\"{}[]:,".contains(&byte));
            quoted = byte == b'"';
        }
    }
    (
        (body.len() as u64 * 4 + slots as u64 * 128 + 512) * 8,
        (slots * 4 + 16) * 8,
    )
}

fn row_cost(body: &str) -> (u64, usize) {
    let json = json_cost(body);
    (
        json.0 + (body.len() + std::mem::size_of::<EpisodeRow>()) as u64 * 8,
        json.1 + 8,
    )
}

#[test]
fn list_input_preserves_sql_order_payload_receipts_and_exact_original_phase_cost() -> Result<()> {
    let connection = connection()?;
    let bodies = [
        payload("z")?,
        format!(" \n{}\t", payload("a\0界\"\\")?),
        payload("b")?,
        payload("c")?,
    ];
    for (id, end, start, body) in [
        ("z", "3", "1", &bodies[0]),
        ("a", "2", "9", &bodies[1]),
        ("b", "2", "9", &bodies[2]),
        ("c", "2", "1", &bodies[3]),
    ] {
        put(&connection, id, end, start, body)?;
    }
    connection.execute_batch(
        "INSERT INTO episode_memory_results VALUES ('b');
         INSERT INTO context_episode_anchors VALUES ('foreign', 'foreign', '9', '9', 'invalid');",
    )?;
    let per_call = bodies
        .iter()
        .map(|body| row_cost(body))
        .fold((0, 0), |(bytes, records), cost| {
            (bytes + cost.0, records + cost.1)
        });
    let initial = base();
    let exact = (initial.0 + 2 * per_call.0, initial.1 + 2 * per_call.1);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        charge_owned_metadata(31, 3)?;
        for call in 1..=2 {
            let rows = all_episode_rows(&connection, AGENT)?;
            assert_eq!(rows.len(), 4);
            for (index, row) in rows.iter().enumerate() {
                assert_eq!(row.payload.as_bytes(), bodies[index].as_bytes());
                assert_eq!(
                    serde_json::to_value(&row.record)?,
                    serde_json::from_str::<serde_json::Value>(&bodies[index])?
                );
                assert_eq!(row.receipted, index == 2);
            }
            assert_eq!(
                phase.finish()?,
                (
                    initial.0 + call * per_call.0,
                    initial.1 + call as usize * per_call.1
                )
            );
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder
            ));
        }
        assert!(all_episode_rows(&connection, "missing")?.is_empty());
        assert_eq!(phase.finish()?, exact);
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let last = holder.clone();
    drop((phase, holder));
    assert_eq!(ledger.report()?.observed_bytes, exact.0);
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn list_input_both_gates_and_dimensions_reject_before_decode_and_roll_back() -> Result<()> {
    for at_row_gate in [false, true] {
        for records_limit in [false, true] {
            let mut connection = connection()?;
            let first = payload("first")?;
            let poison = "not-json";
            put(&connection, "first", "2", "1", &first)?;
            put(&connection, "poison", "1", "1", poison)?;
            let initial = base();
            let first_cost = row_cost(&first);
            let second = if at_row_gate {
                row_cost(poison)
            } else {
                json_cost(poison)
            };
            let exact = (
                initial.0 + first_cost.0 + second.0,
                initial.1 + first_cost.1 + second.1,
            );
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                exact.0 - u64::from(!records_limit),
                exact.1 - usize::from(records_limit),
            )?;
            let holder = phase.guard()?;
            {
                let _entry = phase.enter();
                charge_owned_metadata(31, 3)?;
                assert!(Arc::ptr_eq(
                    &ResourceReadMaterial::active_guard()?.unwrap(),
                    &holder
                ));
                let tx = connection.transaction()?;
                tx.execute("INSERT INTO input_markers VALUES ('rollback')", [])?;
                let error = all_episode_rows(&tx, AGENT)
                    .err()
                    .context("unexpected decode")?;
                assert!(
                    error.downcast_ref::<std::io::Error>().is_some(),
                    "{error:#}"
                );
                assert!(!format!("{error:#}").contains("decoding context episode"));
                assert!(phase.finish().is_err());
                assert!(ResourceReadMaterial::active_guard().is_err());
                drop(tx);
                assert_eq!(
                    connection.query_row("SELECT COUNT(*) FROM input_markers", [], |row| row
                        .get::<_, u64>(0))?,
                    0
                );
            }
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            let retained = ledger.report()?.observed_bytes;
            assert!(retained >= initial.0 + first_cost.0);
            let last = holder.clone();
            drop((phase, holder));
            assert_eq!(ledger.report()?.observed_bytes, retained);
            drop(last);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn list_input_json_and_sql_errors_release_readers_without_refunding_original_phase() -> Result<()> {
    for bad_sql_type in [false, true] {
        let mut connection = connection()?;
        let first = payload("first")?;
        put(&connection, "first", "2", "1", &first)?;
        put(&connection, "broken", "1", "1", "{invalid")?;
        if bad_sql_type {
            connection.execute_batch(
                "UPDATE context_episode_anchors SET payload_json = x'80' WHERE episode_id = 'broken';"
            )?;
        }
        let initial = base();
        let first_cost = row_cost(&first);
        let bad_cost = if bad_sql_type {
            (0, 0)
        } else {
            row_cost("{invalid")
        };
        let exact = (
            initial.0 + first_cost.0 + bad_cost.0,
            initial.1 + first_cost.1 + bad_cost.1,
        );
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            charge_owned_metadata(31, 3)?;
            let tx = connection.transaction()?;
            tx.execute("INSERT INTO input_markers VALUES ('rollback')", [])?;
            let error = all_episode_rows(&tx, AGENT)
                .err()
                .context("unexpected rows")?;
            if bad_sql_type {
                assert!(error
                    .downcast_ref::<rusqlite::types::FromSqlError>()
                    .is_some());
            } else {
                assert!(format!("{error:#}").contains("decoding context episode payload"));
            }
            assert_eq!(phase.finish()?, exact);
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder
            ));
            // No live statement/row or payload borrow escapes either error.
            tx.execute_batch("DROP TABLE context_episode_anchors;")?;
            tx.rollback()?;
            assert_eq!(
                connection.query_row("SELECT COUNT(*) FROM input_markers", [], |row| row
                    .get::<_, u64>(0))?,
                0
            );
        }
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        let last = holder.clone();
        drop((phase, holder));
        assert_eq!(ledger.report()?.observed_bytes, exact.0);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn list_input_original_payload_cap_precedes_admission_and_decode() -> Result<()> {
    let connection = connection()?;
    connection.execute(
        "INSERT INTO context_episode_anchors
         VALUES ('oversize', ?1, '1', '1', CAST(zeroblob(?2) AS TEXT))",
        params![AGENT, CONTINUITY_MAX_PAYLOAD_BYTES + 1],
    )?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let initial = ((AGENT.len() as u64 + 256) * 8, 64);
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), initial.0, initial.1)?;
    {
        let _entry = phase.enter();
        let error = all_episode_rows(&connection, AGENT)
            .err()
            .context("missing hard cap")?;
        assert!(matches!(
            error.downcast_ref::<EpisodeSnapshotError>(),
            Some(EpisodeSnapshotError::ClosureResourceExhausted {
                limit: "payload bytes"
            })
        ));
        assert_eq!(phase.finish()?, initial);
    }
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn list_input_real_identity_import_keeps_rows_and_atomicity_on_original_phase() -> Result<()> {
    let home = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    let identity = AgentIdentityRecord::new(
        AGENT,
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    let body = payload("imported")?;
    let record: ContextEpisodeRecord = serde_json::from_str(&body)?;
    db.context_episodes().upsert(&record)?;
    db.connection()?.execute_batch(
        "CREATE TRIGGER reject_member BEFORE INSERT ON episode_continuity_members
         BEGIN SELECT RAISE(ABORT, 'list input import failure'); END;",
    )?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let holder = phase.guard()?;
    let used;
    {
        let _entry = phase.enter();
        let write = |tx: &Transaction<'_>| -> Result<()> {
            prepare_imported_identity_continuity_tx(tx, &identity, identity.updated_at)?;
            super::super::evidence::upsert_agent_identity_tx(tx, &identity)
        };
        let error = db.transaction(write).unwrap_err();
        assert!(format!("{error:#}").contains("list input import failure"));
        for table in [
            "episode_continuity_seeds",
            "episode_continuity_members",
            "agent_episode_continuity",
            "agent_identities",
            "agent_identity_reservations",
        ] {
            assert_eq!(
                db.connection()?.query_row(
                    &format!("SELECT COUNT(*) FROM {table}"),
                    [],
                    |row| row.get::<_, u64>(0)
                )?,
                0
            );
        }
        db.connection()?
            .execute_batch("DROP TRIGGER reject_member;")?;
        db.transaction(write)?;
        let stored: (String, String) = db.connection()?.query_row(
            "SELECT episode_id, content_digest FROM episode_continuity_members",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(stored, (record.id.clone(), bytes_digest(body.as_bytes())));
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &holder
        ));
        used = phase.finish()?;
        assert!(used.0 >= 4 * row_cost(&serde_json::to_string(&record)?).0);
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let last = holder.clone();
    drop((phase, holder));
    assert_eq!(ledger.report()?.observed_bytes, used.0);
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
