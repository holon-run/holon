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

const SOURCES: &[&str] = &[
    "agent_identities",
    "agent_states",
    "audit_events",
    "audit_event_retention_watermarks",
    "runtime_sequences",
    "briefs",
    "work_items",
    "messages",
    "transcript_entries",
];

fn fixture() -> Result<(tempfile::TempDir, crate::runtime_db::RuntimeDb)> {
    let home = tempfile::tempdir()?;
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    Ok((home, db))
}

fn member(db: &crate::runtime_db::RuntimeDb, id: &str) -> Result<()> {
    db.agent_identities().upsert(&AgentIdentityRecord::new(
        id,
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    ))
}

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "projection-members".into(),
        identity_incarnation: 1,
    }
}

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}

fn metadata_costs(connection: &Connection) -> Result<Vec<(u64, usize)>> {
    [
        "runtime_id",
        "event_log_epoch",
        "visibility_policy_generation",
    ]
    .into_iter()
    .map(|key| {
        let value: String = connection.query_row(
            "SELECT value FROM runtime_metadata WHERE key = ?1",
            [key],
            |row| row.get(0),
        )?;
        Ok((
            ((value.len() + std::mem::size_of::<String>()) as u64) * 8,
            8,
        ))
    })
    .collect()
}

// Independent costs for these public named members without optional anchors.
fn member_projection_cost(connection: &Connection, agent_id: &str) -> Result<(u64, usize)> {
    let payload: String = connection.query_row(
        "SELECT payload_json FROM agent_identities WHERE agent_id = ?1",
        [agent_id],
        |row| row.get(0),
    )?;
    let text = |value: &str| {
        (
            ((value.len() + std::mem::size_of::<String>()) as u64) * 8,
            8,
        )
    };
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
    let decode = (
        (payload.len() as u64 * 4 + slots as u64 * 128 + 512) * 8,
        (slots * 4 + 16) * 8,
    );
    let mut total = add(add(text(&payload), text(&payload)), decode);
    // Durability, attachment, both policies, canonical projection, raw row.
    for _ in 0..6 {
        total = add(total, text(agent_id));
    }
    total = add(
        total,
        (
            (std::mem::size_of::<crate::types::AgentCapabilityPolicyRule>() * 6) as u64 * 8,
            6 * 8,
        ),
    );
    total = add(
        total,
        (
            (std::mem::size_of::<crate::types::AgentMessagePolicyRule>() * 2) as u64 * 8,
            2 * 8,
        ),
    );
    for route in [
        "operator_control",
        "runtime:agent-invocation",
        "agent_invocation",
    ] {
        total = add(total, text(route));
    }
    Ok(total)
}

#[test]
fn ordered_borrowed_members_ignore_hidden_corruption_and_retry_in_one_view() -> Result<()> {
    let (_home, db) = fixture()?;
    // Deliberately insert the members in the opposite order to verification.
    for id in ["z-public", "a-public", "hidden", "deleted"] {
        member(&db, id)?;
    }
    let mut connection = db.connection()?;
    connection.execute_batch(
        "PRAGMA ignore_check_constraints = ON;
         UPDATE agent_identities SET visibility = 'private', payload_json = 'null'
           WHERE agent_id = 'hidden';
         UPDATE agent_identities SET status = 'deleted', payload_json = 'null'
           WHERE agent_id = 'deleted';
         PRAGMA ignore_check_constraints = OFF;",
    )?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
    assert!(verify_projection_snapshot_view(&transaction)?);
    for id in ["z-public", "a-public"] {
        // Valid JSON with an invalid typed State: the selected member must
        // still undergo the original per-projection assembly validation.
        transaction.execute(
            "INSERT INTO agent_states (agent_id, status, updated_at, payload_json)
             VALUES (?1, 'stopped', '2026-01-01T00:00:00Z', 'null')",
            [id],
        )?;
    }
    let error = verify_projection_snapshot_view(&transaction).unwrap_err();
    assert!(
        format!("{error:#}").contains("unreadable projection agent state payload for a-public"),
        "{error:#}"
    );
    transaction.execute("DELETE FROM agent_states WHERE agent_id = 'a-public'", [])?;
    let error = verify_projection_snapshot_view(&transaction).unwrap_err();
    assert!(
        format!("{error:#}").contains("unreadable projection agent state payload for z-public"),
        "{error:#}"
    );
    transaction.execute("DELETE FROM agent_states WHERE agent_id = 'z-public'", [])?;
    assert!(verify_projection_snapshot_view(&transaction)?);
    transaction.rollback()?;
    assert!(connection.is_autocommit());
    assert!(verify_projection_snapshot_view(&connection)?);
    Ok(())
}

#[test]
fn empty_members_still_require_metadata_and_every_projection_source() -> Result<()> {
    let (_home, db) = fixture()?;
    let mut connection = db.connection()?;
    assert!(verify_projection_snapshot_view(&connection)?);
    for key in [
        "runtime_id",
        "event_log_epoch",
        "visibility_policy_generation",
    ] {
        let transaction = connection.transaction()?;
        transaction.execute("DELETE FROM runtime_metadata WHERE key = ?1", [key])?;
        let error = verify_projection_snapshot_view(&transaction).unwrap_err();
        assert!(format!("{error:#}").contains(key), "{error:#}");
        transaction.rollback()?;
    }
    let transaction = connection.transaction()?;
    transaction.execute(
        "UPDATE runtime_metadata SET value = 'not-a-generation'
         WHERE key = 'visibility_policy_generation'",
        [],
    )?;
    let error = verify_projection_snapshot_view(&transaction).unwrap_err();
    assert!(format!("{error:#}").contains("invalid visibility policy generation"));
    transaction.rollback()?;
    assert!(verify_projection_snapshot_view(&connection)?);

    // The source-presence gate must run before attempting metadata or rows.
    let connection = Connection::open_in_memory()?;
    for table in SOURCES {
        connection.execute_batch(&format!("CREATE TABLE {table} (unused INTEGER)"))?;
    }
    for table in SOURCES {
        connection.execute_batch(&format!("DROP TABLE {table}"))?;
        assert!(!verify_projection_snapshot_view(&connection)?, "{table}");
        connection.execute_batch(&format!("CREATE TABLE {table} (unused INTEGER)"))?;
    }
    Ok(())
}

