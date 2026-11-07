use super::*;

fn borrowed_slots(count: usize) -> (u64, usize) {
    (
        (count * std::mem::size_of::<(&str, DateTime<Utc>, usize)>()) as u64 * 8,
        count * 8,
    )
}

#[test]
fn observed_trigger_index_borrows_exact_ids_and_only_charges_slots() -> Result<()> {
    for count in [1, 8, 65] {
        let mut rows = (0..count)
            .rev()
            .map(|i| trigger(&format!("{i:03} 汉字\0🦀"), Some(i as i64), true))
            .collect::<Vec<_>>();
        for row in &mut rows {
            row.token = Some("unused secret fixture 汉字".repeat(1024));
            row.token_hash = "unused hash".repeat(1024);
        }
        let before = rows.clone();
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let one = borrowed_slots(count);
        let total = (header().0 + 2 * one.0, header().1 + 2 * one.1);
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), total.0, total.1)?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            for round in 1..=2 {
                let index = queue_observed_trigger_deliveries(rows.iter())?;
                assert_eq!(index.len(), count);
                assert_eq!(index.capacity(), count);
                assert!(index.windows(2).all(|pair| pair[0].0 < pair[1].0));
                for (id, _, ordinal) in &index {
                    assert_eq!(id.as_ptr(), rows[*ordinal].external_trigger_id.as_ptr());
                }
                assert_eq!(rows, before);
                assert_eq!(
                    phase.finish()?,
                    (
                        header().0 + round as u64 * one.0,
                        header().1 + round * one.1
                    )
                );
                assert!(Arc::ptr_eq(
                    &holder,
                    &ResourceReadMaterial::active_guard()?.unwrap()
                ));
            }
        }
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, total.0);
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn observed_trigger_filter_and_last_effective_duplicate_match_owning_index() -> Result<()> {
    let rows = vec![
        trigger("", Some(90), true),
        trigger("same", Some(90), true),
        trigger("汉字\0🦀", Some(80), true),
        trigger("same", Some(20), true),
        trigger("", Some(10), true),
        trigger("same", Some(999), false),
        trigger("same", None, true),
    ];
    let borrowed = queue_observed_trigger_deliveries(rows.iter())?;
    let owned = queue_trigger_deliveries(rows.clone())?;
    let expected = legacy(&rows);
    assert_eq!(borrowed.len(), expected.len());
    for id in ["", "same", "汉字\0🦀", "missing", "sam"] {
        assert_eq!(
            queue_trigger_delivery(&borrowed, id),
            expected.get(id).copied()
        );
        assert_eq!(
            queue_trigger_delivery(&borrowed, id),
            queue_trigger_delivery(&owned, id)
        );
    }
    assert_eq!(
        borrowed.iter().map(|row| row.2).collect::<Vec<_>>(),
        owned.iter().map(|row| row.2).collect::<Vec<_>>()
    );
    Ok(())
}

#[test]
fn observed_trigger_rejects_both_axes_before_index_and_keeps_original_latch() -> Result<()> {
    let rows = vec![trigger("same", Some(1), true); 9];
    let before = rows.clone();
    let one = borrowed_slots(rows.len());
    for bytes_axis in [false, true] {
        let total = (header().0 + 2 * one.0, header().1 + 2 * one.1);
        let limits = if bytes_axis {
            (total.0 - 1, total.1)
        } else {
            (total.0, total.1 - 1)
        };
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), limits.0, limits.1)?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            let index = queue_observed_trigger_deliveries(rows.iter())?;
            assert_eq!(index.len(), 1);
            assert_eq!(index.capacity(), rows.len());
            assert_eq!(phase.finish()?, (header().0 + one.0, header().1 + one.1));
            let error = derive_observed_context_work_queue(
                None,
                QueueLatestWork::new(),
                &rows,
                vec![],
                vec![],
            )
            .unwrap_err();
            assert!(
                error.to_string().contains("queue trigger slots admission"),
                "{error:#}"
            );
            assert!(error.downcast_ref::<std::io::Error>().is_some());
            assert!(phase.finish().is_err());
            assert!(ResourceReadMaterial::active_guard().is_err());
            assert!(queue_observed_trigger_deliveries(rows.iter()).is_err());
            assert_eq!(rows, before);
        }
        let held = header().0 + one.0;
        assert_eq!(ledger.report()?.observed_bytes, held);
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, held);
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn observed_reader_borrows_scope_and_source_without_new_empty_cost() -> Result<()> {
    let home = tempfile::tempdir()?;
    let storage = crate::storage::AppStorage::new_for_test_with_agent(home.path(), "web")?;
    let view = RuntimeReadModels::borrowed_for_agent(&storage.runtime_db, "web")?;
    let state = AgentState::new("web");
    let rows = vec![trigger("delivered", Some(1), true)];
    let cost = (
        header().0 + borrowed_slots(1).0,
        header().1 + borrowed_slots(1).1,
    );
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), cost.0, cost.1)?;
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        let queue = view.work_queue_with_observed_scheduling(&state, &rows)?;
        assert!(queue.items.is_empty());
        assert_eq!(phase.finish()?, cost);
        assert!(Arc::ptr_eq(
            &holder,
            &ResourceReadMaterial::active_guard()?.unwrap()
        ));
        let filtered = vec![
            trigger("filtered", None, true),
            trigger("revoked", Some(1), false),
        ];
        assert_eq!(
            queue_observed_trigger_deliveries(filtered.iter())?.capacity(),
            0
        );
        assert_eq!(phase.finish()?, cost);
        let mut foreign = rows.clone();
        foreign[0].target_agent_id = "other".into();
        let error = view
            .work_queue_with_observed_scheduling(&state, &foreign)
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "WorkQueue Scheduling observation scope differs"
        );
        assert_eq!(phase.finish()?, cost);
    }
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, cost.0);
    drop(holder);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert_eq!(rows[0].external_trigger_id, "delivered");
    Ok(())
}
