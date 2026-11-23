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
        assert!(SYNC_TEST.verify_projection_snapshot_view(&connection)?);
        assert!(SYNC_TEST.verify_projection_snapshot_view(&connection)?);
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
    assert!(SYNC_TEST.verify_projection_snapshot_view(&connection)?);
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
                let error = SYNC_TEST
                    .verify_projection_snapshot_view(&connection)
                    .unwrap_err();
                assert!(
                    format!("{error:#}").contains("ResourceExhausted"),
                    "{error:#}"
                );
                assert!(ResourceReadMaterial::active_guard().is_err());
                assert!(SYNC_TEST
                    .verify_projection_snapshot_view(&connection)
                    .is_err());
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
            assert!(SYNC_TEST.verify_projection_snapshot_view(&connection)?);
        }
        before = next;
    }
    Ok(())
}
