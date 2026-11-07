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
        agent_id: "revision-budget-agent".into(),
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
    // Independent literal wire shape: a quoted DateTime string, one AST slot.
    let bytes = r#""2026-10-17T14:42:23.123456789Z""#.len() as u64;
    (
        (bytes * 4 + 128 + 512) * 8 * count as u64,
        (4 + 16) * 8 * count,
    )
}

fn now() -> DateTime<Utc> {
    "2026-10-17T14:42:23.123456789Z".parse().unwrap()
}

fn fixture() -> Result<(tempfile::TempDir, RuntimeDb)> {
    let home = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    db.transaction(|tx| {
        ensure_turn_revision_tx(tx, &scope().agent_id, "turn", now())?;
        bump_source_revision_tx(tx, SOURCE_WAIT, "source", &scope().agent_id, None, now())?;
        Ok(())
    })?;
    Ok((home, db))
}

fn ledger() -> Result<InputEvidenceBudgetLedger> {
    Ok(InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?)
}

fn snapshot(db: &RuntimeDb) -> Result<(String, String)> {
    db.transaction(|tx| {
        Ok((
            tx.query_row(
                "SELECT json_group_array(json_array(agent_id,turn_id,summary_revision,
                 detail_revision,result_settled,updated_at)) FROM
                 (SELECT * FROM conversation_turn_revisions ORDER BY agent_id,turn_id)",
                [],
                |row| row.get(0),
            )?,
            tx.query_row(
                "SELECT json_group_array(json_array(source_kind,source_id,agent_id,turn_id,
                 activity_seq,revision,updated_at)) FROM
                 (SELECT * FROM conversation_source_revisions ORDER BY source_kind,source_id)",
                [],
                |row| row.get(0),
            )?,
        ))
    })
}

fn route(tx: &Transaction<'_>, index: usize) -> Result<()> {
    let agent = scope().agent_id;
    match index {
        0 => ensure_turn_revision_tx(tx, &agent, "new-turn", now()).map(|_| ()),
        1 => bump_turn_revision_tx(tx, &agent, "turn", true, true, now()).map(|_| ()),
        2 => bump_turn_revision_tx(tx, &agent, "new-turn", false, false, now()).map(|_| ()),
        3 => settle_turn_result_tx(tx, &agent, "turn", now()).map(|_| ()),
        4 => settle_turn_result_tx(tx, &agent, "new-turn", now()).map(|_| ()),
        5 => {
            bump_source_revision_tx(tx, SOURCE_WAIT, "new-source", &agent, None, now()).map(|_| ())
        }
        6 => bump_source_revision_tx(tx, SOURCE_WAIT, "source", &agent, Some("turn"), now())
            .map(|_| ()),
        _ => unreachable!(),
    }
}

