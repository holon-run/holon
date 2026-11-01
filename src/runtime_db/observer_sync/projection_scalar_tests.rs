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

const AGENT: &str = "scalar-member";

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "projection-scalar".into(),
        identity_incarnation: 1,
    }
}

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn cost(text: &str) -> (u64, usize) {
    (((text.len() + std::mem::size_of::<String>()) as u64) * 8, 8)
}

fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}

// Independent costs for this public named fixture's canonical projection.
fn canonical_costs(connection: &Connection) -> Result<Vec<(u64, usize)>> {
    let payload: String = connection.query_row(
        "SELECT payload_json FROM agent_identities WHERE agent_id = ?1",
        [AGENT],
        |row| row.get(0),
    )?;
    let mut quoted = false;
    let mut escaped = false;
    let mut slots = 0;
    for byte in payload.bytes() {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else {
            slots += usize::from(matches!(
                byte,
                b'"' | b'{' | b'}' | b'[' | b']' | b':' | b','
            ));
            quoted = byte == b'"';
        }
    }
    Ok(vec![
        cost(&payload),
        (
            (payload.len() as u64 * 4 + slots as u64 * 128 + 512) * 8,
            (slots * 4 + 16) * 8,
        ),
        cost(AGENT), // durability
        cost(AGENT), // lifecycle attachment
        (
            (std::mem::size_of::<crate::types::AgentCapabilityPolicyRule>() * 6) as u64 * 8,
            6 * 8,
        ),
        cost(AGENT), // capability policy
        (
            (std::mem::size_of::<crate::types::AgentMessagePolicyRule>() * 2) as u64 * 8,
            2 * 8,
        ),
        cost("operator_control"),
        cost("runtime:agent-invocation"),
        cost("agent_invocation"),
        cost(AGENT), // message policy
        cost(AGENT), // projection
    ])
}

fn fixture() -> Result<(tempfile::TempDir, crate::runtime_db::RuntimeDb)> {
    let home = tempfile::tempdir()?;
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    db.agent_identities().upsert(&AgentIdentityRecord::new(
        AGENT,
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
         VALUES (?1, 'idle', '2026-01-01T00:00:00Z', '{\"state\":\"汉字\"}')",
        [AGENT],
    )?;
    connection.execute(
        "INSERT INTO work_items (work_item_id, agent_id, state, objective,
          plan_status, revision, current_focus, created_at, updated_at, payload_json)
         VALUES ('work', ?1, 'open', 'objective', 'ready', 7, 1,
          '2026-01-01T00:00:00Z', '2026-01-02T00:00:00Z', '{}')",
        [AGENT],
    )?;
    for table in ["messages", "transcript_entries", "briefs"] {
        connection.execute(
            &format!(
                "INSERT INTO {table} (evidence_id, agent_id, created_at, kind, preview, payload_json)
                 VALUES (?1, ?2, '2026-01-03T00:00:00Z', 'fixture', ?3, '{{}}')"
            ),
            rusqlite::params![table, AGENT, "汉字\0\"\\\n"],
        )?;
    }
    Ok((home, db))
}

