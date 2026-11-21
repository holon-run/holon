use super::*;
use crate::{
    runtime_db::current_query::{
        prepare, prepare_event_page, prepare_event_page_with_scan, CurrentSelection, EventPageScan,
    },
    storage::EventLogPageOrder,
};

#[path = "event_page_budget.rs"]
mod budget;
#[path = "event_page_selection.rs"]
mod selection;

fn append(f: &Fixture, index: usize) -> Result<()> {
    let mut event = AuditEvent::legacy(
        if index % 2 == 0 { "even" } else { "odd" },
        serde_json::json!({"index": index}),
    );
    event.id = format!("finite-event-{index}");
    f.db.audit_events()
        .append(Some(&f.scope.agent_id), &event)?;
    Ok(())
}

#[test]
fn finite_event_page_cost_evidence() -> Result<()> {
    for count in [1_000, 10_000, 50_000] {
        let f = Fixture::new()?;
        f.db.transaction(|tx| {
            for index in 0..count {
                let mut event = AuditEvent::legacy(
                    if index < 20 { "target" } else { "noise" },
                    serde_json::json!({"index": index}),
                );
                event.id = format!("cost-event-{index}");
                append_audit_event_tx(tx, Some(&f.scope.agent_id), &event)?;
            }
            Ok(())
        })?;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        for kind in [None, Some("target"), Some("absent")] {
            let started = std::time::Instant::now();
            let page = f.db.transaction(|tx| {
                prepare_event_page_with_scan(
                    tx,
                    &f.scope,
                    &ledger,
                    None,
                    None,
                    20,
                    EventLogPageOrder::Desc,
                    Some(EventPageScan { kind }),
                    |event| kind.is_none_or(|kind| event.kind == kind),
                )
            })?;
            let elapsed = started.elapsed();
            assert!(page.events().len() <= 20);
            assert!(page
                .events()
                .all(|event| kind.is_none_or(|kind| event.kind == kind)));
            if kind == Some("target") {
                assert_eq!(page.events().len(), 20);
                assert_eq!(page.costs.decoded, 20);
                assert_eq!(page.costs.sorts, 0);
                let (ids, vm_steps) = f.db.with_read(|connection| {
                    let mut statement = connection.prepare(
                        "SELECT audit_event_id FROM audit_events
                         INDEXED BY idx_audit_events_agent_seq_created
                         WHERE agent_id=?1 AND kind='target'
                         ORDER BY event_seq DESC,created_at DESC LIMIT 20",
                    )?;
                    let ids = statement
                        .query_map([&f.scope.agent_id], |row| row.get::<_, String>(0))?
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    Ok((ids, statement.get_status(rusqlite::StatementStatus::VmStep)))
                })?;
                assert_eq!(
                    ids,
                    page.events()
                        .map(|event| event.id.clone())
                        .collect::<Vec<_>>()
                );
                assert!(page.costs.vm_steps < vm_steps);
                eprintln!(
                    "Event-page kind index history={count} sequence_index_vm={vm_steps} canonical_kind_vm={}",
                    page.costs.vm_steps
                );
            }
            eprintln!(
                "Event-page evidence history={count} kind={kind:?} elapsed={elapsed:?} costs={:?}",
                page.costs
            );
            drop(page);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn unqualified_history_page_preserves_sparse_order_filter_lookahead_and_one_cut_metadata(
) -> Result<()> {
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    for index in 0..5 {
        append(&f, index)?;
    }
    for order in [EventLogPageOrder::Asc, EventLogPageOrder::Desc] {
        let page = f.db.transaction(|tx| {
            prepare_event_page(tx, &f.scope, &ledger, None, Some(0), 2, order, |event| {
                event.kind == "even"
            })
        })?;
        let ids = page
            .events()
            .map(|event| event.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            if order == EventLogPageOrder::Asc {
                vec!["finite-event-0", "finite-event-2"]
            } else {
                vec!["finite-event-4", "finite-event-2"]
            }
        );
        assert_eq!(page.has_newer, order == EventLogPageOrder::Asc);
        assert_eq!(page.has_older, order == EventLogPageOrder::Desc);
        assert_eq!(page.cursor_seq, Some(5));
        assert_eq!(page.recovery.event_head_seq, 5);
        assert_eq!(page.recovery.event_log_epoch, f.db.event_log_epoch()?);
        assert!(ledger.report()?.observed_bytes > 0);
        drop(page);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn finite_event_history_reads_committed_changes_but_rejects_foreign_incarnation_and_overflow(
) -> Result<()> {
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    append(&f, 0)?;
    let raw = AuditEvent::legacy("unqualified", serde_json::json!({}));
    f.db.audit_events().append(Some(&f.scope.agent_id), &raw)?;
    let foreign_event = AuditEvent::legacy("another-agent", serde_json::json!({}));
    f.db.audit_events()
        .append(Some("another-agent"), &foreign_event)?;
    let read = |before, after, scope: &InputEvidenceScope| {
        f.db.transaction(|tx| {
            prepare_event_page(
                tx,
                scope,
                &ledger,
                before,
                after,
                1,
                EventLogPageOrder::Asc,
                |_| true,
            )
        })
    };
    let page = read(None, Some(1), &f.scope)?;
    assert_eq!(page.events().next().context("raw event")?.id, raw.id);
    assert_eq!(page.events().len(), 1);
    assert!(!page.has_newer);
    drop(page);
    // Cursor bounds exclude both the next own event and all other agents.
    let page = read(Some(2), Some(0), &f.scope)?;
    assert_eq!(
        page.events().next().context("bounded event")?.id,
        "finite-event-0"
    );
    assert!(!page.has_newer);
    drop(page);
    let mut foreign = f.scope.clone();
    foreign.identity_incarnation += 1;
    assert!(read(Some(2), None, &foreign).is_err());
    assert!(read(Some(u64::MAX), None, &f.scope).is_err());
    f.db.connection()?.execute(
        "UPDATE audit_events SET data_json=json_set(data_json,'$.data.index',999) WHERE audit_event_id='finite-event-0'",
        [],
    )?;
    let page = read(Some(2), Some(0), &f.scope)?;
    assert_eq!(
        page.events().next().context("committed change")?.data["index"],
        999
    );
    drop(page);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn finite_event_history_reads_startup_observations_without_live_promotion_or_writes() -> Result<()>
{
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let event = AuditEvent::legacy(
        "startup_external_side_effect_indeterminate",
        serde_json::json!({}),
    );
    f.db.audit_events()
        .append(Some(&f.scope.agent_id), &event)?;
    f.db.transaction(|tx| {
        let before: (i64, i64) = tx.query_row(
            "SELECT total_changes(),(SELECT COUNT(*) FROM current_read_qualification)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let page = prepare_event_page(
            tx,
            &f.scope,
            &ledger,
            None,
            Some(0),
            10,
            EventLogPageOrder::Asc,
            |_| true,
        )?;
        assert_eq!(page.events().next().context("startup event")?.id, event.id);
        drop(page);
        // Historical visibility never qualifies this event for live Current.
        let live = CurrentSelection {
            events_after: Some(0),
            ..Default::default()
        };
        assert!(prepare(tx, &f.scope, Some(&live), &ledger, (1024 * 1024, 1024)).is_err());
        let after = tx.query_row(
            "SELECT total_changes(),(SELECT COUNT(*) FROM current_read_qualification)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(before, after);
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn finite_event_history_still_rejects_corrupt_body_address_and_sequence() -> Result<()> {
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    append(&f, 0)?;
    let payload: String = f.db.connection()?.query_row(
        "SELECT data_json FROM audit_events WHERE audit_event_id='finite-event-0'",
        [],
        |row| row.get(0),
    )?;
    for replacement in [
        "{".to_string(),
        {
            let mut value: serde_json::Value = serde_json::from_str(&payload)?;
            value["id"] = "wrong-address".into();
            value.to_string()
        },
        {
            let mut value: serde_json::Value = serde_json::from_str(&payload)?;
            value["event_seq"] = 999.into();
            value.to_string()
        },
    ] {
        f.db.connection()?.execute(
            "UPDATE audit_events SET data_json=?1 WHERE audit_event_id='finite-event-0'",
            [&replacement],
        )?;
        let error =
            f.db.transaction(|tx| {
                prepare_event_page(
                    tx,
                    &f.scope,
                    &ledger,
                    None,
                    Some(0),
                    1,
                    EventLogPageOrder::Asc,
                    |_| true,
                )
            })
            .err()
            .context("corrupt history must fail")?;
        assert!(!format!("{error:#}").contains("original writer preparation"));
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn event_direct_decode_ignores_legacy_hints_and_keeps_exact_last_holder() -> Result<()> {
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    for index in 0..3 {
        let event = AuditEvent::legacy(
            "direct-event",
            serde_json::json!({"index": index, "text": "路\\\"{}[]:,", "padding": "x".repeat(64 * 1024)}),
        );
        let keys = [(CurrentFamily::Event, event.id.as_str())];
        drop(attempt(&f, &ledger, &keys, &keys, |tx| {
            append_audit_event_tx(tx, Some(&f.scope.agent_id), &event)?;
            Ok(())
        })?);
    }
    let read = |ledger: &InputEvidenceBudgetLedger| {
        f.db.transaction(|tx| {
            prepare_event_page(
                tx,
                &f.scope,
                ledger,
                None,
                Some(0),
                3,
                EventLogPageOrder::Asc,
                |_| true,
            )
        })
    };
    let page = read(&ledger)?;
    assert_eq!(page.events().len(), 3);
    let bytes = ledger.report()?.observed_bytes;
    drop(page);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let nodes: u64 = f.db.connection()?.query_row(
        "SELECT COUNT(*) FROM audit_events e,json_tree(e.data_json) WHERE e.agent_id=?1",
        [&f.scope.agent_id],
        |row| row.get(0),
    )?;
    // Scope (2), then raw (2), nodes (+1 guard), and vector slot (2) per Event.
    let records = 2 + nodes + 3 * 5;
    for (short_bytes, short_records) in [(0, 0), (1, 0), (0, 1)] {
        let allowed = bytes - short_bytes;
        let limits = crate::config::StateMemoryLimits {
            history_per_agent_bytes: allowed / 2,
            pending_per_agent_bytes: allowed - allowed / 2,
            record_limit: records - short_records,
            ..Default::default()
        };
        let bounded = InputEvidenceBudgetLedger::new(limits)?;
        let result = read(&bounded);
        let page = Arc::new(result?);
        let last = page.clone();
        drop(page);
        assert_eq!(bounded.report()?.observed_bytes, bytes);
        assert_eq!(last.events().len(), 3);
        drop(last);
        assert_eq!(bounded.report()?.observed_bytes, 0);
    }
    Ok(())
}
