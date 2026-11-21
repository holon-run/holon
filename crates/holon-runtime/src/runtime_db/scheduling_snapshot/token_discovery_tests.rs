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
