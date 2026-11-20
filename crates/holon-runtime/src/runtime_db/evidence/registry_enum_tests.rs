use super::{registry_incarnation_budget_tests::*, *};
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
    runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    types::{AgentOwnership, AgentProfilePreset, AgentVisibility},
};
use std::sync::Arc;

fn columns(db: &crate::runtime_db::RuntimeDb, id: &str) -> Result<serde_json::Value> {
    Ok(db.connection()?.query_row(
        "SELECT kind, visibility, ownership, profile_preset, status
         FROM agent_identities WHERE agent_id = ?1",
        [id],
        |row| {
            Ok(serde_json::json!({
                "kind": row.get::<_, String>(0)?,
                "visibility": row.get::<_, String>(1)?,
                "ownership": row.get::<_, Option<String>>(2)?,
                "profile_preset": row.get::<_, Option<String>>(3)?,
                "status": row.get::<_, String>(4)?,
            }))
        },
    )?)
}

fn expected(record: &AgentIdentityRecord) -> Result<serde_json::Value> {
    Ok(serde_json::json!({
        "kind": serde_json::to_value(record.kind)?,
        "visibility": serde_json::to_value(record.visibility)?,
        "ownership": serde_json::to_value(record.ownership)?,
        "profile_preset": serde_json::to_value(record.profile_preset)?,
        "status": serde_json::to_value(record.status)?,
    }))
}

#[test]
fn registry_enum_columns_match_serde_for_all_variants_and_null_combinations() -> Result<()> {
    let (_home, db) = fixture()?;
    let mut cases = 0;
    for kind in [AgentKind::Default, AgentKind::Named, AgentKind::Child] {
        for visibility in [AgentVisibility::Public, AgentVisibility::Private] {
            for ownership in [
                None,
                Some(AgentOwnership::ParentSupervised),
                Some(AgentOwnership::SelfOwned),
            ] {
                for profile_preset in [
                    None,
                    Some(AgentProfilePreset::PrivateChild),
                    Some(AgentProfilePreset::PublicNamed),
                ] {
                    for status in [
                        AgentRegistryStatus::Active,
                        AgentRegistryStatus::Deleting,
                        AgentRegistryStatus::Deleted,
                    ] {
                        let mut record = identity();
                        record.agent_id = format!("registry-enum-{cases}");
                        record.name = None;
                        record.kind = kind;
                        record.visibility = visibility;
                        record.ownership = ownership;
                        record.profile_preset = profile_preset;
                        record.status = status;
                        db.agent_identities().upsert(&record)?;
                        assert_eq!(columns(&db, &record.agent_id)?, expected(&record)?);
                        assert_eq!(
                            db.agent_identities().latest(&record.agent_id)?,
                            Some(record)
                        );
                        cases += 1;
                    }
                }
            }
        }
    }
    assert_eq!(cases, 162);
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}

