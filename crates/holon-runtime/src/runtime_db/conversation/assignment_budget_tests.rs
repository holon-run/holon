use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    runtime_db::RuntimeDb,
};

type Cost = (u64, usize);

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "assignment-budget-agent".into(),
        identity_incarnation: 1,
    }
}

fn header() -> Cost {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn add(a: Cost, b: Cost) -> Cost {
    (a.0 + b.0, a.1 + b.1)
}

fn timestamps(count: usize) -> Cost {
    // Literal DateTime wire shape, independent of the production scanner.
    let bytes = r#""2026-10-17T15:23:00.123456789Z""#.len() as u64;
    (
        (bytes * 4 + 128 + 512) * 8 * count as u64,
        (4 + 16) * 8 * count,
    )
}

fn now() -> DateTime<Utc> {
    "2026-10-17T15:23:00.123456789Z".parse().unwrap()
}

fn ledger() -> Result<InputEvidenceBudgetLedger> {
    Ok(InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?)
}

fn fixture(turn_exists: bool, source_exists: bool) -> Result<(tempfile::TempDir, RuntimeDb)> {
    let home = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    db.transaction(|tx| {
        if turn_exists {
            ensure_turn_revision_tx(tx, &scope().agent_id, "turn", now())?;
        }
        if source_exists {
            bump_source_revision_tx(
                tx,
                SOURCE_OPERATOR,
                "message",
                &scope().agent_id,
                None,
                now(),
            )?;
        }
        Ok(())
    })?;
    Ok((home, db))
}

fn assign(db: &RuntimeDb, message: &str) -> Result<bool> {
    db.transaction(|tx| assign_input_to_turn_tx(tx, &scope().agent_id, message, "turn", now()))
}

fn snapshot(db: &RuntimeDb) -> Result<Vec<String>> {
    db.transaction(|tx| {
        [
            "SELECT json_group_array(json_array(message_id,agent_id,turn_id,revision,assigned_at))
             FROM (SELECT * FROM conversation_input_assignments ORDER BY message_id)",
            "SELECT json_group_array(json_array(source_kind,source_id,agent_id,turn_id,
             activity_seq,revision,updated_at))
             FROM (SELECT * FROM conversation_source_revisions ORDER BY source_kind,source_id)",
            "SELECT json_group_array(json_array(agent_id,turn_id,summary_revision,
             detail_revision,result_settled,updated_at))
             FROM (SELECT * FROM conversation_turn_revisions ORDER BY agent_id,turn_id)",
        ]
        .into_iter()
        .map(|sql| tx.query_row(sql, [], |row| row.get(0)).map_err(Into::into))
        .collect()
    })
}

