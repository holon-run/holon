use super::*;

// #3816 B4a: a registered open WorkItem stranded off its durable revision
// blocks the send before model IO, even when the drifting WorkItem is not
// the attempt's own binding. Without this guard, conversation and lifecycle
// attempts keep running the model and only fail at settlement.
#[test]
fn drifted_sibling_work_item_blocks_dispatch_before_io() -> Result<()> {
    let (_dir, db) = database()?;
    let attempt = canonical_root(&db)?;
    // Strand a second registered open WorkItem: durable revision 2 while
    // its execution registration still records source revision 1.
    let mut sibling = WorkItemRecord::new(
        "agent-a",
        "drifted sibling",
        crate::types::WorkItemState::Open,
    );
    sibling.id = "work-drifted".into();
    db.work_items().insert_new(&sibling)?;
    let mut protocol = db
        .transitions()
        .load_execution_protocol_state_if_initialized("agent-a")?
        .unwrap();
    protocol.work_items.insert(
        sibling.id.clone(),
        WorkItemExecutionRecord {
            source_revision: 1,
            state: WorkItemExecutionState::Runnable {
                generation: 1,
                recovery_ref: None,
            },
        },
    );
    db.transaction(|tx| {
        crate::runtime_db::transitions::execution_protocol_repository::persist_state_tx(
            tx, &protocol,
        )?;
        // Bypass the protocol-aware writers to strand the durable revision
        // ahead of the sibling registration (legacy drift shape).
        let mut drifted = sibling.clone();
        drifted.revision = 2;
        tx.execute(
            "UPDATE work_items SET revision = ?1, payload_json = ?2 WHERE work_item_id = ?3",
            rusqlite::params![2, serde_json::to_string(&drifted)?, sibling.id],
        )?;
        Ok(())
    })?;
    let episode = canonical_episode(&db, &attempt)?;
    let admission = lease(&db, &[transport("drifted-owner")])?;
    let result = db.charge_provider_recovery_dispatch_for_attempt(
        &episode,
        &admission,
        "drift-blocked-send",
        1,
        &attempt,
        now(),
    )?;
    assert_denied(
        result,
        ProviderRecoveryDispatchDenial::StaleExecution(
            ProviderDispatchRejection::WorkItemRevisionDrift,
        ),
    );
    assert_not_charged(&db, &episode)?;
    let drifted = db
        .audit_events()
        .recent(Some("agent-a"), 10)?
        .into_iter()
        .find(|event| event.kind == "provider_dispatch_work_item_drift_rejected")
        .expect("drift rejection audit event");
    let details = &drifted.data;
    assert_eq!(
        details["drifted_work_items"][0]["work_item_id"],
        "work-drifted"
    );
    assert_eq!(details["drifted_work_items"][0]["source_revision"], 1);
    assert_eq!(details["drifted_work_items"][0]["durable_revision"], 2);
    assert_eq!(details["dispatch_occurred"], false);
    Ok(())
}

#[test]
fn public_work_item_writer_advances_source_without_rewriting_attempt() -> Result<()> {
    let (_dir, db) = database()?;
    let attempt = canonical_root(&db)?;
    let mut work = db.work_items().latest("work-a")?.unwrap();
    let previous_revision = work.revision;
    work.revision += 1;
    work.objective
        .push_str(" with an in-flight reference update");
    assert!(db.work_items().update_expected(&work, previous_revision)?);
    let protocol = db
        .transitions()
        .load_execution_protocol_state_if_initialized("agent-a")?
        .unwrap();
    assert_eq!(protocol.work_items["work-a"].source_revision, work.revision);
    assert_eq!(protocol.work_items["work-a"].generation(), 1);
    assert_eq!(protocol.attempts[&attempt.attempt_id], attempt);
    assert!(!db.work_items().update_expected(&work, previous_revision)?);
    let episode = canonical_episode(&db, &attempt)?;
    let admission = lease(&db, &[transport("updated-owner")])?;
    assert!(matches!(
        db.charge_provider_recovery_dispatch_for_attempt(
            &episode,
            &admission,
            "first-send-after-update",
            1,
            &attempt,
            now(),
        )?,
        ProviderRecoveryDispatchResult::Charged(_)
    ));
    Ok(())
}

