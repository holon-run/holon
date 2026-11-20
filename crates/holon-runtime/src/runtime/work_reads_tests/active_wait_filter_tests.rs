use super::*;
use crate::{
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::input_evidence_budget::{InputEvidenceBudgetLedger, InputMemoryCategory},
};

fn legacy_filter(observation: &CapturedWork) -> Result<Vec<&WaitConditionRecord>> {
    let mut selected = Vec::new();
    for wait in observation.unresolved_wait_conditions()? {
        if wait.status != WaitConditionStatus::Active {
            continue;
        }
        let live_owner = match wait.work_item_id.as_deref() {
            Some(id) => observation
                .work_item(id)?
                .is_some_and(|work| work.state == WorkItemState::Open),
            None => true,
        };
        if live_owner {
            selected.push(wait);
        }
    }
    Ok(selected)
}

#[test]
fn active_wait_filter_reuses_raw_allocation_and_preserves_owner_matrix_order() -> Result<()> {
    let mut records = vec![
        CanonicalWorkRecord::Work(work("open", WorkItemState::Open)),
        CanonicalWorkRecord::Work(work("completing", WorkItemState::Completing)),
        CanonicalWorkRecord::Work(work("closed", WorkItemState::Completed)),
    ];
    for (i, owner) in [
        None,
        Some("open"),
        Some("completing"),
        Some("closed"),
        Some("absent"),
        Some("pruned"),
    ]
    .into_iter()
    .enumerate()
    {
        for triggered in [false, true] {
            let mut row = wait(
                &format!("wait-{i}-{triggered}-汉字\0🦀"),
                owner,
                1000 + (i % 2) as i64,
            );
            row.updated_at = at(3000);
            if triggered {
                row.mark_triggered("message", row.updated_at);
            }
            records.push(CanonicalWorkRecord::Wait(row));
        }
    }
    // Bootstrap requires present owning dependencies. Only the reader's
    // defensive parity matrix below contains absent/pruned owner observations.
    for id in ["absent", "pruned"] {
        records.push(CanonicalWorkRecord::Work(work(
            id,
            WorkItemState::Completed,
        )));
    }
    let mut observation = captured(records);
    for (id, availability) in [
        ("absent", WorkAvailability::Absent),
        ("pruned", WorkAvailability::Pruned),
    ] {
        let key = WorkKey::new(WorkFamily::Work, id);
        observation.records.retain(|row| row.key != key);
        observation.observations.insert(key, availability);
    }
    let counts = observation
        .records
        .iter()
        .map(Arc::strong_count)
        .collect::<Vec<_>>();
    let mut raw = observation.unresolved_wait_conditions()?;
    raw.reserve_exact(17);
    let pointer = raw.as_ptr();
    let capacity = raw.capacity();
    let selected = observation.context_active_wait_conditions_from_unresolved(raw)?;
    assert_eq!(selected.as_ptr(), pointer);
    assert_eq!(selected.capacity(), capacity);
    assert_eq!(selected, legacy_filter(&observation)?);
    assert_eq!(selected, observation.context_active_wait_conditions()?);
    assert_eq!(
        selected
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["wait-1-false-汉字\0🦀", "wait-0-false-汉字\0🦀"]
    );
    for row in selected {
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
fn active_wait_filter_keeps_empty_and_fully_filtered_buffer_capacity() -> Result<()> {
    let empty = captured(Vec::new());
    let raw = empty.unresolved_wait_conditions()?;
    let pointer = raw.as_ptr();
    let selected = empty.context_active_wait_conditions_from_unresolved(raw)?;
    assert!(selected.is_empty());
    assert_eq!(selected.capacity(), 0);
    assert_eq!(selected.as_ptr(), pointer);
    assert!(empty.context_active_wait_conditions()?.is_empty());

    let mut triggered = wait("triggered", Some("unprepared-skipped-owner"), 1000);
    triggered.mark_triggered("message", at(2000));
    // This test-only observation deliberately lacks the skipped owner.
    let mut observation = captured(vec![CanonicalWorkRecord::Wait(wait("raw", None, 1000))]);
    let mut record = observation.records[0].as_ref().clone();
    record.record = CanonicalWorkRecord::Wait(triggered);
    if let CanonicalWorkRecord::Wait(row) = &mut record.record {
        row.id = "raw".into();
    }
    observation.records[0] = Arc::new(record);
    let raw = observation.unresolved_wait_conditions()?;
    let pointer = raw.as_ptr();
    let capacity = raw.capacity();
    let selected = observation.context_active_wait_conditions_from_unresolved(raw)?;
    assert!(selected.is_empty());
    assert_eq!(selected.as_ptr(), pointer);
    assert_eq!(selected.capacity(), capacity);
    assert!(capacity > 0);
    assert!(observation.context_active_wait_conditions()?.is_empty());
    Ok(())
}

#[test]
fn active_wait_filter_preserves_complete_validation_and_first_owner_error() -> Result<()> {
    let mut observation = captured(vec![
        CanonicalWorkRecord::Work(work("first-owner", WorkItemState::Open)),
        CanonicalWorkRecord::Work(work("second-owner", WorkItemState::Open)),
        CanonicalWorkRecord::Wait(wait("a-first", Some("first-owner"), 2000)),
        CanonicalWorkRecord::Wait(wait("b-second", Some("second-owner"), 1000)),
    ]);
    for owner in ["first-owner", "second-owner"] {
        observation
            .observations
            .remove(&WorkKey::new(WorkFamily::Work, owner));
    }
    let error = observation.context_active_wait_conditions().unwrap_err();
    assert_eq!(
        error.to_string(),
        legacy_filter(&observation).unwrap_err().to_string()
    );
    assert!(
        matches!(error.downcast_ref::<WorkReadError>(), Some(WorkReadError::NeedsPreparation(key))
            if key == &WorkKey::new(WorkFamily::Work, "first-owner"))
    );
    let first = WorkKey::new(WorkFamily::Work, "first-owner");
    observation
        .observations
        .insert(first.clone(), WorkAvailability::Present);
    let index = observation
        .records
        .iter()
        .position(|row| row.key == first)
        .unwrap();
    let original = observation.records[index].clone();
    let mut wrong_family = original.as_ref().clone();
    wrong_family.record = CanonicalWorkRecord::Wait(wait("first-owner", None, 1000));
    observation.records[index] = Arc::new(wrong_family);
    assert_eq!(
        observation
            .context_active_wait_conditions()
            .unwrap_err()
            .to_string(),
        "captured Work key has a different family"
    );
    observation.records[index] = original;
    observation.observations.remove(&first);
    // Raw selection validates every Wait before even the first owner lookup.
    let second = WorkKey::new(WorkFamily::Wait, "b-second");
    observation.observations.remove(&second);
    let error = observation.context_active_wait_conditions().unwrap_err();
    assert!(
        matches!(error.downcast_ref::<WorkReadError>(), Some(WorkReadError::NeedsPreparation(key))
            if key == &second)
    );
    observation
        .observations
        .insert(second.clone(), WorkAvailability::Present);
    let index = observation
        .records
        .iter()
        .position(|row| row.key == second)
        .unwrap();
    let mut record = observation.records[index].as_ref().clone();
    if let CanonicalWorkRecord::Wait(row) = &mut record.record {
        row.status = WaitConditionStatus::Resolved;
    }
    observation.records[index] = Arc::new(record);
    assert_eq!(
        observation
            .context_active_wait_conditions()
            .unwrap_err()
            .to_string(),
        "captured unresolved Wait has a terminal state"
    );
    observation.live_complete = false;
    assert!(matches!(
        observation
            .context_active_wait_conditions()
            .unwrap_err()
            .downcast_ref::<WorkReadError>(),
        Some(WorkReadError::IncompleteLive)
    ));
    Ok(())
}

#[test]
fn active_wait_filter_keeps_original_phase_and_selected_record_holder() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
    let mut observation = captured(vec![
        CanonicalWorkRecord::Work(work("owner", WorkItemState::Open)),
        CanonicalWorkRecord::Wait(wait("selected", Some("owner"), 1000)),
    ]);
    let view =
        ledger.register_work_view(&scope(), InputMemoryCategory::Pins, &observation.records)?;
    let selected_holder = Arc::downgrade(&view);
    observation.budget_views.push(view);
    observation.budget = Some(ledger.clone());
    let record = Arc::downgrade(
        observation
            .records
            .iter()
            .find(|row| row.key.family == WorkFamily::Wait)
            .unwrap(),
    );
    let header = ((scope().agent_id.len() as u64 + 256) * 8, 64);
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), header.0, header.1)?;
    let guard = phase.guard()?;
    let material = Arc::downgrade(&guard);
    let bytes = ledger.report()?.observed_bytes;
    {
        let _entry = phase.enter();
        let active = ResourceReadMaterial::active_guard()?.unwrap();
        assert!(Arc::ptr_eq(&active, &guard));
        let selected = observation.context_active_wait_conditions()?;
        assert_eq!(selected.len(), 1);
        assert!(std::ptr::eq(
            selected[0],
            observation.wait_condition("selected")?.unwrap()
        ));
        assert_eq!(phase.finish()?, header);
        assert_eq!(ledger.report()?.observed_bytes, bytes);
        // A refusal remains latched; pure reference filtering must not reset it.
        assert!(crate::diagnostics::resource_io::admit_owned_metadata(1, 0).is_err());
        assert!(phase.finish().is_err());
        assert_eq!(observation.context_active_wait_conditions()?.len(), 1);
        assert!(ResourceReadMaterial::active_guard().is_err());
        assert_eq!(ledger.report()?.observed_bytes, bytes);
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop((phase, guard));
    assert!(material.upgrade().is_none());
    assert!(record.upgrade().is_some());
    assert!(selected_holder.upgrade().is_some());
    let last = Arc::new(observation);
    let first = last.clone();
    drop(first);
    assert!(selected_holder.upgrade().is_some());
    assert_eq!(last.context_active_wait_conditions()?.len(), 1);
    drop(last);
    assert!(record.upgrade().is_none());
    assert!(selected_holder.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[tokio::test]
async fn scheduler_caller_keeps_active_wait_value_and_original_last_holder() -> Result<()> {
    use crate::runtime::{
        tests::support::{context_config, StubProvider},
        WaitForWakeKind,
    };
    let home = tempfile::tempdir()?;
    let runtime = RuntimeHandle::new(
        "active-filter-caller",
        home.path().into(),
        None::<crate::types::WorkspaceEntry>,
        String::new(),
        Arc::new(StubProvider::new("unused")),
        "active-filter-caller".into(),
        context_config(),
    )?;
    let owner = runtime
        .create_work_item("active wait owner".into(), None, None, Vec::new())
        .await?;
    runtime
        .register_wait_for(
            "active-filter-caller",
            Some(owner.id.clone()),
            WaitForWakeKind::External,
            Some("external:active-filter".into()),
            "wait for external".into(),
            None,
        )
        .await?;
    runtime.join_resource_worker().await?;
    let mut mutation = runtime.begin_mutation().await?;
    let read = mutation
        .prepare_scheduler_read(None, Default::default(), Default::default(), runtime.now())
        .await?;
    drop(mutation);
    let raw = read.work.context_active_wait_conditions()?;
    assert_eq!(raw.len(), 1);
    assert_eq!(
        read.active_waits,
        raw.into_iter().cloned().collect::<Vec<_>>()
    );
    assert_eq!(
        read.active_waits,
        runtime
            .storage()
            .active_wait_conditions_for_agent("active-filter-caller")?
    );
    let trigger = read.active_waits[0]
        .wake_sources
        .iter()
        .find_map(|source| match source {
            crate::types::WakeSource::ExternalIngress {
                external_trigger_id,
            } => Some(external_trigger_id.as_deref()),
            _ => None,
        })
        .unwrap();
    let trigger_id = trigger.unwrap_or("wildcard-filter-test");
    assert_eq!(
        runtime.wake_hint_work_item_id(Some(trigger_id)).await?,
        Some(owner.id.clone())
    );
    assert_eq!(
        runtime
            .exact_external_wait_correlation(Some(trigger_id))
            .await?,
        trigger.map(|_| read.active_waits[0].id.clone())
    );
    let holder = Arc::downgrade(&read._owned_budget);
    let expected = serde_json::to_value(&read.active_waits)?;
    let first = Arc::new(read);
    let last = first.clone();
    drop(first);
    assert!(holder.upgrade().is_some());
    assert_eq!(serde_json::to_value(&last.active_waits)?, expected);
    assert_eq!(last.work.context_active_wait_conditions()?.len(), 1);
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop(last);
    assert!(holder.upgrade().is_none());
    Ok(())
}
