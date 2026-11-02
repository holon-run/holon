use super::*;
fn token_trigger() -> ExternalTriggerRecord {
    let mut row = trigger();
    row.token_hash = crate::callbacks::hash_callback_token(row.token.as_deref().unwrap());
    row
}
#[test]
fn token_locator_keeps_material_and_never_grants_qualification_or_state() -> Result<()> {
    let row = token_trigger();
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_external_trigger_tx(tx, &row)
    })?;
    let budget = ledger()?;
    let before:(usize,usize,usize)=db.connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM scheduling_domain_current),(SELECT COUNT(*) FROM scheduling_domain_records),(SELECT COUNT(*) FROM queue_entries)",
        [],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    let locator = db
        .transaction(|tx| {
            discover_external_trigger_token_tx(tx, &row.token_hash, &request(), &budget)
        })?
        .context("original token route missing")?;
    assert_eq!(locator.target_agent_id, row.target_agent_id);
    assert_eq!(locator.external_trigger_id, row.external_trigger_id);
    assert_eq!(locator.delivery_mode, row.delivery_mode);
    assert!(budget.report()?.observed_bytes > 0);
    let compatible = db
        .transaction(|tx| {
            read_external_trigger_token_compatibility_tx(tx, &row.token_hash, &request(), &budget)
        })?
        .context("legacy compatibility route missing")?;
    assert_eq!(compatible.record(), &row);
    drop((locator, compatible));
    assert_eq!(budget.report()?.observed_bytes, 0);
    let after:(usize,usize,usize)=db.connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM scheduling_domain_current),(SELECT COUNT(*) FROM scheduling_domain_records),(SELECT COUNT(*) FROM queue_entries)",
        [],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    assert_eq!(before, after);
    Ok(())
}
#[test]
fn token_discovery_raw_and_native_nodes_are_budgeted_before_header_or_body_decode() -> Result<()> {
    for corrupt in ["bad_large_body", "large_normalized_owner", "native_nodes"] {
        let row = token_trigger();
        let (_home, db) = legacy(true, |tx| {
            repositories::upsert_external_trigger_tx(tx, &row)
        })?;
        db.transaction(|tx| {
            match corrupt {
                "bad_large_body"=>{tx.execute("UPDATE external_triggers SET payload_json=?1 WHERE external_trigger_id=?2",params!["!".repeat(65537),row.external_trigger_id])?;}
                "large_normalized_owner"=>{tx.execute("UPDATE external_triggers SET target_agent_id=?1,payload_json='!' WHERE external_trigger_id=?2",params!["x".repeat(65537),row.external_trigger_id])?;}
                _=>{tx.execute("UPDATE external_triggers SET payload_json=json_set(payload_json,'$.ignored_collection',json(?1)) WHERE external_trigger_id=?2",params![serde_json::to_string(&vec![0u8;500])?,row.external_trigger_id])?;}
            }
            Ok(())
        })?;
        let budget = ledger()?;
        let limit = SchedulingDomainLoadRequest {
            capture: SchedulingCaptureRequest::current(),
            byte_limit: if corrupt == "native_nodes" {
                1024 * 1024
            } else {
                4096
            },
            record_limit: 32,
        };
        for compatibility in [false, true] {
            let error = db
                .transaction(|tx| {
                    if compatibility {
                        read_external_trigger_token_compatibility_tx(
                            tx,
                            &row.token_hash,
                            &limit,
                            &budget,
                        )
                        .map(|_| ())
                    } else {
                        discover_external_trigger_token_tx(tx, &row.token_hash, &limit, &budget)
                            .map(|_| ())
                    }
                })
                .err()
                .context("oversize callback material must reject")?;
            assert!(
                matches!(
                    error.downcast_ref::<SchedulingSnapshotError>(),
                    Some(SchedulingSnapshotError::ResourceExhausted(_))
                ),
                "{corrupt}/{compatibility}: {error:#}"
            );
            assert_eq!(budget.report()?.observed_bytes, 0);
        }
        let current: usize = db.connection()?.query_row(
            "SELECT COUNT(*) FROM scheduling_domain_current",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(current, 0);
    }
    Ok(())
}
#[test]
fn token_discovery_normalized_body_drift_is_not_absence_and_revocation_never_decodes_body(
) -> Result<()> {
    let row = token_trigger();
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_external_trigger_tx(tx, &row)
    })?;
    let budget = ledger()?;
    db.transaction(|tx| {
        tx.execute("UPDATE external_triggers SET payload_json=json_set(payload_json,'$.target_agent_id','foreign-owner') WHERE external_trigger_id=?1",[&row.external_trigger_id])?;
        Ok(())
    })?;
    let error = db
        .transaction(|tx| {
            discover_external_trigger_token_tx(tx, &row.token_hash, &request(), &budget)
        })
        .err()
        .context("corrupt selected address cannot become absence")?;
    assert!(
        error
            .to_string()
            .contains("normalized/body address differs"),
        "{error:#}"
    );
    assert_eq!(budget.report()?.observed_bytes, 0);
    db.transaction(|tx| {
        tx.execute("UPDATE external_triggers SET status='revoked',payload_json='!' WHERE external_trigger_id=?1",[&row.external_trigger_id])?;
        Ok(())
    })?;
    assert!(db
        .transaction(|tx| discover_external_trigger_token_tx(
            tx,
            &row.token_hash,
            &request(),
            &budget
        ))?
        .is_none());
    assert!(db
        .transaction(|tx| read_external_trigger_token_compatibility_tx(
            tx,
            &row.token_hash,
            &request(),
            &budget
        ))?
        .is_none());
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
