use super::*;
use crate::runtime::input_evidence_budget::InputMemoryCategory;

fn candidate_bytes(ledger: &InputEvidenceBudgetLedger) -> Result<u64> {
    Ok(ledger
        .report()?
        .categories
        .get(&InputMemoryCategory::Candidate)
        .map_or(0, |usage| usage.bytes))
}

#[test]
fn recovery_observation_selectors_intersect_qualification_and_keep_pruned_strict() -> Result<()> {
    let (_dir, db) = fresh()?;
    let present = TaskKey::new(TaskFamily::Task, "qualified");
    let raw = TaskKey::new(TaskFamily::Task, "raw-history");
    db.transaction(|tx| {
        write_task(tx, &task(&present.id, TaskStatus::Completed))?;
        repositories::upsert_task_tx(tx, &task(&raw.id, TaskStatus::Completed))?;
        let requested = BTreeSet::from([
            present.clone(),
            raw.clone(),
            TaskKey::new(TaskFamily::Task, "unknown-observation"),
        ]);
        assert_eq!(
            qualified_task_recovery_selectors_tx(tx, &scope(), &requested)?,
            BTreeSet::from([present.clone()])
        );
        assert!(alias(tx, &scope(), &raw)?.is_none());
        Ok(())
    })?;
    db.transaction(|tx| {
        let proof = prepare_task_write_tx(tx, &scope(), &BTreeSet::from([present.clone()]))?;
        tx.execute("DELETE FROM tasks WHERE task_id=?1", [&present.id])?;
        let mut requested = request();
        requested.capture.required_keys.insert(present.clone());
        register_task_writes_tx(
            tx,
            &proof,
            &BTreeSet::from([present.clone()]),
            &AgentState::new("task-agent"),
            &requested,
        )?;
        let error =
            qualified_task_recovery_selectors_tx(tx, &scope(), &BTreeSet::from([present.clone()]))
                .unwrap_err();
        assert!(matches!(error.downcast_ref::<TaskSnapshotError>(),
            Some(TaskSnapshotError::NeedsPreparation(key)) if key == &present));
        Ok(())
    })?;
    db.transaction(|tx| {
        let raw_identity: String = tx.query_row(
            "SELECT payload_json FROM agent_identities WHERE agent_id='task-agent'",
            [],
            |row| row.get(0),
        )?;
        let mut successor: AgentIdentityRecord = serde_json::from_str(&raw_identity)?;
        successor.incarnation += 1;
        successor.updated_at += Duration::days(1);
        upsert_agent_identity_tx(tx, &successor)?;
        let next = InputEvidenceScope {
            agent_id: scope().agent_id,
            identity_incarnation: 2,
        };
        assert!(qualified_task_recovery_selectors_tx(
            tx,
            &next,
            &BTreeSet::from([present.clone(), raw.clone()])
        )?
        .is_empty());
        assert!(alias(tx, &next, &raw)?.is_none());
        Ok(())
    })
}

