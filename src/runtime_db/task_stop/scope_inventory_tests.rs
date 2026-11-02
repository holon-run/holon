use super::*;

#[test]
fn startup_stop_inventory_keeps_original_retired_scope_and_validated_sequence_without_state_read(
) -> Result<()> {
    let (_dir, db, ledger) = fixture()?;
    let cancel = db.transaction(|tx| {
        stop(
            tx,
            &ledger,
            "inventory-cancel",
            TaskStopSignal::Cancel,
            true,
        )
    })?;
    let force =
        db.transaction(|tx| stop(tx, &ledger, "inventory-force", TaskStopSignal::Force, true))?;
    db.transaction(|tx|{
        // Explicit successor restore: discovery must neither rewrite the old
        // responsibility's scope nor decode this unrelated State body.
        tx.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',2) WHERE agent_id=?1",[&scope().agent_id])?;
        tx.execute("UPDATE agent_states SET payload_json='poisoned-successor-state' WHERE agent_id=?1",[&scope().agent_id])?;
        let inventory=pending_task_stop_scope_inventory_tx(tx,&scope(),&request(),&ledger)?;
        let selected=inventory.scopes().collect::<Vec<_>>();
        assert_eq!(selected,vec![(&scope(),2)]);assert_eq!(inventory.pending_effect_count(),2);
        // Actual body/whole receipt proof remains separate from discovery.
        let rows=load_pending_task_stops_tx(tx,&scope(),100,request().byte_limit,&ledger)?;
        assert_eq!(rows.records.iter().map(|r|r.effect_seq).collect::<Vec<_>>(),vec![cancel.effect_seq,force.effect_seq]);
        let successor:String=tx.query_row("SELECT payload_json FROM agent_states WHERE agent_id=?1",[&scope().agent_id],|r|r.get(0))?;
        assert_eq!(successor,"poisoned-successor-state");
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn startup_stop_inventory_admits_scope_material_before_decode_and_releases_guard() -> Result<()> {
    let (_dir, db, ledger) = fixture()?;
    db.transaction(|tx| stop(tx, &ledger, "inventory-owned", TaskStopSignal::Cancel, true))?;
    let inventory = db.transaction(|tx| {
        pending_task_stop_scope_inventory_tx(tx, &scope(), &request(), &ledger)
    })?;
    assert_eq!(inventory.pending_effect_count(), 1);
    assert!(ledger.report()?.observed_bytes > 0);
    drop(inventory);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let mut tiny = request();
    tiny.record_limit = 0;
    let error = db
        .transaction(|tx| pending_task_stop_scope_inventory_tx(tx, &scope(), &tiny, &ledger))
        .err()
        .context("record budget must reject")?;
    assert!(error
        .downcast_ref::<crate::runtime_db::task_snapshot::TaskSnapshotError>()
        .is_some());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn startup_stop_inventory_oversized_corrupt_scope_is_budgeted_before_string_or_body_decode(
) -> Result<()> {
    let (_dir, db, ledger) = fixture()?;
    db.transaction(|tx| {
        stop(
            tx,
            &ledger,
            "inventory-oversize",
            TaskStopSignal::Cancel,
            true,
        )
    })?;
    db.transaction(|tx| {
        // Malformed conflicting restore is a negative fixture. The immutable
        // prepared/receipt address intentionally remains the original small one.
        tx.execute(
            "UPDATE task_stop_effects SET agent_id=?1",
            ["x".repeat(128 * 1024)],
        )?;
        Ok(())
    })?;
    let mut tiny = request();
    tiny.byte_limit = 1024;
    let error = db
        .transaction(|tx| pending_task_stop_scope_inventory_tx(tx, &scope(), &tiny, &ledger))
        .err()
        .context("byte budget must reject")?;
    assert!(error
        .downcast_ref::<crate::runtime_db::task_snapshot::TaskSnapshotError>()
        .is_some());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn host_stop_inventory_accounts_metadata_without_default_or_original_current_identity() -> Result<()>
{
    let (_dir, db, ledger) = fixture()?;
    db.transaction(|tx| {
        stop(
            tx,
            &ledger,
            "host-inventory-original",
            TaskStopSignal::Cancel,
            true,
        )
    })?;
    db.transaction(|tx|{
        tx.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',2) WHERE agent_id=?1",[&scope().agent_id])?;
        tx.execute("UPDATE agent_states SET payload_json='poisoned-successor' WHERE agent_id=?1",[&scope().agent_id])?;
        let inventory=pending_task_stop_host_scope_inventory_tx(tx,&request(),&ledger)?;
        assert_eq!(inventory.pending_effect_count(),1);
        assert_eq!(inventory.scopes().map(|(s,_)|s.clone()).collect::<Vec<_>>(),vec![scope()]);
        let report=ledger.report()?;assert!(report.host_owned.payload_bytes.checked_add(report.host_owned.metadata_estimate_bytes).context("Host metadata fixture overflow")?>0);
        assert!(report.scopes.is_empty(),"Host metadata must not invent an incarnation scope");
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