#[test]
fn assignment_exact_cost_sql_compatibility_and_last_shared_holder() -> Result<()> {
    for turn_exists in [false, true] {
        for source_exists in [false, true] {
            let (_home, db) = fixture(turn_exists, source_exists)?;
            let budget = ledger()?;
            let exact = add(add(header(), header()), timestamps(3));
            let phase =
                ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
            let transaction = phase.transaction()?;
            {
                let _entry = transaction.enter();
                assert!(assign(&db, "message")?);
                assert!(!assign(&db, "message")?);
            }
            assert_eq!(transaction.finish()?, add(header(), timestamps(3)));
            assert_eq!(phase.finish()?, exact);
            db.transaction(|tx| {
                let assignment: (i64, String) = tx.query_row(
                    "SELECT revision,assigned_at FROM conversation_input_assignments
                     WHERE message_id='message'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                assert_eq!(assignment, (1, "2026-10-17T15:23:00.123Z".into()));
                let source: (String, i64, i64, String) = tx.query_row(
                    "SELECT turn_id,revision,activity_seq,updated_at
                     FROM conversation_source_revisions WHERE source_kind=?1 AND source_id='message'",
                    [SOURCE_OPERATOR],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )?;
                assert_eq!(
                    source,
                    ("turn".into(), if source_exists { 2 } else { 1 }, 1, assignment.1)
                );
                let turn = turn_revision_tx(tx, &scope().agent_id, "turn")?.unwrap();
                assert_eq!(turn.summary, if turn_exists { 2 } else { 1 });
                assert_eq!(turn.detail, turn.summary);
                assert!(!turn.result_settled);
                Ok(())
            })?;
            let first = transaction.guard()?;
            let last = first.clone();
            assert!(std::sync::Arc::ptr_eq(&first, &last));
            drop((transaction, phase));
            assert_eq!(
                budget.report()?.observed_bytes,
                add(header(), timestamps(3)).0
            );
            drop(first);
            assert_eq!(
                budget.report()?.observed_bytes,
                add(header(), timestamps(3)).0
            );
            drop(last);
            assert_eq!(budget.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn assignment_each_timestamp_gate_rejects_and_rolls_back_all_sql() -> Result<()> {
    for turn_exists in [false, true] {
        for source_exists in [false, true] {
            let (_home, db) = fixture(turn_exists, source_exists)?;
            let before = snapshot(&db)?;
            for admitted in 0..3 {
                for bytes in [true, false] {
                    let budget = ledger()?;
                    let prefix = add(add(header(), header()), timestamps(admitted));
                    let next = timestamps(1);
                    let phase = ResourceReadMaterial::with_test_limits(
                        &budget,
                        &scope(),
                        if bytes {
                            prefix.0 + next.0 - 1
                        } else {
                            64 * 1024 * 1024
                        },
                        if bytes { 65536 } else { prefix.1 + next.1 - 1 },
                    )?;
                    let transaction = phase.transaction()?;
                    {
                        let _entry = transaction.enter();
                        let error = assign(&db, "message").unwrap_err();
                        assert!(format!("{error:#}")
                            .to_lowercase()
                            .contains("resourceexhausted"));
                        assert!(assign(&db, "message").is_err());
                    }
                    assert_eq!(snapshot(&db)?, before);
                    assert_eq!(budget.report()?.observed_bytes, prefix.0);
                    assert!(transaction.finish().is_err());
                    drop(transaction);
                    assert_eq!(budget.report()?.observed_bytes, header().0);
                    assert!(phase.finish().is_err());
                    assert!(phase.transaction().is_err());
                    drop(phase);
                    assert_eq!(budget.report()?.observed_bytes, 0);
                }
            }
        }
    }
    Ok(())
}

#[test]
fn assignment_reuses_original_remainder_failure_latch_and_tls() -> Result<()> {
    let (_home, db) = fixture(true, false)?;
    let budget = ledger()?;
    let exact = add(add(header(), header()), timestamps(5));
    let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
    let transaction = phase.transaction()?;
    let outer_entry = phase.enter();
    let first;
    let before;
    {
        let _entry = transaction.enter();
        assert!(assign(&db, "message")?);
        first = transaction.guard()?;
    }
    // Inspect persisted facts outside the budgeted scope.
    drop(outer_entry);
    before = snapshot(&db)?;
    let outer_entry = phase.enter();
    {
        let _entry = transaction.enter();
        let error = assign(&db, "second-message").unwrap_err();
        assert!(format!("{error:#}")
            .to_lowercase()
            .contains("resourceexhausted"));
        assert!(assign(&db, "second-message").is_err());
    }
    // Restored TLS belongs to the failed original owner, not a fresh allowance.
    assert!(crate::diagnostics::resource_io::admit_serialized_value(&now()).is_err());
    assert!(phase.finish().is_err());
    assert!(phase.transaction().is_err());
    assert!(transaction.finish().is_err());
    assert_eq!(budget.report()?.observed_bytes, exact.0);
    let last = first.clone();
    drop(outer_entry);
    assert_eq!(snapshot(&db)?, before);
    drop((transaction, phase));
    let held = add(header(), timestamps(5));
    assert_eq!(budget.report()?.observed_bytes, held.0);
    drop(first);
    assert_eq!(budget.report()?.observed_bytes, held.0);
    drop(last);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn assignment_borrowed_identity_and_missing_table_are_zero_cost_short_circuits() -> Result<()> {
    let (_home, db) = fixture(false, false)?;
    let agent = "身份".repeat(2000);
    let turn = "回合".repeat(2000);
    db.transaction(|tx| assign_input_to_turn_tx(tx, &agent, "long-message", &turn, now()))?;
    let before = snapshot(&db)?;
    let budget = ledger()?;
    let exact = add(header(), header());
    let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
    let transaction = phase.transaction()?;
    {
        let _entry = transaction.enter();
        db.transaction(|tx| {
            assert!(!assign_input_to_turn_tx(
                tx,
                &agent,
                "long-message",
                &turn,
                now()
            )?);
            for (agent, turn) in [
                ("wrong-agent", turn.as_str()),
                (agent.as_str(), "wrong-turn"),
            ] {
                let error =
                    assign_input_to_turn_tx(tx, agent, "long-message", turn, now()).unwrap_err();
                assert!(error.to_string().contains("assignment identity conflict"));
            }
            Ok(())
        })?;
        let mut legacy = Connection::open_in_memory()?;
        let tx = legacy.transaction()?;
        assert!(!assign_input_to_turn_tx(&tx, "a", "m", "t", now())?);
    }
    assert_eq!(snapshot(&db)?, before);
    assert_eq!(transaction.finish()?, header());
    assert_eq!(phase.finish()?, exact);
    drop((transaction, phase));
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
