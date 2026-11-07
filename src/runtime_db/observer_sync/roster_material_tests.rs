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
use std::sync::Arc;

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "raw-roster-material".into(),
        identity_incarnation: 1,
    }
}

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn add(left: (u64, usize), right: (u64, usize)) -> (u64, usize) {
    (left.0 + right.0, left.1 + right.1)
}

fn text(value: &str) -> (u64, usize) {
    ((value.len() + std::mem::size_of::<String>()) as u64 * 8, 8)
}

fn slot<T>() -> (u64, usize) {
    (std::mem::size_of::<T>() as u64 * 8, 8)
}

fn fixture() -> Result<(tempfile::TempDir, RuntimeDb)> {
    let home = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    db.agent_identities().upsert(&AgentIdentityRecord::new(
        "web",
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    ))?;
    let connection = db.connection()?;
    connection.execute(
        "INSERT INTO agent_states (agent_id, status, updated_at, payload_json)
         VALUES ('web', 'idle', '2026-01-01T00:00:00Z', '{\"state\":\"汉字\"}')",
        [],
    )?;
    connection.execute(
        "INSERT INTO briefs (evidence_id, agent_id, created_at, kind, preview, payload_json)
         VALUES ('brief', 'web', '2026-01-03T00:00:00Z', 'fixture', ?1, '{}')",
        ["汉字\0\"\\\n"],
    )?;
    drop(connection);
    Ok((home, db))
}

// Independent allocation-order oracle: query values without an active phase,
// then calculate envelopes directly, never by measuring the production reader.
fn gates(connection: &Connection) -> Result<Vec<(&'static str, (u64, usize))>> {
    let mut gates = Vec::new();
    for key in [
        "runtime_id",
        "event_log_epoch",
        "visibility_policy_generation",
    ] {
        let value: String = connection.query_row(
            "SELECT value FROM runtime_metadata WHERE key = ?1",
            [key],
            |row| row.get(0),
        )?;
        gates.push((key, text(&value)));
    }
    let (id, identity): (String, String) = connection.query_row(
        "SELECT agent_id, payload_json FROM agent_identities WHERE agent_id = 'web'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    gates.extend([
        ("identity-id", text(&id)),
        ("identity-json", text(&identity)),
        ("identities-slot", slot::<(String, String, i64, i64)>()),
    ]);
    let (id, brief, created, preview): (String, String, String, Option<String>) = connection
        .query_row(
        "SELECT agent_id, evidence_id, created_at, preview FROM briefs WHERE evidence_id = 'brief'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    gates.extend([
        ("brief-agent", text(&id)),
        ("brief-id", text(&brief)),
        ("brief-created", text(&created)),
    ]);
    if let Some(preview) = preview {
        gates.push(("brief-preview", text(&preview)));
    }
    gates.push(("brief-slot", slot::<(String, AgentRosterLatestBriefRow)>()));
    let (id, state): (String, String) = connection.query_row(
        "SELECT agent_id, payload_json FROM agent_states WHERE agent_id = 'web'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    gates.extend([
        ("state-agent", text(&id)),
        ("state-json", text(&state)),
        ("state-slot", slot::<(String, String)>()),
        ("rows-slot", slot::<AgentRosterRow>()),
    ]);
    Ok(gates)
}