#[test]
fn revision_helpers_share_exact_original_allowance_and_last_transaction_holder() -> Result<()> {
    let (_home, db) = fixture()?;
    let budget = ledger()?;
    let exact = add(add(header(), header()), timestamps(7));
    let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
    let transaction = phase.transaction()?;
    let outer_entry = phase.enter();
    {
        let _entry = transaction.enter();
        db.transaction(|tx| {
            for index in [0, 1, 3, 5, 6] {
                route(tx, index)?;
            }
            // Missing-turn settlement allocates once for ensure, then once for settle.
            settle_turn_result_tx(tx, &scope().agent_id, "another-turn", now())?;
            Ok(())
        })?;
    }
    assert_eq!(transaction.finish()?, add(header(), timestamps(7)));
    assert_eq!(phase.finish()?, exact);
    let first = transaction.guard()?;
    let last = first.clone();
    assert!(std::sync::Arc::ptr_eq(&first, &last));
    let before = snapshot(&db)?;
    // The inner TLS entry restored the original owner, with no new allowance.
    let error = db.transaction(|tx| route(tx, 6)).unwrap_err();
    assert!(format!("{error:#}")
        .to_lowercase()
        .contains("resourceexhausted"));
    assert_eq!(snapshot(&db)?, before);
    assert!(phase.finish().is_err());
    assert!(phase.transaction().is_err());
    drop(outer_entry);
    drop((phase, transaction));
    let held = add(header(), timestamps(7));
    assert_eq!(budget.report()?.observed_bytes, held.0);
    drop(first);
    assert_eq!(budget.report()?.observed_bytes, held.0);
    drop(last);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn every_revision_timestamp_gate_rejects_before_sql_and_rolls_back_prefix() -> Result<()> {
    for (index, count) in [1, 1, 1, 1, 2, 1, 1].into_iter().enumerate() {
        let (_home, db) = fixture()?;
        let before = snapshot(&db)?;
        for admitted in 0..count {
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
                    let error = db.transaction(|tx| route(tx, index)).unwrap_err();
                    assert!(format!("{error:#}")
                        .to_lowercase()
                        .contains("resourceexhausted"));
                    assert!(db.transaction(|tx| route(tx, index)).is_err());
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
    Ok(())
}

#[test]
fn no_write_and_borrowed_identity_fences_do_not_consume_timestamp_allowance() -> Result<()> {
    let (_home, db) = fixture()?;
    let long_turn = "路".repeat(2000);
    db.transaction(|tx| {
        settle_turn_result_tx(tx, &scope().agent_id, "turn", now())?;
        bump_source_revision_tx(
            tx,
            SOURCE_WAIT,
            "source",
            &scope().agent_id,
            Some(&long_turn),
            now(),
        )?;
        Ok(())
    })?;
    let before = snapshot(&db)?;
    let budget = ledger()?;
    let exact = add(header(), header());
    let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
    let transaction = phase.transaction()?;
    {
        let _entry = transaction.enter();
        db.transaction(|tx| {
            let agent = scope().agent_id;
            assert!(ensure_turn_revision_tx(tx, &agent, "turn", now())?.result_settled);
            bump_turn_revision_tx(tx, &agent, "turn", false, false, now())?;
            assert!(settle_turn_result_tx(tx, &agent, "turn", now())?.result_settled);
            for (agent, turn, expected) in [
                ("wrong-agent", Some(long_turn.as_str()), "changed agent"),
                (agent.as_str(), Some("wrong-turn"), "changed turn"),
            ] {
                let error = bump_source_revision_tx(tx, SOURCE_WAIT, "source", agent, turn, now())
                    .unwrap_err();
                assert!(error.to_string().contains(expected));
            }
            Ok(())
        })?;
        let mut empty = Connection::open_in_memory()?;
        let tx = empty.transaction()?;
        assert_eq!(ensure_turn_revision_tx(&tx, "a", "t", now())?.summary, 0);
        assert_eq!(
            bump_turn_revision_tx(&tx, "a", "t", true, true, now())?.detail,
            0
        );
        assert!(settle_turn_result_tx(&tx, "a", "t", now())?.result_settled);
        assert_eq!(
            bump_source_revision_tx(&tx, SOURCE_WAIT, "s", "a", None, now())?,
            0
        );
    }
    assert_eq!(snapshot(&db)?, before);
    assert_eq!(transaction.finish()?, header());
    assert_eq!(phase.finish()?, exact);
    drop((transaction, phase));
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn source_revision_preserves_null_binding_sequence_monotonicity_and_millisecond_format(
) -> Result<()> {
    let (_home, db) = fixture()?;
    db.transaction(|tx| {
        let agent = scope().agent_id;
        let seq: i64 = tx.query_row(
            "SELECT activity_seq FROM conversation_source_revisions WHERE source_id='source'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(
            bump_source_revision_tx(tx, SOURCE_WAIT, "source", &agent, Some("turn"), now())?,
            2
        );
        assert_eq!(
            bump_source_revision_tx(tx, SOURCE_WAIT, "source", &agent, None, now())?,
            3
        );
        let row: (String, i64, i64, String) = tx.query_row(
            "SELECT turn_id,activity_seq,revision,updated_at FROM conversation_source_revisions
             WHERE source_id='source'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        assert_eq!(
            row,
            ("turn".into(), seq, 3, "2026-10-17T14:42:23.123Z".into())
        );
        tx.execute(
            "UPDATE conversation_source_revisions SET activity_seq=NULL",
            [],
        )?;
        assert_eq!(
            bump_source_revision_tx(tx, SOURCE_WAIT, "source", &agent, None, now())?,
            4
        );
        assert_eq!(
            tx.query_row(
                "SELECT activity_seq FROM conversation_source_revisions WHERE source_id='source'",
                [],
                |row| row.get::<_, i64>(0),
            )?,
            1
        );
        let revision = bump_turn_revision_tx(tx, &agent, "turn", true, false, now())?;
        assert_eq!((revision.summary, revision.detail), (2, 1));
        let revision = settle_turn_result_tx(tx, &agent, "turn", now())?;
        assert_eq!(
            (revision.summary, revision.detail, revision.result_settled),
            (3, 1, true)
        );
        Ok(())
    })?;
    Ok(())
}
