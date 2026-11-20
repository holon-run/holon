use super::*;

const BYTE_LIMIT: usize = 8 * 1024 * 1024;

fn set_raw_bytes(f: &Fixture, seq: u64, bytes: usize) -> Result<()> {
    let payload: String = f.db.connection()?.query_row(
        "SELECT data_json FROM audit_events WHERE event_seq=?1",
        [seq],
        |row| row.get(0),
    )?;
    let mut value: serde_json::Value = serde_json::from_str(&payload)?;
    value["payload"]["padding"] = "".into();
    let remaining = bytes - serde_json::to_string(&value)?.len();
    // Multibyte text makes a character-count budget observably incorrect.
    value["payload"]["padding"] = format!(
        "{}{}",
        "界".repeat(remaining / 3),
        "x".repeat(remaining % 3)
    )
    .into();
    let payload = serde_json::to_string(&value)?;
    assert_eq!(payload.len(), bytes);
    assert_eq!(
        f.db.connection()?.execute(
            "UPDATE audit_events SET data_json=?1 WHERE event_seq=?2",
            params![payload, seq],
        )?,
        1
    );
    Ok(())
}

#[test]
fn bounded_event_page_candidate_budget_advances_empty_pages_without_loss() -> Result<()> {
    let f = Fixture::new()?;
    f.db.transaction(|tx| {
        for index in 0..601 {
            let event = AuditEvent::legacy(
                if index == 0 || index == 600 {
                    "target"
                } else {
                    "other"
                },
                serde_json::json!({"index": index}),
            );
            append_audit_event_tx(tx, Some(&f.scope.agent_id), &event)?;
        }
        // Every normalized/canonical disagreement must still be checked.
        tx.execute("UPDATE audit_events SET kind='untrusted-hint'", [])?;
        Ok(())
    })?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    for kind in [None, Some("target")] {
        for order in [EventLogPageOrder::Asc, EventLogPageOrder::Desc] {
            let (mut before, mut after) = (Some(602), Some(0));
            let mut all = Vec::new();
            let mut scanned = 0;
            for page_index in 0..3 {
                let page = f.db.transaction(|tx| {
                    prepare_event_page_with_scan(
                        tx,
                        &f.scope,
                        &ledger,
                        before,
                        after,
                        1000,
                        order,
                        Some(EventPageScan { kind }),
                        |_| true,
                    )
                })?;
                let count = if page_index < 2 { 256 } else { 89 };
                assert_eq!(page.costs.scanned, count);
                assert_eq!(page.costs.decoded, count);
                scanned += count;
                assert_eq!(page.cursor_seq, Some(601));
                assert_eq!(page.recovery.event_head_seq, 601);
                if kind.is_some() && page_index == 1 {
                    assert_eq!(page.events().len(), 0);
                }
                all.extend(page.events().map(|event| event.event_seq));
                let next = if order == EventLogPageOrder::Asc {
                    assert!(!page.has_older && page.next_before_seq.is_none());
                    assert_eq!(page.has_newer, page_index < 2);
                    page.next_after_seq
                } else {
                    assert!(!page.has_newer && page.next_after_seq.is_none());
                    assert_eq!(page.has_older, page_index < 2);
                    page.next_before_seq
                };
                if page_index < 2 {
                    let next = next.context("bounded scan must advance even on empty pages")?;
                    if order == EventLogPageOrder::Asc {
                        assert_eq!(next, 256 * (page_index + 1));
                        assert!(next > after.unwrap());
                        after = Some(next);
                    } else {
                        assert_eq!(next, 602 - 256 * (page_index + 1));
                        assert!(next < before.unwrap());
                        before = Some(next);
                    }
                } else {
                    assert!(next.is_none());
                }
                drop(page);
                assert_eq!(ledger.report()?.observed_bytes, 0);
            }
            let mut expected = if kind.is_some() {
                vec![1, 601]
            } else {
                (1..=601).collect()
            };
            if order == EventLogPageOrder::Desc {
                expected.reverse();
            }
            assert_eq!(all, expected);
            assert_eq!(scanned, 601);
        }
    }
    let exact = f.db.transaction(|tx| {
        prepare_event_page_with_scan(
            tx,
            &f.scope,
            &ledger,
            Some(257),
            Some(0),
            1000,
            EventLogPageOrder::Asc,
            Some(EventPageScan { kind: None }),
            |_| true,
        )
    })?;
    assert_eq!(exact.costs.scanned, 256);
    assert_eq!(exact.events().len(), 256);
    assert!(!exact.has_newer && exact.next_after_seq.is_none());
    Ok(())
}

