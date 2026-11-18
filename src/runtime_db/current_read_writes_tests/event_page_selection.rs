use super::*;

#[test]
fn kind_selection_does_not_hide_duplicate_canonical_fields() -> Result<()> {
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    append(&f, 0)?;
    let payload: String = f.db.connection()?.query_row(
        "SELECT data_json FROM audit_events WHERE event_seq=1",
        [],
        |row| row.get(0),
    )?;
    for field in serde_json::from_str::<serde_json::Value>(&payload)?
        .as_object()
        .unwrap()
        .keys()
    {
        for key in [
            field.to_owned(),
            format!(r"\u{:04x}{}", field.as_bytes()[0], &field[1..]),
        ] {
            let duplicate = format!(r#"{},"{key}":null}}"#, &payload[..payload.len() - 1]);
            f.db.connection()?.execute(
                "UPDATE audit_events SET data_json=?1 WHERE event_seq=1",
                [duplicate],
            )?;
            for order in [EventLogPageOrder::Asc, EventLogPageOrder::Desc] {
                let error =
                    f.db.transaction(|tx| {
                        prepare_event_page_with_scan(
                            tx,
                            &f.scope,
                            &ledger,
                            None,
                            None,
                            1,
                            order,
                            Some(EventPageScan {
                                kind: Some("absent"),
                            }),
                            |_| true,
                        )
                    })
                    .err()
                    .with_context(|| format!("duplicate field {key} must not be skipped"))?;
                assert!(
                    format!("{error:#}").contains("duplicate field"),
                    "{error:#}"
                );
                assert_eq!(ledger.report()?.observed_bytes, 0);
            }
        }
    }
    Ok(())
}

#[test]
fn continuation_preserves_both_bounds_and_never_consumes_lookahead() -> Result<()> {
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    for index in 0..9 {
        append(&f, index)?;
    }
    for order in [EventLogPageOrder::Asc, EventLogPageOrder::Desc] {
        let (mut before, mut after) = (Some(9), Some(1));
        let mut all = Vec::new();
        let mut watermarks = Vec::new();
        loop {
            let page = f.db.transaction(|tx| {
                prepare_event_page_with_scan(
                    tx,
                    &f.scope,
                    &ledger,
                    before,
                    after,
                    1,
                    order,
                    Some(EventPageScan { kind: None }),
                    |event| event.event_seq % 3 == 0,
                )
            })?;
            all.extend(page.events().map(|event| event.event_seq));
            assert_eq!(page.cursor_seq, Some(9));
            assert_eq!(page.recovery.event_head_seq, 9);
            let continuation = match order {
                EventLogPageOrder::Asc => {
                    assert!(page.next_before_seq.is_none());
                    assert_eq!(page.has_newer, page.next_after_seq.is_some());
                    page.next_after_seq
                }
                EventLogPageOrder::Desc => {
                    assert!(page.next_after_seq.is_none());
                    assert_eq!(page.has_older, page.next_before_seq.is_some());
                    page.next_before_seq
                }
            };
            drop(page);
            assert_eq!(ledger.report()?.observed_bytes, 0);
            let Some(next) = continuation else { break };
            assert!(next > after.unwrap_or(0) && next < before.unwrap_or(u64::MAX));
            watermarks.push(next);
            match order {
                EventLogPageOrder::Asc => after = Some(next),
                EventLogPageOrder::Desc => before = Some(next),
            }
            assert!(watermarks.len() < 9, "continuation must strictly advance");
        }
        assert_eq!(
            (all, watermarks),
            if order == EventLogPageOrder::Asc {
                (vec![3, 6], vec![3, 6])
            } else {
                (vec![6, 3], vec![6, 3])
            }
        );
    }
    Ok(())
}

#[test]
fn kind_selection_preserves_canonical_mismatch_and_corruption() -> Result<()> {
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    for index in 0..9 {
        append(&f, index)?;
    }
    // Both directions of disagreement follow canonical kind, not the
    // normalized column. These rows move into the disjoint fallback range.
    f.db.connection()?.execute(
        "UPDATE audit_events SET kind=CASE WHEN kind='even' THEN 'odd' ELSE 'even' END
         WHERE event_seq BETWEEN 3 AND 6",
        [],
    )?;
    for order in [EventLogPageOrder::Asc, EventLogPageOrder::Desc] {
        let page = f.db.transaction(|tx| {
            prepare_event_page_with_scan(
                tx,
                &f.scope,
                &ledger,
                Some(9),
                Some(1),
                20,
                order,
                Some(EventPageScan { kind: Some("even") }),
                |_| true,
            )
        })?;
        let mut expected = vec![3, 5, 7];
        if order == EventLogPageOrder::Desc {
            expected.reverse();
        }
        assert_eq!(
            page.events()
                .map(|event| event.event_seq)
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(page.cursor_seq, Some(9));
        assert!(!page.has_older && !page.has_newer);
        drop(page);
    }
    let payload: String = f.db.connection()?.query_row(
        "SELECT data_json FROM audit_events WHERE event_seq=1",
        [],
        |row| row.get(0),
    )?;
    for replacement in [
        "{".to_owned(),
        serde_json::from_str::<serde_json::Value>(&payload).map(|mut value| {
            value["id"] = "foreign".into();
            value.to_string()
        })?,
        serde_json::from_str::<serde_json::Value>(&payload).map(|mut value| {
            value["event_seq"] = 999.into();
            value.to_string()
        })?,
    ] {
        f.db.connection()?.execute(
            "UPDATE audit_events SET data_json=?1 WHERE event_seq=1",
            [&replacement],
        )?;
        let result = f.db.transaction(|tx| {
            prepare_event_page_with_scan(
                tx,
                &f.scope,
                &ledger,
                Some(2),
                None,
                1,
                EventLogPageOrder::Asc,
                Some(EventPageScan {
                    kind: Some("absent"),
                }),
                |_| true,
            )
        });
        assert!(
            result.is_err(),
            "untrusted selection hints cannot hide corrupt fallback rows"
        );
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
