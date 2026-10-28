use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    types::AgentVisibility,
};
use std::sync::Arc;

const PARENT: &str = "parent-汉字";
const CANONICAL: &str = "a-canonical\0\"\\\n";
const LEGACY: &str = "z-legacy";

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "lineage-children-material".into(),
        identity_incarnation: 1,
    }
}

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}

fn text_cost(text: &str) -> (u64, usize) {
    (((text.len() + std::mem::size_of::<String>()) as u64) * 8, 8)
}

fn slot_cost<T>(count: usize) -> (u64, usize) {
    ((count * std::mem::size_of::<T>()) as u64 * 8, count * 8)
}

fn json_cost(payload: &str) -> (u64, usize) {
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
    (
        (payload.len() as u64 * 4 + slots as u64 * 128 + 512) * 8,
        (slots * 4 + 16) * 8,
    )
}

fn fixture() -> Result<(tempfile::TempDir, crate::runtime_db::RuntimeDb)> {
    let home = tempfile::tempdir()?;
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    let now = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00.123Z")?.with_timezone(&Utc);
    for id in [PARENT, CANONICAL, LEGACY] {
        let mut identity = AgentIdentityRecord::new(
            id,
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            (id != PARENT).then(|| PARENT.to_owned()),
            None,
        );
        identity.created_at = now;
        identity.updated_at = now;
        db.agent_identities().upsert(&identity)?;
    }
    db.agent_canonical_relations()
        .upsert_lineage(&AgentLineageRecord {
            child_agent_id: CANONICAL.into(),
            parent_agent_id: PARENT.into(),
            creation_cause: AgentLineageCreationCause::Migration,
            revision: 7,
            created_at: now,
        })?;
    Ok((home, db))
}

// Independently enumerate each owning admission, in reader order.
fn steps(db: &crate::runtime_db::RuntimeDb) -> Result<Vec<(u64, usize)>> {
    let connection = db.connection()?;
    let payload: String = connection.query_row(
        "SELECT payload_json FROM agent_lineages WHERE child_agent_id = ?1",
        [CANONICAL],
        |row| row.get(0),
    )?;
    let mut costs = vec![
        json_cost(&payload),
        slot_cost::<(String, usize, AgentLineageRecord)>(1),
        text_cost(CANONICAL),
    ];
    let mut statement =
        connection.prepare("SELECT payload_json FROM agent_identities ORDER BY agent_id")?;
    let identities = statement.query_map([], |row| row.get::<_, String>(0))?;
    for payload in identities {
        let payload = payload?;
        let identity: AgentIdentityRecord = serde_json::from_str(&payload)?;
        costs.push(json_cost(&payload));
        if identity.agent_id != PARENT {
            costs.push(text_cost(&identity.agent_id));
            costs.push(text_cost(PARENT));
            if identity.agent_id == LEGACY {
                costs.push(slot_cost::<(String, usize, AgentLineageRecord)>(1));
            }
        }
    }
    costs.push(slot_cost::<AgentLineageRecord>(2));
    Ok(costs)
}

#[test]
fn lineage_children_exact_cost_precedes_every_allocation_and_last_holder_drop() -> Result<()> {
    let (_home, db) = fixture()?;
    let expected = steps(&db)?.into_iter().fold(header(), add);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), expected.0, expected.1)?;
    let holder = phase.guard()?;
    let snapshot = {
        let _entry = phase.enter();
        db.agent_canonical_relations().lineage_children(PARENT)?
    };
    assert_eq!(phase.finish()?, expected);
    assert!(Arc::ptr_eq(snapshot._material.as_ref().unwrap(), &holder));
    assert_eq!(snapshot.len(), 2);
    assert_eq!(snapshot[0].child_agent_id, CANONICAL);
    assert_eq!(snapshot[0].revision, 7, "canonical wins over legacy");
    assert_eq!(snapshot[1].child_agent_id, LEGACY);
    assert_eq!(
        snapshot[1].creation_cause,
        AgentLineageCreationCause::LegacySpawn
    );
    let cloned = snapshot.clone();
    assert_eq!(snapshot, cloned);
    assert!(!Arc::ptr_eq(cloned._material.as_ref().unwrap(), &holder));
    assert_eq!(ledger.report()?.observed_bytes, expected.0 * 2);
    drop((phase, holder, snapshot));
    assert_eq!(ledger.report()?.observed_bytes, expected.0);
    assert_eq!(cloned[0].revision, 7);
    drop(cloned);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn lineage_children_rejects_each_bytes_and_records_boundary_without_partial_charge() -> Result<()> {
    let (_home, db) = fixture()?;
    let steps = steps(&db)?;
    let total = steps.iter().copied().fold(header(), add);
    for bytes in [true, false] {
        let mut prefix = header();
        for (index, step) in steps.iter().copied().enumerate() {
            let next = add(prefix, step);
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                if bytes { next.0 - 1 } else { total.0 },
                if bytes { total.1 } else { next.1 - 1 },
            )?;
            {
                let _entry = phase.enter();
                let error = db
                    .agent_canonical_relations()
                    .lineage_children(PARENT)
                    .unwrap_err();
                assert!(
                    format!("{error:#}").contains("ResourceExhausted"),
                    "step {index}, bytes={bytes}: {error:#}"
                );
            }
            assert!(phase.finish().is_err());
            assert_eq!(ledger.report()?.observed_bytes, prefix.0, "step {index}");
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, 0);
            prefix = next;
        }
    }
    Ok(())
}