#[test]
fn roster_all_sql_and_collection_gates_reject_before_charge_on_original_phase() -> Result<()> {
    let (_home, db) = fixture()?;
    let connection = db.connection()?;
    let gates = gates(&connection)?;
    drop(connection);
    assert_eq!(gates.len(), 15);
    let baseline = db.agent_roster_snapshot_rows()?;
    assert!(baseline._material.is_none());
    let mut before = header();
    for (label, gate) in gates {
        let full = add(before, gate);
        for byte_limit in [true, false] {
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                full.0 - u64::from(byte_limit),
                full.1 - usize::from(!byte_limit),
            )?;
            let holder = phase.guard()?;
            {
                let _entry = phase.enter();
                assert!(Arc::ptr_eq(
                    &ResourceReadMaterial::active_guard()?.unwrap(),
                    &holder
                ));
                let error = db.agent_roster_snapshot_rows().unwrap_err();
                assert!(
                    format!("{error:#}").contains("ResourceExhausted"),
                    "{label}: {error:#}"
                );
                assert_eq!(ledger.report()?.observed_bytes, before.0, "{label}");
                assert!(phase.finish().is_err(), "{label}");
                assert!(ResourceReadMaterial::active_guard().is_err(), "{label}");
                assert!(db.agent_roster_snapshot_rows().is_err(), "{label}");
                assert_eq!(ledger.report()?.observed_bytes, before.0, "{label}");
            }
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, before.0, "{label}");
            drop(holder);
            assert_eq!(ledger.report()?.observed_bytes, 0, "{label}");
        }
        before = full;
    }
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), before.0, before.1)?;
    let holder = phase.guard()?;
    let rows = {
        let _entry = phase.enter();
        db.agent_roster_snapshot_rows()?
    };
    assert_eq!(rows, baseline);
    assert_eq!(phase.finish()?, before);
    assert!(Arc::ptr_eq(rows._material.as_ref().unwrap(), &holder));
    let cloned = rows.clone();
    assert_eq!(cloned, rows);
    assert!(!Arc::ptr_eq(cloned._material.as_ref().unwrap(), &holder));
    assert_eq!(ledger.report()?.observed_bytes, before.0 * 2);
    drop((phase, holder, rows));
    assert_eq!(ledger.report()?.observed_bytes, before.0);
    drop(cloned);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn roster_sql_failure_rolls_back_and_retries_with_null_and_empty_membership() -> Result<()> {
    let (_home, db) = fixture()?;
    let connection = db.connection()?;
    let gates = gates(&connection)?;
    for (sql, refused) in [
        (
            "UPDATE agent_identities SET payload_json = X'ff'",
            "identity-json",
        ),
        ("UPDATE briefs SET evidence_id = X'ff'", "brief-id"),
        ("UPDATE briefs SET created_at = X'ff'", "brief-created"),
        ("UPDATE briefs SET preview = X'ff'", "brief-preview"),
        ("UPDATE agent_states SET payload_json = X'ff'", "state-json"),
    ] {
        connection.execute_batch("BEGIN; PRAGMA ignore_check_constraints = ON;")?;
        connection.execute(sql, [])?;
        let before = gates
            .iter()
            .take_while(|(label, _)| *label != refused)
            .fold(header(), |sum, (_, gate)| add(sum, *gate));
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            let error = collect_agent_roster_rows(&connection).unwrap_err();
            assert!(
                error.downcast_ref::<rusqlite::Error>().is_some(),
                "{error:#}"
            );
            assert!(
                !format!("{error:#}").contains("ResourceExhausted"),
                "{error:#}"
            );
            assert_eq!(phase.finish()?, before, "{refused}");
            connection.execute_batch("ROLLBACK; PRAGMA ignore_check_constraints = OFF;")?;
            let rows = collect_agent_roster_rows(&connection)?;
            assert!(Arc::ptr_eq(rows._material.as_ref().unwrap(), &holder));
            let complete = gates.iter().fold((0, 0), |sum, (_, gate)| add(sum, *gate));
            assert_eq!(phase.finish()?, add(before, complete));
        }
        drop((phase, holder));
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    for preview in [None, Some("")] {
        connection.execute("UPDATE briefs SET preview = ?1", [preview])?;
        let expected = gates_for_current_preview(&connection)?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let rows = {
            let _entry = phase.enter();
            collect_agent_roster_rows(&connection)?
        };
        assert_eq!(
            rows.rows[0]
                .latest_brief
                .as_ref()
                .unwrap()
                .preview
                .as_deref(),
            preview
        );
        assert_eq!(phase.finish()?, expected);
        drop((rows, phase));
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    connection.execute("UPDATE agent_identities SET visibility = 'private'", [])?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let rows = {
        let _entry = phase.enter();
        collect_agent_roster_rows(&connection)?
    };
    assert!(rows.rows.is_empty());
    let metadata = gates
        .iter()
        .take(3)
        .fold(header(), |sum, (_, gate)| add(sum, *gate));
    assert_eq!(phase.finish()?, metadata);
    drop((rows, phase));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

fn gates_for_current_preview(connection: &Connection) -> Result<(u64, usize)> {
    Ok(gates(connection)?
        .iter()
        .fold(header(), |sum, (_, gate)| add(sum, *gate)))
}