// Scalar copies and the intervening canonical projection, in allocation order.
fn costs(connection: &Connection) -> Result<Vec<(u64, usize)>> {
    let queries = [
        "SELECT value FROM runtime_metadata WHERE key = 'runtime_id'",
        "SELECT value FROM runtime_metadata WHERE key = 'event_log_epoch'",
        "SELECT value FROM runtime_metadata WHERE key = 'visibility_policy_generation'",
        "SELECT payload_json FROM agent_identities WHERE agent_id = 'scalar-member'",
        "SELECT payload_json FROM agent_states WHERE agent_id = 'scalar-member'",
        "SELECT work_item_id FROM work_items WHERE agent_id = 'scalar-member'",
        "SELECT state FROM work_items WHERE agent_id = 'scalar-member'",
        "SELECT plan_status FROM work_items WHERE agent_id = 'scalar-member'",
        "SELECT updated_at FROM work_items WHERE agent_id = 'scalar-member'",
        "SELECT evidence_id FROM messages WHERE agent_id = 'scalar-member'",
        "SELECT evidence_id FROM transcript_entries WHERE agent_id = 'scalar-member'",
        "SELECT evidence_id FROM briefs WHERE agent_id = 'scalar-member'",
        "SELECT created_at FROM briefs WHERE agent_id = 'scalar-member'",
        "SELECT preview FROM briefs WHERE agent_id = 'scalar-member'",
    ];
    let mut costs = Vec::new();
    for (index, sql) in queries.into_iter().enumerate() {
        let text: String = connection.query_row(sql, [], |row| row.get(0))?;
        costs.push(cost(&text));
        if index == 4 {
            costs.extend(canonical_costs(connection)?);
        }
    }
    costs.push(cost(AGENT));
    Ok(costs)
}

#[test]
fn scalar_projection_exact_remainder_returns_original_holder_and_owning_clone() -> Result<()> {
    let (_home, db) = fixture()?;
    let baseline = db.agent_projection_snapshot_rows(AGENT)?;
    assert!(baseline._material.is_none());
    let total = costs(&db.connection()?)?.into_iter().fold((0, 0), add);
    let used = (31 * 8, 3 * 8);
    let exact = add(add(header(), used), total);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
    let holder = phase.guard()?;
    let snapshot = {
        let _entry = phase.enter();
        charge_owned_metadata(31, 3)?;
        db.agent_projection_snapshot_rows(AGENT)?
    };
    assert_eq!(snapshot, baseline);
    assert_eq!(phase.finish()?, exact);
    assert!(Arc::ptr_eq(snapshot._material.as_ref().unwrap(), &holder));
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let cloned = snapshot.clone();
    assert_eq!(cloned, snapshot);
    assert!(!Arc::ptr_eq(
        cloned._material.as_ref().unwrap(),
        snapshot._material.as_ref().unwrap(),
    ));
    assert_eq!(ledger.report()?.observed_bytes, exact.0 * 2);
    drop((phase, holder, snapshot));
    assert_eq!(ledger.report()?.observed_bytes, exact.0);
    assert_eq!(
        cloned
            .row
            .as_ref()
            .unwrap()
            .latest_brief
            .as_ref()
            .unwrap()
            .preview
            .as_deref(),
        Some("汉字\0\"\\\n")
    );
    drop(cloned);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn scalar_projection_rejects_each_copy_before_allocating_without_refunding_prefix() -> Result<()> {
    let (_home, db) = fixture()?;
    let used = (19 * 8, 2 * 8);
    let mut before = add(header(), used);
    for (index, next) in costs(&db.connection()?)?.into_iter().enumerate() {
        let exact = add(before, next);
        for byte_failure in [true, false] {
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                exact.0 - u64::from(byte_failure),
                exact.1 - usize::from(!byte_failure),
            )?;
            let holder = phase.guard()?;
            {
                let _entry = phase.enter();
                charge_owned_metadata(19, 2)?;
                let error = db.agent_projection_snapshot_rows(AGENT).unwrap_err();
                assert!(
                    format!("{error:#}").contains("ResourceExhausted"),
                    "{index}: {error:#}"
                );
                assert!(ResourceReadMaterial::active_guard().is_err());
            }
            assert!(phase.finish().is_err());
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            assert_eq!(ledger.report()?.observed_bytes, before.0, "copy {index}");
            assert!(db.connection()?.is_autocommit());
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, before.0);
            drop(holder);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        before = exact;
    }
    Ok(())
}

