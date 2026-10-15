use super::*;

#[test]
fn scheduling_state_nodes_exhaust_records_before_typed_corruption_and_write() -> Result<()> {
    let (_home, db) = legacy(true, |_| Ok(()))?;
    let budget = ledger()?;
    let mut state = serde_json::to_value(AgentState::new(&scope().agent_id))?;
    state["pending"] = serde_json::json!("deliberately-invalid-count");
    state["working_memory"] = serde_json::json!({"oversized_collection":vec![0; 4096]});
    let raw = serde_json::to_string(&state)?;
    db.transaction(|tx| {
        tx.execute(
            "UPDATE agent_states SET payload_json=?2 WHERE agent_id=?1",
            params![scope().agent_id, raw],
        )?;
        Ok(())
    })?;
    let mut small = request();
    small.record_limit = 512;
    let error = db
        .transaction(|tx| prepare_scheduling_domain_snapshot_tx(tx, &scope(), &small, &budget))
        .unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<SchedulingSnapshotError>(),
            Some(SchedulingSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(budget.report()?.observed_bytes, 0);
    let (stored, captures, aliases): (String, usize, usize) = db.transaction(|tx| {
        Ok(tx.query_row(
            "SELECT payload_json,(SELECT COUNT(*) FROM scheduling_domain_current),
            (SELECT COUNT(*) FROM scheduling_domain_records) FROM agent_states WHERE agent_id=?1",
            [&scope().agent_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?)
    })?;
    assert_eq!(stored, raw);
    assert_eq!((captures, aliases), (0, 0));
    let mut adequate = request();
    adequate.record_limit = 65536;
    let error = db
        .transaction(|tx| prepare_scheduling_domain_snapshot_tx(tx, &scope(), &adequate, &budget))
        .unwrap_err();
    assert!(
        error.downcast_ref::<SchedulingSnapshotError>().is_none(),
        "{error:#}"
    );
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn scheduling_state_node_material_lives_until_snapshot_drop() -> Result<()> {
    let (_home, db) = legacy(true, |_| Ok(()))?;
    let budget = ledger()?;
    let snapshot = db.transaction(|tx| {
        prepare_scheduling_domain_snapshot_tx(tx, &scope(), &request(), &budget)
    })?;
    assert!(matches!(snapshot, SchedulingBootstrapSnapshot::Current(_)));
    assert!(budget.report()?.observed_bytes > 0);
    drop(snapshot);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn scheduling_retired_incarnation_skips_oversized_corrupt_successor_state() -> Result<()> {
    let (_home, db) = legacy(true, |_| Ok(()))?;
    let budget = ledger()?;
    let mut successor = identity();
    successor.incarnation += 1;
    db.transaction(|tx| {
        tx.execute(
            "UPDATE agent_identities SET payload_json=?2 WHERE agent_id=?1",
            params![scope().agent_id, serde_json::to_string(&successor)?],
        )?;
        tx.execute(
            "UPDATE agent_states SET payload_json=?2 WHERE agent_id=?1",
            params![scope().agent_id, "not-json".repeat(10000)],
        )?;
        Ok(())
    })?;
    let mut small = request();
    small.record_limit = 32;
    small.byte_limit = 8192;
    let snapshot =
        db.transaction(|tx| prepare_scheduling_domain_snapshot_tx(tx, &scope(), &small, &budget))?;
    assert!(matches!(
        snapshot,
        SchedulingBootstrapSnapshot::Retired { .. }
    ));
    drop(snapshot);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn scheduling_state_object_keys_are_admitted_before_second_state_decode() -> Result<()> {
    let (_home, db) = legacy(true, |_| Ok(()))?;
    let budget = ledger()?;
    let mut state = AgentState::new(&scope().agent_id);
    state.tool_latency = (0..512)
        .map(|index| crate::types::ToolLatencyMetrics {
            tool_name: format!("dense-tool-{index}"),
            total_calls: 1,
            total_duration_ms: 1,
        })
        .collect();
    let valid = serde_json::to_string(&state)?;
    let mut corrupt = serde_json::to_value(&state)?;
    corrupt["pending"] = serde_json::json!("typed-decode-must-not-run");
    let raw = serde_json::to_string(&corrupt)?;
    let (values, keys): (usize, usize) = db.transaction(|tx| {
        tx.execute(
            "UPDATE agent_states SET payload_json=?2 WHERE agent_id=?1",
            params![scope().agent_id, raw],
        )?;
        Ok(tx.query_row(
            "SELECT COUNT(*),COUNT(CASE WHEN typeof(key)='text' THEN 1 END)
             FROM json_tree((SELECT payload_json FROM agent_states WHERE agent_id=?1))",
            [&scope().agent_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?)
    })?;
    assert!(keys > 1536, "the actual tool records carry object keys");
    for input_memory in [false, true] {
        let mut small = request();
        small.byte_limit = MAX_BYTES;
        // Identity and raw-State each admit one slot. The previous values-only
        // gate fit exactly; the actual keys must reject before typed corruption.
        small.record_limit = (values + 2) * 8;
        let error = db
            .transaction(|tx| {
                let original_scope = scope();
                let mut material = Material::new(&original_scope, &small, &budget);
                material.input_memory = input_memory;
                loader::scope_facts(tx, &original_scope, &mut material, false)
            })
            .unwrap_err();
        assert!(
            matches!(
                error.downcast_ref::<SchedulingSnapshotError>(),
                Some(SchedulingSnapshotError::ResourceExhausted(_))
            ),
            "{error:#}"
        );
        assert_eq!(budget.report()?.observed_bytes, 0);
        let mut enough = small.clone();
        enough.record_limit = 65536;
        let error = db
            .transaction(|tx| {
                let original_scope = scope();
                let mut material = Material::new(&original_scope, &enough, &budget);
                material.input_memory = input_memory;
                loader::scope_facts(tx, &original_scope, &mut material, false)
            })
            .unwrap_err();
        assert!(
            error.downcast_ref::<SchedulingSnapshotError>().is_none(),
            "{error:#}"
        );
        assert!(format!("{error:#}").contains("decoding agent state payload"));
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    db.transaction(|tx| {
        let (stored, current, aliases): (String, usize, usize) = tx.query_row(
            "SELECT payload_json,(SELECT COUNT(*) FROM scheduling_domain_current),
             (SELECT COUNT(*) FROM scheduling_domain_records) FROM agent_states WHERE agent_id=?1",
            [&scope().agent_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(stored, raw);
        assert_eq!((current, aliases), (0, 0));
        tx.execute(
            "UPDATE agent_states SET payload_json=?2 WHERE agent_id=?1",
            params![scope().agent_id, valid],
        )?;
        Ok(())
    })?;
    for input_memory in [false, true] {
        db.transaction(|tx| {
            let original_scope = scope();
            let mut full = request();
            full.record_limit = 65536;
            full.byte_limit = MAX_BYTES;
            let mut material = Material::new(&original_scope, &full, &budget);
            material.input_memory = input_memory;
            let facts = loader::scope_facts(tx, &original_scope, &mut material, false)?
                .context("actual original State must remain available")?;
            assert_eq!(facts.state.as_ref(), Some(&state));
            assert_eq!(material.records, (values + keys + 2) * 8);
            assert!(budget.report()?.observed_bytes > 0);
            Ok(())
        })?;
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    Ok(())
}
