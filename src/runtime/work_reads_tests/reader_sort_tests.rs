use super::*;

#[test]
fn unresolved_reader_in_place_order_preserves_timestamps_ids_and_borrowed_records() -> Result<()> {
    let observation = captured(
        (0..48)
            .rev()
            .map(|i| {
                let mut row = wait(&format!("wait-{i:02}-汉字\0🦀"), None, 1000 + i % 2 * 1000);
                row.updated_at = at(3000 + i % 3 * 1000);
                if i % 4 == 0 {
                    row.mark_triggered(&format!("message-{i}"), row.updated_at);
                }
                CanonicalWorkRecord::Wait(row)
            })
            .collect(),
    );
    let counts = observation
        .records
        .iter()
        .map(Arc::strong_count)
        .collect::<Vec<_>>();
    let sorted = observation.unresolved_wait_conditions()?;
    // Independent descending time buckets and ascending unique IDs.
    let mut expected = Vec::new();
    for updated in (0..3).rev() {
        for created in (0..2).rev() {
            for i in 0..48 {
                if i % 3 == updated && i % 2 == created {
                    expected.push(format!("wait-{i:02}-汉字\0🦀"));
                }
            }
        }
    }
    assert_eq!(
        sorted.iter().map(|row| &row.id).collect::<Vec<_>>(),
        expected.iter().collect::<Vec<_>>()
    );
    assert_eq!(sorted.len(), observation.unresolved_wait_ids.len());
    assert_eq!(
        sorted
            .iter()
            .filter(|row| row.status == WaitConditionStatus::Triggered)
            .count(),
        12
    );
    for row in sorted {
        assert!(std::ptr::eq(
            row,
            observation.wait_condition(&row.id)?.unwrap()
        ));
    }
    assert_eq!(
        observation
            .records
            .iter()
            .map(Arc::strong_count)
            .collect::<Vec<_>>(),
        counts
    );
    Ok(())
}

#[test]
fn blocked_reader_in_place_order_preserves_normalized_ties_predicates_and_due_window() -> Result<()>
{
    let observation = captured(
        (0..48)
            .rev()
            .map(|i| {
                let mut row = work(
                    &format!("work-{i:02}-汉字\0🦀"),
                    if i % 5 == 0 {
                        WorkItemState::Completing
                    } else {
                        WorkItemState::Open
                    },
                );
                row.blocked_by = (i % 11 != 0).then(|| "dependency".into());
                row.recheck_at =
                    Some(at(3000 + i % 3) + chrono::Duration::nanoseconds(i % 2 * 900_000));
                // Equal milliseconds count as consumed even if nanoseconds differ.
                row.recheck_consumed_at = (i % 7 == 0).then(|| at(3000 + i % 3));
                CanonicalWorkRecord::Work(row)
            })
            .collect(),
    );
    let sorted = observation.blocked_work_rechecks()?;
    let mut expected = Vec::new();
    for millis in 0..3 {
        for i in 0..48 {
            if i % 3 == millis && i % 5 != 0 && i % 11 != 0 && i % 7 != 0 {
                expected.push(format!("work-{i:02}-汉字\0🦀"));
            }
        }
    }
    assert_eq!(
        sorted.iter().map(|row| &row.id).collect::<Vec<_>>(),
        expected.iter().collect::<Vec<_>>()
    );
    for row in &sorted {
        assert!(std::ptr::eq(*row, observation.work_item(&row.id)?.unwrap()));
    }
    assert_eq!(observation.next_work_recheck_at()?, Some(at(3000)));
    assert!(observation.due_work_rechecks(at(2999))?.is_empty());
    let expected_due = sorted
        .iter()
        .copied()
        .filter(|row| row.recheck_at.unwrap().timestamp_millis() <= 3001)
        .collect::<Vec<_>>();
    assert_eq!(observation.due_work_rechecks(at(3001))?, expected_due);
    assert_eq!(observation.due_work_rechecks(at(3002))?, sorted);
    Ok(())
}