#[test]
fn scalar_projection_sql_type_error_rolls_back_and_retries_on_same_phase() -> Result<()> {
    let (_home, db) = fixture()?;
    let baseline = db.agent_projection_snapshot_rows(AGENT)?;
    let ordered = costs(&db.connection()?)?;
    let canonical_steps = canonical_costs(&db.connection()?)?.len();
    let total = ordered.iter().copied().fold((0, 0), add);
    let mut connection = db.connection()?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let holder = phase.guard()?;
    let mut expected = header();
    // Every optional/required scalar family, including a failure after prior
    // text copies. The identity JSON remains valid for canonical relations.
    for (table, column, index) in [
        ("agent_states", "payload_json", 4),
        ("work_items", "work_item_id", 5),
        ("work_items", "state", 6),
        ("work_items", "plan_status", 7),
        ("work_items", "updated_at", 8),
        ("messages", "evidence_id", 9),
        ("transcript_entries", "evidence_id", 10),
        ("briefs", "evidence_id", 11),
        ("briefs", "created_at", 12),
        ("briefs", "preview", 13),
    ] {
        let _entry = phase.enter();
        let tx = connection.transaction()?;
        tx.execute(&format!("UPDATE {table} SET {column} = x'ff'"), [])?;
        assert!(
            collect_agent_projection_anchors(&tx, AGENT).is_err(),
            "{table}.{column}"
        );
        let index = index + if index >= 5 { canonical_steps } else { 0 };
        expected = ordered[..index].iter().copied().fold(expected, add);
        assert_eq!(phase.finish()?, expected);
        tx.rollback()?;
        let restored = collect_agent_projection_anchors(&connection, AGENT)?;
        assert_eq!(restored, baseline);
        assert!(Arc::ptr_eq(restored._material.as_ref().unwrap(), &holder));
        expected = add(expected, total);
        assert_eq!(phase.finish()?, expected);
    }
    assert!(connection.is_autocommit());
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, expected.0);
    drop(holder);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn scalar_projection_nulls_and_hidden_members_allocate_only_selected_text() -> Result<()> {
    let (_home, db) = fixture()?;
    let mut connection = db.connection()?;
    let metadata = costs(&connection)?[..3].iter().copied().fold((0, 0), add);
    for status in ["private", "deleted", "unknown"] {
        let tx = connection.transaction()?;
        match status {
            "private" => {
                tx.execute("UPDATE agent_identities SET visibility = 'private'", [])?;
            }
            "deleted" => {
                tx.execute("UPDATE agent_identities SET status = 'deleted'", [])?;
            }
            _ => {
                tx.execute("DELETE FROM agent_identities", [])?;
            }
        }
        let exact = add(header(), metadata);
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
        let snapshot = {
            let _entry = phase.enter();
            collect_agent_projection_anchors(&tx, AGENT)?
        };
        assert!(snapshot.row.is_none());
        assert_eq!(phase.finish()?, exact);
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, exact.0);
        drop(snapshot);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        tx.rollback()?;
    }
    let total = costs(&connection)?.into_iter().fold((0, 0), add);
    let state: String =
        connection.query_row("SELECT payload_json FROM agent_states", [], |r| r.get(0))?;
    connection.execute_batch(
        "DELETE FROM agent_states;
         UPDATE work_items SET current_focus = 0, plan_status = NULL;
         UPDATE briefs SET preview = NULL;",
    )?;
    let omitted = [cost(&state), cost("ready"), cost("汉字\0\"\\\n")]
        .into_iter()
        .fold((0, 0), add);
    let exact = (
        header().0 + total.0 - omitted.0,
        header().1 + total.1 - omitted.1,
    );
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
    let snapshot = {
        let _entry = phase.enter();
        collect_agent_projection_anchors(&connection, AGENT)?
    };
    let row = snapshot.row.as_ref().unwrap();
    assert!(row.agent_state_json.is_none());
    assert!(row
        .current_work_item
        .as_ref()
        .unwrap()
        .plan_status
        .is_none());
    assert!(row.latest_brief.as_ref().unwrap().preview.is_none());
    assert_eq!(phase.finish()?, exact);
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, exact.0);
    drop(snapshot);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
