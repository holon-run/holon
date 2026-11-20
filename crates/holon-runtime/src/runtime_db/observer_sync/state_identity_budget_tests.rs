use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
};
use std::sync::Arc;

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "state-identity-verifier".into(),
        identity_incarnation: 1,
    }
}

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}

fn wire_cost(payload: &str) -> Result<(u64, usize)> {
    // Independent AST formula for these unique-key valid JSON fixtures.
    fn slots(value: &serde_json::Value) -> usize {
        match value {
            serde_json::Value::Object(fields) => {
                2 + fields.len().saturating_sub(1)
                    + fields.values().map(|value| 2 + slots(value)).sum::<usize>()
            }
            serde_json::Value::Array(values) => {
                2 + values.len().saturating_sub(1) + values.iter().map(slots).sum::<usize>()
            }
            serde_json::Value::String(_) => 1,
            _ => 0,
        }
    }
    let slots = slots(&serde_json::from_str(payload)?);
    Ok((
        (payload.len() as u64 * 4 + slots as u64 * 128 + 512) * 8,
        (slots * 4 + 16) * 8,
    ))
}

fn connection() -> Result<Connection> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch("CREATE TABLE agent_states (agent_id TEXT, payload_json TEXT)")?;
    Ok(connection)
}

fn insert(connection: &Connection, id: &str, payload: &str) -> Result<()> {
    connection.execute(
        "INSERT INTO agent_states (agent_id, payload_json) VALUES (?1, ?2)",
        [id, payload],
    )?;
    Ok(())
}

fn bodies() -> [&'static str; 2] {
    [
        r#"{"id":"one","extra":[null,{"quoted":"\\\"{}[]:, 路"}]}"#,
        r#"{"id":"not-two","extra":[]}"#,
    ]
}