#[test]
fn named_preparation_counts_long_normalized_projection_before_body_decode() -> Result<()> {
    let (_dir, db) = fresh()?;
    let mut value = task("long-normalized-target", TaskStatus::Running);
    value.detail = Some(serde_json::json!({"input_target":"x".repeat(1024 * 1024)}));
    db.transaction(|tx| {
        write_task(tx, &value)?;
        Ok(())
    })?;
    let before = db.transaction(|tx| Ok(current_coverage(tx, &scope())?))?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let error = db.transaction(|tx| {
        let (raw, normalized): (usize, usize) = tx.query_row(
            "SELECT length(CAST(payload_json AS BLOB)),length(CAST(input_target AS BLOB)) FROM tasks WHERE task_id=?1",
            [&value.id], |row| Ok((row.get(0)?,row.get(1)?)))?;
        assert!(raw < 16 * 1024);
        assert_eq!(normalized, 1024 * 1024);
        prepare_task_domain_snapshot_with_budget_tx(tx, &scope(), &request(), &ledger)?
            .require_current()
    }).unwrap_err();
    assert!(matches!(
        error.downcast_ref::<TaskSnapshotError>(),
        Some(TaskSnapshotError::ResourceExhausted("bytes"))
    ));
    assert_eq!(candidate_bytes(&ledger)?, 0);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert_eq!(
        db.transaction(|tx| Ok(current_coverage(tx, &scope())?))?,
        before
    );
    let mut enough = request();
    enough.byte_limit = 16 * 1024 * 1024;
    let resident = db
        .transaction(|tx| {
            prepare_task_domain_snapshot_with_budget_tx(tx, &scope(), &enough, &ledger)?
                .require_current()
        })?
        .resident;
    assert_eq!(candidate_bytes(&ledger)?, 0);
    let captured = resident.capture(&enough.capture)?;
    let record = captured
        .lookup(&TaskKey::new(TaskFamily::Task, &value.id))?
        .unwrap();
    assert_eq!(
        record.fact,
        CanonicalTaskFact::Task(repositories::slim_task_record_for_payload(&value))
    );
    drop(captured);
    drop(resident);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn budgeted_preparation_hands_off_to_weak_views_and_selected_pins() -> Result<()> {
    let (_dir, db) = fresh()?;
    let value = task("selected-without-root-backref", TaskStatus::Running);
    db.transaction(|tx| {
        write_task(tx, &value)?;
        Ok(())
    })?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let resident = db
        .transaction(|tx| {
            prepare_task_domain_snapshot_with_budget_tx(tx, &scope(), &request(), &ledger)?
                .require_current()
        })?
        .resident;
    assert_eq!(candidate_bytes(&ledger)?, 0);
    assert_eq!(ledger.report()?.physical_records.allocations, 1);
    let captured = resident.capture(&request().capture)?;
    let weak = Arc::downgrade(
        captured
            .lookup(&TaskKey::new(TaskFamily::Task, &value.id))?
            .unwrap(),
    );
    drop(resident);
    assert!(weak.upgrade().is_some());
    assert_eq!(ledger.report()?.physical_records.allocations, 1);
    drop(captured);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn fixed_selector_and_budget_failure_do_not_materialize_or_adopt_late_history() -> Result<()> {
    let (_dir, db) = legacy(true, |tx| {
        repositories::upsert_task_tx(tx, &task("fixed-read", TaskStatus::Running))?;
        Ok(())
    })?;
    let key = TaskKey::new(TaskFamily::Task, "fixed-read");
    db.transaction(|tx| {
        repositories::upsert_task_tx(tx, &task("late-read", TaskStatus::Completed))?;
        assert_eq!(
            qualified_task_recovery_selectors_tx(
                tx,
                &scope(),
                &BTreeSet::from([key.clone(), TaskKey::new(TaskFamily::Task, "late-read")])
            )?,
            BTreeSet::from([key.clone()])
        );
        assert!(alias(tx, &scope(), &key)?.is_none());
        Ok(())
    })?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let mut tight = request();
    tight.byte_limit = 1;
    let error = db
        .transaction(|tx| {
            prepare_task_domain_snapshot_with_budget_tx(tx, &scope(), &tight, &ledger)?
                .require_current()
        })
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<TaskSnapshotError>(),
        Some(TaskSnapshotError::ResourceExhausted("bytes"))
    ));
    db.transaction(|tx| {
        assert!(alias(tx, &scope(), &key)?.is_none());
        assert!(current_coverage(tx, &scope())?.is_none());
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let prepared = db.transaction(|tx| {
        prepare_task_domain_snapshot_with_budget_tx(tx, &scope(), &request(), &ledger)?
            .require_current()
    })?;
    assert!(prepared
        .resident
        .coverage()
        .active_task_ids
        .contains(&key.id));
    assert!(!prepared
        .resident
        .coverage()
        .terminal
        .keys
        .iter()
        .any(|key| key.id == "late-read"));
    assert_eq!(candidate_bytes(&ledger)?, 0);
    drop(prepared);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