#[test]
fn lineage_children_sql_and_json_errors_retry_on_original_phase() -> Result<()> {
    let (_home, db) = fixture()?;
    let baseline = db.agent_canonical_relations().lineage_children(PARENT)?;
    assert!(baseline._material.is_none());
    let success = steps(&db)?.into_iter().fold(header(), add);
    for (table, expression) in [
        ("agent_lineages", "'{'"),
        ("agent_lineages", "CAST(payload_json AS BLOB)"),
        ("agent_identities", "'{'"),
        ("agent_identities", "CAST(payload_json AS BLOB)"),
    ] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let holder = phase.guard()?;
        let connection = db.connection()?;
        let original = connection
            .prepare(&format!("SELECT rowid, payload_json FROM {table}"))?
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        connection.execute_batch("PRAGMA ignore_check_constraints=ON;")?;
        connection.execute(
            &format!("UPDATE {table} SET payload_json = {expression}"),
            [],
        )?;
        connection.execute_batch("PRAGMA ignore_check_constraints=OFF;")?;
        let failed = {
            let _entry = phase.enter();
            db.agent_canonical_relations().lineage_children(PARENT)
        };
        for (rowid, payload) in original {
            connection.execute(
                &format!("UPDATE {table} SET payload_json = ?1 WHERE rowid = ?2"),
                params![payload, rowid],
            )?;
        }
        let error = failed.unwrap_err();
        if expression.starts_with("CAST") {
            assert!(
                matches!(
                    error.downcast_ref::<rusqlite::Error>(),
                    Some(rusqlite::Error::InvalidColumnType(index, name, rusqlite::types::Type::Blob))
                        if *index == usize::from(table == "agent_lineages") && name == "payload_json"
                ),
                "{table}: {error:#}"
            );
        } else {
            assert!(
                format!("{error:#}").contains("decoding"),
                "{table}: {error:#}"
            );
        }
        let prefix = phase.finish()?;
        let snapshot = {
            let _entry = phase.enter();
            db.agent_canonical_relations().lineage_children(PARENT)?
        };
        assert_eq!(snapshot, baseline);
        assert!(Arc::ptr_eq(snapshot._material.as_ref().unwrap(), &holder));
        assert_eq!(
            phase.finish()?,
            (
                prefix.0 + success.0 - header().0,
                prefix.1 + success.1 - header().1
            )
        );
        let used = phase.finish()?.0;
        drop((phase, holder));
        assert_eq!(ledger.report()?.observed_bytes, used);
        drop(snapshot);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn lineage_children_preserves_sql_key_precedence_and_payload_sorting() -> Result<()> {
    let (_home, db) = fixture()?;
    let connection = db.connection()?;
    // Canonical merge keys are SQL IDs, not decoded payload IDs.
    connection.execute(
        "UPDATE agent_lineages SET payload_json =
         json_set(payload_json, '$.child_agent_id', 'zz-payload')",
        [],
    )?;
    let children = db.agent_canonical_relations().lineage_children(PARENT)?;
    assert_eq!(
        children
            .iter()
            .map(|lineage| lineage.child_agent_id.as_str())
            .collect::<Vec<_>>(),
        [LEGACY, "zz-payload"]
    );
    assert_eq!(children[1].revision, 7);
    // Distinct SQL identities can decode to the same legacy key; first wins.
    connection.execute(
        "UPDATE agent_identities SET payload_json =
         json_set(payload_json, '$.agent_id', ?1, '$.created_at', '2026-01-02T00:00:00Z')
         WHERE agent_id = ?2",
        params![LEGACY, CANONICAL],
    )?;
    let children = db.agent_canonical_relations().lineage_children(PARENT)?;
    assert_eq!(children.len(), 2);
    assert_eq!(children[0].child_agent_id, LEGACY);
    assert_eq!(children[0].revision, 0);
    assert_eq!(
        children[0].created_at.to_rfc3339(),
        "2026-01-02T00:00:00+00:00"
    );
    Ok(())
}

#[test]
fn lineage_children_empty_snapshot_keeps_array_schema_and_original_holder() -> Result<()> {
    let home = tempfile::tempdir()?;
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), header().0, header().1)?;
    let holder = phase.guard()?;
    let snapshot = {
        let _entry = phase.enter();
        db.agent_canonical_relations().lineage_children(PARENT)?
    };
    assert!(snapshot.is_empty());
    assert!(Arc::ptr_eq(snapshot._material.as_ref().unwrap(), &holder));
    assert_eq!(phase.finish()?, header());
    let wire = serde_json::to_string(&snapshot)?;
    assert_eq!(wire, "[]");
    let decoded: AgentLineageChildrenSnapshot = serde_json::from_str(&wire)?;
    assert_eq!(decoded, snapshot);
    assert!(decoded._material.is_none());
    let schema = serde_json::to_value(schemars::schema_for!(AgentLineageChildrenSnapshot))?;
    assert_eq!(schema["type"], "array", "{schema}");
    assert!(schema.get("properties").is_none());
    drop((phase, holder));
    assert_eq!(ledger.report()?.observed_bytes, header().0);
    drop(snapshot);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
