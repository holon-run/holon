use super::*;

#[test]
fn taking_writes_moves_both_sets_without_charges_or_changing_applied() -> Result<()> {
    for flags in 0..8 {
        let (message, _) = message();
        let mut result = TriggerWaitAndEnqueueResult {
            message,
            message_inserted: flags & 1 != 0,
            queue_applied: flags & 2 != 0,
            wait_applied: flags & 4 != 0,
            wait_writes: WaitConditionWrites {
                primary_applied: true,
                wait_ids: BTreeSet::from(["wait-路".to_owned()]),
                turn_ids: BTreeSet::from(["turn-路".to_owned()]),
            },
            audit_events: Vec::new(),
            existing_audit_events: Vec::new(),
            outcome: TriggerWaitAndEnqueueOutcome::NoMatch,
        };
        let wait_ptr = result.wait_writes.wait_ids.first().unwrap().as_ptr();
        let turn_ptr = result.wait_writes.turn_ids.first().unwrap().as_ptr();
        let budget = ledger()?;
        let material =
            ResourceReadMaterial::with_test_limits(&budget, &scope(), header().0, header().1)?;
        {
            let _entry = material.enter();
            let moved = result.take_wait_writes();
            assert!(moved.primary_applied);
            assert_eq!(moved.wait_ids.first().unwrap().as_ptr(), wait_ptr);
            assert_eq!(moved.turn_ids.first().unwrap().as_ptr(), turn_ptr);
            assert!(!result.wait_writes.primary_applied);
            assert!(result.wait_writes.wait_ids.is_empty());
            assert!(result.wait_writes.turn_ids.is_empty());
            let empty = result.take_wait_writes();
            assert!(!empty.primary_applied);
            assert!(empty.wait_ids.is_empty() && empty.turn_ids.is_empty());
            assert_eq!(result.applied(), flags != 0);
            assert_eq!(result.outcome, TriggerWaitAndEnqueueOutcome::NoMatch);
        }
        assert_eq!(material.finish()?, header());
        drop(material);
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn real_reducer_write_move_preserves_audit_and_original_transaction_holder() -> Result<()> {
    let source = record();
    let (_home, db) = fixture(&source)?;
    let (message, queue) = message();
    let budget = ledger()?;
    let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), 64 * 1024 * 1024, 65536)?;
    let transaction = phase.transaction()?;
    let (result, moved) = {
        let _entry = transaction.enter();
        let mut result = db.transaction(|tx| trigger_wait_and_enqueue_tx(tx, &message, &queue))?;
        assert!(result.applied());
        let wait_ptr = result.wait_writes.wait_ids.first().unwrap().as_ptr();
        let audit_ptr = result.audit_events.as_ptr();
        let audit_len = result.audit_events.len();
        assert!(audit_len > 0);
        let message_ptr = result.message.id.as_ptr();
        let before = transaction.finish()?;
        let moved = result.take_wait_writes();
        assert_eq!(transaction.finish()?, before);
        assert_eq!(moved.wait_ids.first().unwrap().as_ptr(), wait_ptr);
        assert_eq!(moved.wait_ids, BTreeSet::from([source.id]));
        assert!(result.applied());
        assert_eq!(result.audit_events.as_ptr(), audit_ptr);
        assert_eq!(result.audit_events.len(), audit_len);
        assert_eq!(result.message.id.as_ptr(), message_ptr);
        assert!(matches!(
            result.outcome,
            TriggerWaitAndEnqueueOutcome::Triggered { .. }
        ));
        (result, moved)
    };
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let first = [phase.guard()?, transaction.guard()?];
    let last = first.clone();
    let used = budget.report()?.observed_bytes;
    drop((result, moved, transaction, phase));
    assert_eq!(budget.report()?.observed_bytes, used);
    drop(first);
    assert_eq!(budget.report()?.observed_bytes, used);
    drop(last);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