#[test]
fn bounded_event_page_accepts_exact_raw_utf8_limit_and_rejects_one_more_byte() -> Result<()> {
    let f = Fixture::new()?;
    append(&f, 0)?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    set_raw_bytes(&f, 1, BYTE_LIMIT)?;
    for order in [EventLogPageOrder::Asc, EventLogPageOrder::Desc] {
        let page = f.db.transaction(|tx| {
            prepare_event_page_with_scan(
                tx,
                &f.scope,
                &ledger,
                None,
                None,
                20,
                order,
                Some(EventPageScan { kind: None }),
                |_| true,
            )
        })?;
        assert_eq!(page.events().len(), 1);
        assert_eq!(page.costs.scanned, 1);
        assert_eq!(page.costs.decoded, 1);
        assert_eq!(page.costs.payload_bytes, BYTE_LIMIT);
        assert!(!page.has_older && !page.has_newer);
        assert!(page.next_before_seq.is_none() && page.next_after_seq.is_none());
        drop(page);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    set_raw_bytes(&f, 1, BYTE_LIMIT + 1)?;
    for order in [EventLogPageOrder::Asc, EventLogPageOrder::Desc] {
        let error =
            f.db.transaction(|tx| {
                prepare_event_page_with_scan(
                    tx,
                    &f.scope,
                    &ledger,
                    None,
                    None,
                    20,
                    order,
                    Some(EventPageScan { kind: None }),
                    |_| true,
                )
            })
            .err()
            .context("one excess input byte must fail")?;
        let error = error
            .downcast_ref::<crate::runtime_error::RuntimeError>()
            .context("structured Event-page resource error")?;
        assert_eq!(error.descriptor().code, "event_page_resource_exhausted");
        assert_eq!(
            error.descriptor().domain,
            crate::runtime_error::RuntimeErrorDomain::Runtime
        );
        assert!(!error.descriptor().retryable);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn bounded_event_page_byte_remainder_defers_without_consuming_lookahead() -> Result<()> {
    let f = Fixture::new()?;
    append(&f, 0)?;
    append(&f, 1)?;
    set_raw_bytes(&f, 1, 5 * 1024 * 1024)?;
    set_raw_bytes(&f, 2, 3 * 1024 * 1024 + 1)?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    for order in [EventLogPageOrder::Asc, EventLogPageOrder::Desc] {
        let first_seq = if order == EventLogPageOrder::Asc {
            1
        } else {
            2
        };
        let page = f.db.transaction(|tx| {
            prepare_event_page_with_scan(
                tx,
                &f.scope,
                &ledger,
                Some(3),
                Some(0),
                20,
                order,
                Some(EventPageScan { kind: None }),
                |_| true,
            )
        })?;
        assert_eq!(
            page.events()
                .map(|event| event.event_seq)
                .collect::<Vec<_>>(),
            [first_seq]
        );
        assert_eq!(page.costs.scanned, 1);
        assert_eq!(page.costs.decoded, 1);
        assert_eq!(
            page.costs.payload_bytes,
            if first_seq == 1 {
                5 * 1024 * 1024
            } else {
                3 * 1024 * 1024 + 1
            }
        );
        let next = if order == EventLogPageOrder::Asc {
            page.next_after_seq
        } else {
            page.next_before_seq
        };
        assert_eq!(next, Some(first_seq));
        drop(page);
        let page = f.db.transaction(|tx| {
            prepare_event_page_with_scan(
                tx,
                &f.scope,
                &ledger,
                if order == EventLogPageOrder::Desc {
                    next
                } else {
                    Some(3)
                },
                if order == EventLogPageOrder::Asc {
                    next
                } else {
                    Some(0)
                },
                20,
                order,
                Some(EventPageScan { kind: None }),
                |_| true,
            )
        })?;
        assert_eq!(
            page.events()
                .map(|event| event.event_seq)
                .collect::<Vec<_>>(),
            [3 - first_seq]
        );
        assert!(!page.has_older && !page.has_newer);
        drop(page);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    set_raw_bytes(&f, 2, 3 * 1024 * 1024)?;
    for order in [EventLogPageOrder::Asc, EventLogPageOrder::Desc] {
        let page = f.db.transaction(|tx| {
            prepare_event_page_with_scan(
                tx,
                &f.scope,
                &ledger,
                None,
                None,
                20,
                order,
                Some(EventPageScan { kind: None }),
                |_| true,
            )
        })?;
        assert_eq!(page.costs.payload_bytes, BYTE_LIMIT);
        assert_eq!(page.costs.decoded, 2);
        assert_eq!(page.events().len(), 2);
        assert!(!page.has_older && !page.has_newer);
    }
    Ok(())
}

#[test]
fn bounded_event_page_oversized_corrupt_lookahead_fails_only_on_next_page() -> Result<()> {
    for order in [EventLogPageOrder::Asc, EventLogPageOrder::Desc] {
        let f = Fixture::new()?;
        append(&f, 0)?;
        append(&f, 1)?;
        let first_seq = if order == EventLogPageOrder::Asc {
            1
        } else {
            2
        };
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let corrupt = format!("{{{}", " ".repeat(BYTE_LIMIT));
        f.db.connection()?.execute(
            "UPDATE audit_events SET data_json=?1 WHERE event_seq=?2",
            params![corrupt, 3 - first_seq],
        )?;
        f.db.connection()?
            .execute("UPDATE audit_events SET kind='untrusted-hint'", [])?;
        for kind in [None, Some("absent")] {
            let page = f.db.transaction(|tx| {
                prepare_event_page_with_scan(
                    tx,
                    &f.scope,
                    &ledger,
                    None,
                    None,
                    20,
                    order,
                    Some(EventPageScan { kind }),
                    |_| true,
                )
            })?;
            assert_eq!(page.events().len(), if kind.is_none() { 1 } else { 0 });
            assert_eq!(page.costs.scanned, 1);
            assert_eq!(page.costs.decoded, 1);
            let next = if order == EventLogPageOrder::Asc {
                page.next_after_seq
            } else {
                page.next_before_seq
            };
            assert_eq!(next, Some(first_seq));
            drop(page);
            let error =
                f.db.transaction(|tx| {
                    prepare_event_page_with_scan(
                        tx,
                        &f.scope,
                        &ledger,
                        if order == EventLogPageOrder::Desc {
                            next
                        } else {
                            None
                        },
                        if order == EventLogPageOrder::Asc {
                            next
                        } else {
                            None
                        },
                        20,
                        order,
                        Some(EventPageScan { kind }),
                        |_| true,
                    )
                })
                .err()
                .context("oversized canonical input must fail before JSON parsing")?;
            assert_eq!(
                error
                    .downcast_ref::<crate::runtime_error::RuntimeError>()
                    .context("resource error must win over malformed JSON")?
                    .descriptor()
                    .code,
                "event_page_resource_exhausted"
            );
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}