#[test]
fn registry_enum_updates_reuse_exact_caller_phase_and_last_holder() -> Result<()> {
    let (_home, db) = fixture()?;
    let original = identity();
    db.agent_identities().upsert(&original)?;
    let mut replacement = original.clone();
    replacement.kind = AgentKind::Child;
    replacement.visibility = AgentVisibility::Private;
    replacement.ownership = None;
    replacement.profile_preset = None;
    replacement.status = AgentRegistryStatus::Deleting;
    replacement.updated_at += chrono::Duration::seconds(1);
    let first = add(
        decode_cost(&serde_json::to_string(&original)?),
        add(
            name_key_cost(&replacement),
            upsert_timestamp_cost(&replacement),
        ),
    );
    let second = add(
        decode_cost(&serde_json::to_string(&replacement)?),
        add(
            name_key_cost(&replacement),
            upsert_timestamp_cost(&replacement),
        ),
    );
    let before = add(header(), (31 * 8, 3 * 8));
    let exact = add(add(before, first), second);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        charge_owned_metadata(31, 3)?;
        db.agent_identities().upsert(&replacement)?;
        assert_eq!(phase.finish()?, add(before, first));
        db.agent_identities().upsert(&replacement)?;
        assert_eq!(phase.finish()?, exact);
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &holder,
        ));
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    assert_eq!(
        columns(&db, &replacement.agent_id)?,
        expected(&replacement)?
    );
    let last = holder.clone();
    drop((phase, holder));
    assert_eq!(ledger.report()?.observed_bytes, exact.0);
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn registry_enum_sql_failure_rolls_back_with_original_phase_intact() -> Result<()> {
    for existing in [false, true] {
        let (_home, db) = fixture()?;
        let mut record = identity();
        if existing {
            db.agent_identities().upsert(&record)?;
        }
        let before_sql = snapshot(&db)?;
        let before = add(header(), (29 * 8, 2 * 8));
        let exact = add(
            add(
                before,
                add(name_key_cost(&record), upsert_timestamp_cost(&record)),
            ),
            if existing {
                decode_cost(&serde_json::to_string(&record)?)
            } else {
                (0, 0)
            },
        );
        record.kind = AgentKind::Default;
        record.visibility = AgentVisibility::Private;
        record.ownership = None;
        record.profile_preset = None;
        record.status = AgentRegistryStatus::Deleting;
        db.connection()?.execute_batch(
            "CREATE TRIGGER reject_enum_write BEFORE INSERT ON agent_identities
             BEGIN SELECT RAISE(ABORT, 'registry_enum_sql_failure'); END;",
        )?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            charge_owned_metadata(29, 2)?;
            let error = db
                .transaction(|tx| {
                    tx.execute(
                        "INSERT INTO runtime_metadata (key, value, created_at, updated_at)
                         VALUES ('enum_marker', 'rollback',
                                 '2026-10-17T22:00:00Z', '2026-10-17T22:00:00Z')",
                        [],
                    )?;
                    upsert_agent_identity_tx(tx, &record)
                })
                .unwrap_err();
            assert!(format!("{error:#}").contains("registry_enum_sql_failure"));
            assert_eq!(phase.finish()?, exact);
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder,
            ));
        }
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        assert_eq!(snapshot(&db)?, before_sql);
        let last = holder.clone();
        drop((phase, holder));
        assert_eq!(ledger.report()?.observed_bytes, exact.0);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn registry_enum_stale_write_and_legacy_deleted_alias_preserve_lifecycle() -> Result<()> {
    let (_home, db) = fixture()?;
    let original = identity();
    db.agent_identities().upsert(&original)?;
    let before_sql = snapshot(&db)?;
    let mut stale = original.clone();
    stale.kind = AgentKind::Default;
    stale.visibility = AgentVisibility::Private;
    stale.ownership = None;
    stale.profile_preset = None;
    stale.status = AgentRegistryStatus::Deleting;
    stale.updated_at -= chrono::Duration::seconds(1);
    db.agent_identities().upsert(&stale)?;
    assert_eq!(snapshot(&db)?, before_sql);
    assert_eq!(columns(&db, &original.agent_id)?, expected(&original)?);

    let mut legacy = serde_json::to_value(&stale)?;
    legacy["status"] = "archived".into();
    let mut deleted: AgentIdentityRecord = serde_json::from_value(legacy)?;
    deleted.updated_at = original.updated_at + chrono::Duration::seconds(1);
    db.agent_identities().upsert(&deleted)?;
    assert_eq!(columns(&db, &deleted.agent_id)?, expected(&deleted)?);
    assert_eq!(columns(&db, &deleted.agent_id)?["status"], "deleted");
    let retired = snapshot(&db)?;
    let error = db.agent_identities().upsert(&original).unwrap_err();
    assert!(format!("{error:#}").contains("retired"));
    assert_eq!(snapshot(&db)?, retired);
    db.agent_identities().upsert(&deleted)?;
    assert_eq!(snapshot(&db)?, retired);
    Ok(())
}