#[test]
fn unresolved_reader_order_preserves_agent_deadline_first_match_and_context_filter() -> Result<()> {
    let mut triggered = wait("triggered", None, 2000);
    triggered.mark_triggered("message", at(6000));
    let mut owned = wait("owned", Some("owner"), 1000);
    owned.updated_at = at(5500);
    let mut newest = wait("z-newest", None, 3000);
    newest.updated_at = at(5000);
    let mut same = newest.clone();
    same.id = "a-same".into();
    let mut older = wait("a-older", None, 2000);
    older.updated_at = at(4000);
    let observation = captured(vec![
        CanonicalWorkRecord::Wait(older),
        CanonicalWorkRecord::Wait(newest),
        CanonicalWorkRecord::Wait(same),
        CanonicalWorkRecord::Wait(owned),
        CanonicalWorkRecord::Wait(triggered),
        CanonicalWorkRecord::Work(work("owner", WorkItemState::Open)),
    ]);
    let selected = observation.unresolved_wait_conditions()?;
    assert_eq!(
        selected
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["triggered", "owned", "a-same", "z-newest", "a-older"]
    );
    assert_eq!(observation.next_agent_wait_recheck_at()?, Some(at(3000)));
    assert!(observation.due_agent_wait_recheck(at(2999))?.is_none());
    let due = observation.due_agent_wait_recheck(at(3000))?.unwrap();
    assert_eq!(due.id, "a-same");
    assert!(std::ptr::eq(
        due,
        observation.wait_condition("a-same")?.unwrap()
    ));
    assert_eq!(
        observation
            .context_active_wait_conditions()?
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["owned", "a-same", "z-newest", "a-older"]
    );
    Ok(())
}

#[test]
fn in_place_reader_sort_keeps_empty_incomplete_missing_and_invalid_body_errors() -> Result<()> {
    let mut empty = captured(Vec::new());
    assert!(empty.unresolved_wait_conditions()?.is_empty());
    assert!(empty.blocked_work_rechecks()?.is_empty());
    empty.live_complete = false;
    for error in [
        empty.unresolved_wait_conditions().unwrap_err(),
        empty.blocked_work_rechecks().unwrap_err(),
    ] {
        assert!(matches!(
            error.downcast_ref::<WorkReadError>(),
            Some(WorkReadError::IncompleteLive)
        ));
    }
    for family in [WorkFamily::Work, WorkFamily::Wait] {
        let original = match family {
            WorkFamily::Work => CanonicalWorkRecord::Work(work("selected", WorkItemState::Open)),
            WorkFamily::Wait => CanonicalWorkRecord::Wait(wait("selected", None, 1000)),
            _ => unreachable!(),
        };
        let mut observation = captured(vec![original]);
        let key = WorkKey::new(family, "selected");
        observation.observations.remove(&key);
        let error = match family {
            WorkFamily::Work => observation.blocked_work_rechecks().unwrap_err(),
            WorkFamily::Wait => observation.unresolved_wait_conditions().unwrap_err(),
            _ => unreachable!(),
        };
        assert!(
            matches!(error.downcast_ref::<WorkReadError>(), Some(WorkReadError::NeedsPreparation(missing)) if missing == &key)
        );
        observation
            .observations
            .insert(key, WorkAvailability::Present);
        let mut invalid = observation.records[0].as_ref().clone();
        match &mut invalid.record {
            CanonicalWorkRecord::Work(row) => row.state = WorkItemState::Completed,
            CanonicalWorkRecord::Wait(row) => row.status = WaitConditionStatus::Resolved,
            _ => unreachable!(),
        }
        // A deliberately invalid post-capture fixture, never new qualification.
        observation.records[0] = Arc::new(invalid);
        let error = match family {
            WorkFamily::Work => observation.blocked_work_rechecks().unwrap_err(),
            WorkFamily::Wait => observation.unresolved_wait_conditions().unwrap_err(),
            _ => unreachable!(),
        };
        assert_eq!(
            error.to_string(),
            match family {
                WorkFamily::Work => "captured live Work has a non-live state",
                WorkFamily::Wait => "captured unresolved Wait has a terminal state",
                _ => unreachable!(),
            }
        );
    }
    Ok(())
}