#[test]
fn scalar_scan_preserves_json_identity_and_malformed_payload_semantics() -> Result<()> {
    let connection = connection()?;
    for (id, payload) in [
        ("one", r#"{"id":"one","unknown":{"nested":[1,true]}}"#),
        ("路\"{}", r#"{"id":"\u8def\"{}"}"#),
        ("", r#"{"id":""}"#),
        ("last", r#"{"id":"wrong","id":"last"}"#),
        ("different", r#"{"id":"other"}"#),
        ("missing", "{}"),
        ("number", r#"{"id":42}"#),
        ("null-id", r#"{"id":null}"#),
        ("root-null", "null"),
        ("array", r#"[{"id":"array"}]"#),
        ("string", r#""string""#),
        ("malformed", r#"{"id":"malformed","unfinished":["#),
        ("last-wrong", r#"{"id":"last-wrong","id":"other"}"#),
    ] {
        insert(&connection, id, payload)?;
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    assert_eq!(count_agent_state_identity_mismatches(&connection)?, 9);
    assert_eq!(count_agent_state_identity_mismatches(&connection)?, 9);
    assert!(connection.is_autocommit());
    Ok(())
}

#[test]
fn scalar_scan_uses_original_transaction_allowance_tls_and_last_holder() -> Result<()> {
    let connection = connection()?;
    let bodies = bodies();
    insert(&connection, "one", bodies[0])?;
    insert(&connection, "two", bodies[1])?;
    let budget = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let prior = (37 * 8, 3 * 8);
    let decoded = add(wire_cost(bodies[0])?, wire_cost(bodies[1])?);
    let exact = add(add(add(header(), header()), prior), decoded);
    let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
    {
        let _active = phase.enter();
        charge_owned_metadata(37, 3)?;
    }
    let transaction = phase.transaction()?;
    {
        let _outer = phase.enter();
        {
            let _active = transaction.enter();
            assert_eq!(count_agent_state_identity_mismatches(&connection)?, 1);
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &transaction.guard()?,
            ));
        }
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &phase.guard()?,
        ));
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    charge_owned_metadata(1, 1)?;
    assert_eq!(phase.finish()?, exact);
    assert_eq!(transaction.finish()?, add(header(), decoded));
    assert_eq!(budget.report()?.observed_bytes, exact.0);
    let first = Arc::new(vec![phase.guard()?, transaction.guard()?]);
    let last = first.clone();
    drop((phase, transaction));
    assert_eq!(budget.report()?.observed_bytes, exact.0);
    drop(first);
    assert_eq!(budget.report()?.observed_bytes, exact.0);
    drop(last);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn scalar_scan_rejects_each_decode_before_later_rows_without_swallowing_budget_errors() -> Result<()>
{
    let connection = connection()?;
    let bodies = bodies();
    insert(&connection, "one", bodies[0])?;
    insert(&connection, "two", bodies[1])?;
    // A later SQLite type error must not mask the earlier admission rejection.
    connection.execute("INSERT INTO agent_states VALUES ('poison', X'7b7d')", [])?;
    let costs = [wire_cost(bodies[0])?, wire_cost(bodies[1])?];
    for gate in 0..2 {
        let before = costs[..gate].iter().copied().fold(header(), add);
        let through = add(before, costs[gate]);
        for records in [false, true] {
            let budget = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(
                &budget,
                &scope(),
                through.0 - u64::from(!records),
                through.1 - usize::from(records),
            )?;
            let holder = phase.guard()?;
            {
                let _active = phase.enter();
                let error = count_agent_state_identity_mismatches(&connection).unwrap_err();
                assert!(
                    format!("{error:#}").contains("ResourceExhausted"),
                    "{error:#}"
                );
                assert!(ResourceReadMaterial::active_guard().is_err());
            }
            assert!(phase.finish().is_err());
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            assert_eq!(budget.report()?.observed_bytes, before.0);
            assert_eq!(
                count_rows(&connection, "SELECT COUNT(*) FROM agent_states")?,
                3
            );
            drop(phase);
            assert_eq!(budget.report()?.observed_bytes, before.0);
            drop(holder);
            assert_eq!(budget.report()?.observed_bytes, 0);
        }
    }
    // Malformed JSON normally counts as a mismatch, but may not swallow a
    // failed admission before the parser has attempted to construct a Value.
    connection.execute("DELETE FROM agent_states", [])?;
    insert(&connection, "bad", r#"{"id":"bad","unfinished":["#)?;
    let budget = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), header().0, header().1)?;
    {
        let _active = phase.enter();
        let error = count_agent_state_identity_mismatches(&connection).unwrap_err();
        assert!(format!("{error:#}").contains("ResourceExhausted"));
    }
    drop(phase);
    assert_eq!(budget.report()?.observed_bytes, 0);
    assert_eq!(count_agent_state_identity_mismatches(&connection)?, 1);
    Ok(())
}

#[test]
fn scalar_scan_sql_and_text_errors_release_rows_and_restore_original_phase() -> Result<()> {
    for values in [
        "NULL, '{}'",
        "X'6964', '{}'",
        "CAST(X'ff' AS TEXT), '{}'",
        "'id', NULL",
        "'id', X'7b7d'",
        "'id', CAST(X'ff' AS TEXT)",
    ] {
        let connection = connection()?;
        let body = bodies()[0];
        insert(&connection, "one", body)?;
        connection.execute(&format!("INSERT INTO agent_states VALUES ({values})"), [])?;
        let expected = add(header(), wire_cost(body)?);
        let budget = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase =
            ResourceReadMaterial::with_test_limits(&budget, &scope(), expected.0, expected.1)?;
        {
            let _active = phase.enter();
            let error = count_agent_state_identity_mismatches(&connection).unwrap_err();
            assert!(
                !format!("{error:#}").contains("ResourceExhausted"),
                "{error:#}"
            );
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &phase.guard()?,
            ));
        }
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        assert_eq!(phase.finish()?, expected);
        connection.execute("DROP TABLE agent_states", [])?;
        assert!(count_agent_state_identity_mismatches(&connection).is_err());
        let first = phase.guard()?;
        let last = first.clone();
        drop((phase, first));
        assert_eq!(budget.report()?.observed_bytes, expected.0);
        drop(last);
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn actual_verifier_propagates_admission_before_persistence_and_keeps_mismatch_detail() -> Result<()>
{
    let home = tempfile::tempdir()?;
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    let mut connection = db.connection()?;
    connection.execute(
        "INSERT INTO agent_states (agent_id, status, updated_at, payload_json)
         VALUES ('one', 'idle', '2026-10-17T19:30:00.000Z', ?1)",
        [serde_json::to_string(&crate::types::AgentState::new(
            "other",
        ))?],
    )?;
    let before = count_rows(
        &connection,
        "SELECT COUNT(*) FROM observer_sync_capability_verifications",
    )?;
    connection.execute(
        "UPDATE observer_sync_capability_verifications SET verified_at = 'sentinel'",
        [],
    )?;
    let budget = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), header().0, header().1)?;
    {
        let _active = phase.enter();
        let error = verify_observer_sync_foundations(&mut connection).unwrap_err();
        assert!(
            format!("{error:#}").contains("ResourceExhausted"),
            "{error:#}"
        );
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    assert!(phase.finish().is_err());
    assert_eq!(
        count_rows(
            &connection,
            "SELECT COUNT(*) FROM observer_sync_capability_verifications
             WHERE verified_at = 'sentinel'",
        )?,
        before,
    );
    drop(phase);
    assert_eq!(budget.report()?.observed_bytes, 0);
    for (id, expected) in [("other", 1), ("one", 0)] {
        connection.execute(
            "UPDATE agent_states SET payload_json = ?1 WHERE agent_id = 'one'",
            [serde_json::to_string(&crate::types::AgentState::new(id))?],
        )?;
        verify_observer_sync_foundations(&mut connection)?;
        let detail: String = connection.query_row(
            "SELECT detail FROM observer_sync_capability_verifications WHERE capability = ?1",
            [AGENT_IDENTITY_RESERVED],
            |row| row.get(0),
        )?;
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&detail)?["agent_state_identity_mismatch"],
            expected,
        );
        assert!(connection.is_autocommit());
    }
    Ok(())
}