#[test]
fn public_work_item_writer_cannot_repair_preexisting_revision_drift() -> Result<()> {
    let (_dir, db) = database()?;
    canonical_root(&db)?;
    // Corrupt the execution protocol snapshot without a legitimate composed
    // writer: the protocol source revision runs ahead of the durable record.
    let mut protocol = db
        .transitions()
        .load_execution_protocol_state_if_initialized("agent-a")?
        .unwrap();
    let durable = db.work_items().latest("work-a")?.unwrap();
    protocol
        .work_items
        .get_mut("work-a")
        .unwrap()
        .source_revision = durable.revision + 2;
    db.transaction(|tx| {
        crate::runtime_db::transitions::persist_state_tx(tx, &protocol)?;
        Ok(())
    })?;
    let before = dispatch_authorization_snapshot(&db)?;
    let mut next = durable.clone();
    next.revision += 1;
    assert!(db
        .work_items()
        .update_expected(&next, durable.revision)
        .is_err());
    assert_eq!(db.work_items().latest("work-a")?, Some(durable));
    assert_eq!(dispatch_authorization_snapshot(&db)?, before);
    Ok(())
}

#[test]
fn revision_rejection_audits_the_exact_transaction_snapshot_without_charging() -> Result<()> {
    for (admitted, source, detail) in [
        (Some(1), 2, "durable_source_revision_mismatch"),
        (None, 1, "admitted_revision_missing"),
        (Some(0), 1, "admitted_revision_zero"),
        (Some(2), 1, "admitted_revision_ahead"),
    ] {
        let (_dir, db) = database()?;
        let mut attempt = canonical_root(&db)?;
        attempt.admitted_fences.work_item_source_revision = admitted;
        store_dispatch_attempt(&db, &attempt)?;
        db.transaction(|tx| {
            let execution = WorkItemExecutionRecord {
                source_revision: source,
                state: WorkItemExecutionState::InFlight {
                    generation: 1,
                    attempt_id: attempt.attempt_id.clone(),
                },
            };
            tx.execute(
                "UPDATE execution_protocol_work_items SET payload_json = ?1
                 WHERE agent_id = 'agent-a' AND work_item_id = 'work-a'",
                [serde_json::to_string(&execution)?],
            )?;
            Ok(())
        })?;
        let episode = canonical_episode(&db, &attempt)?;
        let admission = lease(&db, &[transport(detail)])?;
        assert_dispatch_rejected(
            &db,
            &episode,
            &admission,
            &attempt,
            ProviderDispatchRejection::WorkItemRevisionMismatch,
        )?;
        db.with_read(|connection| {
            let raw: String = connection.query_row(
                "SELECT data_json FROM audit_events
                 WHERE agent_id = 'agent-a'
                   AND kind = 'provider_dispatch_revision_rejected'",
                [],
                |row| row.get(0),
            )?;
            let event: crate::types::AuditEvent = serde_json::from_str(&raw)?;
            assert_eq!(event.data["detail"], detail);
            assert_eq!(event.data["reason"], "work_item_revision_mismatch");
            assert_eq!(event.data["durable_revision"], 1);
            assert_eq!(event.data["source_revision"], source);
            assert_eq!(event.data["admitted_revision"], serde_json::json!(admitted));
            assert_eq!(event.data["expected_generation"], 1);
            assert_eq!(event.data["actual_generation"], 1);
            assert_eq!(event.data["dispatch_occurred"], false);
            assert_eq!(event.data["attempt_id"], attempt.attempt_id);
            assert_eq!(event.data["work_item_id"], "work-a");
            assert!(event.data.get("prompt").is_none());
            Ok(())
        })?;
    }
    Ok(())
}