#[test]
fn empty_members_validate_original_roster_columns_and_retry_after_rollback() -> Result<()> {
    let (_home, db) = fixture()?;
    let mut connection = db.connection()?;
    for (table, columns) in [
        (
            "agent_identities",
            &["agent_id", "payload_json", "status", "visibility"][..],
        ),
        (
            "runtime_sequences",
            &["domain", "scope_key", "last_value"][..],
        ),
        (
            "audit_event_retention_watermarks",
            &["scope_key", "oldest_retained_seq"][..],
        ),
        (
            "briefs",
            &[
                "agent_id",
                "evidence_id",
                "created_event_seq",
                "created_at",
                "preview",
            ][..],
        ),
        ("agent_states", &["agent_id", "payload_json"][..]),
    ] {
        for column in columns {
            let transaction = connection.transaction()?;
            transaction.execute_batch(&format!(
                "ALTER TABLE {table} RENAME COLUMN {column} TO saved_{column}"
            ))?;
            let before: i64 = transaction.query_row(
                "SELECT COUNT(*) FROM observer_sync_capability_verifications",
                [],
                |row| row.get(0),
            )?;
            let error = verify_projection_snapshot_view(&transaction).unwrap_err();
            assert!(
                format!("{error:#}").contains("no such column"),
                "{table}.{column}: {error:#}"
            );
            let after: i64 = transaction.query_row(
                "SELECT COUNT(*) FROM observer_sync_capability_verifications",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(before, after);
            transaction.rollback()?;
            assert!(connection.is_autocommit());
            assert!(verify_projection_snapshot_view(&connection)?);
        }
    }
    Ok(())
}

#[test]
fn borrowed_enumeration_reuses_exact_original_phase_and_last_holder() -> Result<()> {
    let (_home, db) = fixture()?;
    let ids = ["a-\"\\\n汉字", "z-public"];
    for id in ids {
        member(&db, id)?;
    }
    let connection = db.connection()?;
    // Enumeration only borrows IDs; each actual projection still owns its
    // metadata, identity texts, decoded canonical material, and returned ID.
    let metadata = metadata_costs(&connection)?.into_iter().fold((0, 0), add);
    let mut one = metadata;
    for id in ids {
        one = add(one, add(metadata, member_projection_cost(&connection, id)?));
    }
    let used = (19 * 8, 2 * 8);
    let exact = add(add(header(), used), add(one, one));
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        charge_owned_metadata(19, 2)?;
        assert!(verify_projection_snapshot_view(&connection)?);
        assert!(verify_projection_snapshot_view(&connection)?);
        assert!(Arc::ptr_eq(
            &holder,
            &ResourceReadMaterial::active_guard()?.unwrap()
        ));
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    assert_eq!(phase.finish()?, exact);
    let last = holder.clone();
    drop((phase, holder));
    assert_eq!(ledger.report()?.observed_bytes, exact.0);
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert!(verify_projection_snapshot_view(&connection)?);
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}

#[test]
fn original_metadata_rejections_release_statements_without_writes_or_phase_reset() -> Result<()> {
    let (_home, db) = fixture()?;
    member(&db, "member")?;
    let connection = db.connection()?;
    let costs = metadata_costs(&connection)?;
    let used = (23 * 8, 3 * 8);
    let mut before = add(header(), used);
    let changes: i64 = connection.query_row("SELECT total_changes()", [], |row| row.get(0))?;
    // Reject every boundary/member metadata allocation in both dimensions.
    for cost in costs.iter().chain(costs.iter()) {
        let next = add(before, *cost);
        for dimension in 0..2 {
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                next.0 - u64::from(dimension == 0),
                next.1 - usize::from(dimension == 1),
            )?;
            let holder = phase.guard()?;
            {
                let _entry = phase.enter();
                charge_owned_metadata(23, 3)?;
                let error = verify_projection_snapshot_view(&connection).unwrap_err();
                assert!(
                    format!("{error:#}").contains("ResourceExhausted"),
                    "{error:#}"
                );
                assert!(ResourceReadMaterial::active_guard().is_err());
                assert!(verify_projection_snapshot_view(&connection).is_err());
            }
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            assert!(phase.finish().is_err());
            assert_eq!(
                connection.query_row("SELECT total_changes()", [], |row| row.get::<_, i64>(0))?,
                changes
            );
            assert!(connection.is_autocommit());
            let last = holder.clone();
            drop((phase, holder));
            assert!(ledger.report()?.observed_bytes >= before.0);
            drop(last);
            assert_eq!(ledger.report()?.observed_bytes, 0);
            assert!(verify_projection_snapshot_view(&connection)?);
        }
        before = next;
    }
    Ok(())
}
