use super::*;
use crate::{runtime_db::current_query::prepare_event_page, storage::EventLogPageOrder};

fn append(f: &Fixture, ledger: &InputEvidenceBudgetLedger, index: usize) -> Result<()> {
    let mut event = AuditEvent::legacy(
        if index % 2 == 0 { "even" } else { "odd" },
        serde_json::json!({"index": index}),
    );
    event.id = format!("finite-event-{index}");
    drop(attempt(
        f,
        ledger,
        &[(CurrentFamily::Event, &event.id)],
        &[(CurrentFamily::Event, &event.id)],
        |tx| {
            append_audit_event_tx(tx, Some(&f.scope.agent_id), &event)?;
            Ok(())
        },
    )?);
    Ok(())
}

#[test]
fn qualified_history_page_preserves_sparse_order_filter_lookahead_and_one_cut_metadata(
) -> Result<()> {
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    for index in 0..5 {
        append(&f, &ledger, index)?;
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
fn finite_event_history_rejects_raw_changed_foreign_incarnation_and_overflow() -> Result<()> {
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    append(&f, &ledger, 0)?;
    let raw = AuditEvent::legacy("unqualified", serde_json::json!({}));
    f.db.audit_events().append(Some(&f.scope.agent_id), &raw)?;
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
    assert!(format!(
        "{:#}",
        read(None, Some(1), &f.scope)
            .err()
            .context("raw must fail")?
    )
    .contains("Current body needs original writer preparation"));
    // The finite bound does not prepare or inspect bodies beyond it.
    let page = read(Some(2), Some(0), &f.scope)?;
    assert_eq!(page.events().len(), 1);
    drop(page);
    let mut foreign = f.scope.clone();
    foreign.identity_incarnation += 1;
    assert!(read(Some(2), None, &foreign).is_err());
    assert!(read(Some(u64::MAX), None, &f.scope).is_err());
    f.db.connection()?.execute(
        "UPDATE audit_events SET data_json=json_set(data_json,'$.data.index',999) WHERE audit_event_id='finite-event-0'",
        [],
    )?;
    assert!(format!(
        "{:#}",
        read(Some(2), Some(0), &f.scope)
            .err()
            .context("digest must fail")?
    )
    .contains("Current body needs original writer preparation"));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn finite_event_direct_decode_keeps_exact_cumulative_axes_and_last_holder() -> Result<()> {
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
        if short_bytes != 0 || short_records != 0 {
            let error = result
                .err()
                .context("one-short Event material must refuse")?;
            assert!(
                format!("{error:#}").contains("Input read material"),
                "{error:#}"
            );
            assert_eq!(bounded.report()?.observed_bytes, 0);
            continue;
        }
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
