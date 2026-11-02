use super::*;
use crate::{
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::input_evidence_budget::{InputEvidenceBudgetLedger, InputMemoryCategory},
};

fn legacy_lookup<'a>(
    observation: &'a CapturedWork,
    key: &WorkKey,
) -> Result<Option<&'a Arc<QualifiedWorkRecord>>> {
    match observation.observations.get(key) {
        Some(WorkAvailability::Present) => observation
            .records
            .iter()
            .find(|row| row.key == *key)
            .map(Some)
            .ok_or_else(|| WorkReadError::NeedsPreparation(key.clone()).into()),
        Some(WorkAvailability::Absent | WorkAvailability::Pruned) => Ok(None),
        None => Err(WorkReadError::NeedsPreparation(key.clone()).into()),
    }
}

fn assert_lookup_parity(observation: &CapturedWork, family: WorkFamily, id: &str) {
    let key = WorkKey::new(family, id);
    let expected = legacy_lookup(observation, &key);
    for actual in [
        observation.lookup_parts(family, id),
        observation.lookup(&key),
    ] {
        match (&expected, actual) {
            (Ok(Some(expected)), Ok(Some(actual))) => assert!(std::ptr::eq(*expected, actual)),
            (Ok(None), Ok(None)) => {}
            (Err(expected), Err(actual)) => {
                assert_eq!(actual.to_string(), expected.to_string());
                assert!(matches!(actual.downcast_ref::<WorkReadError>(),
                        Some(WorkReadError::NeedsPreparation(actual)) if actual == &key));
            }
            (expected, actual) => panic!("lookup differs: {expected:?} / {actual:?}"),
        }
    }
}

#[test]
fn borrowed_lookup_preserves_composite_order_and_same_id_family_isolation() -> Result<()> {
    let long = "汉字\0🦀".repeat(1024);
    let ids = ["", "\0", "A", "a", "a\0", "汉字", "🦀", long.as_str()];
    let mut records = Vec::new();
    for id in ids.iter().rev() {
        records.push(CanonicalWorkRecord::Wait(wait(id, None, 1000)));
        records.push(CanonicalWorkRecord::Work(work(id, WorkItemState::Open)));
    }
    let mut observation = captured(records);
    for id in ids {
        observation.observations.insert(
            WorkKey::new(WorkFamily::Frame, id),
            WorkAvailability::Pruned,
        );
    }
    let counts = observation
        .records
        .iter()
        .map(Arc::strong_count)
        .collect::<Vec<_>>();
    for id in ids.into_iter().chain(["missing", "a\0\0", "汉"]) {
        for family in [WorkFamily::Work, WorkFamily::Wait, WorkFamily::Frame] {
            assert_lookup_parity(&observation, family, id);
        }
    }
    for id in ids {
        let expected_work = observation.lookup_parts(WorkFamily::Work, id)?.unwrap();
        let expected_wait = observation.lookup_parts(WorkFamily::Wait, id)?.unwrap();
        match (&expected_work.record, &expected_wait.record) {
            (CanonicalWorkRecord::Work(work), CanonicalWorkRecord::Wait(wait)) => {
                assert!(std::ptr::eq(observation.work_item(id)?.unwrap(), work));
                assert!(std::ptr::eq(observation.wait_condition(id)?.unwrap(), wait));
            }
            _ => panic!("lookup crossed families"),
        }
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
fn borrowed_lookup_keeps_observation_precedence_missing_body_and_owned_errors() -> Result<()> {
    for family in [WorkFamily::Work, WorkFamily::Wait] {
        for availability in [
            Some(WorkAvailability::Present),
            Some(WorkAvailability::Absent),
            Some(WorkAvailability::Pruned),
            None,
        ] {
            for keep_body in [false, true] {
                let id = "same-id\0汉字🦀";
                let mut observation = captured(vec![
                    CanonicalWorkRecord::Work(work(id, WorkItemState::Open)),
                    CanonicalWorkRecord::Wait(wait(id, None, 1000)),
                ]);
                let key = WorkKey::new(family, id);
                match availability {
                    Some(value) => {
                        observation.observations.insert(key.clone(), value);
                    }
                    None => {
                        observation.observations.remove(&key);
                    }
                }
                if !keep_body {
                    observation.records.retain(|row| row.key != key);
                }
                assert_lookup_parity(&observation, family, id);
                let typed = match family {
                    WorkFamily::Work => observation.work_item(id).map(|value| value.is_some()),
                    WorkFamily::Wait => observation.wait_condition(id).map(|value| value.is_some()),
                    WorkFamily::Frame => unreachable!(),
                };
                match (availability, keep_body) {
                    (Some(WorkAvailability::Present), true) => assert!(typed?),
                    (Some(WorkAvailability::Absent | WorkAvailability::Pruned), _) => {
                        assert!(!typed?);
                    }
                    _ => {
                        let error = typed.unwrap_err();
                        assert!(matches!(error.downcast_ref::<WorkReadError>(),
                                Some(WorkReadError::NeedsPreparation(actual)) if actual == &key));
                    }
                }
            }
        }
    }
    Ok(())
}

#[test]
fn borrowed_typed_lookup_preserves_family_validation_without_live_requirement() -> Result<()> {
    let mut observation = captured(vec![
        CanonicalWorkRecord::Work(work("owner", WorkItemState::Open)),
        CanonicalWorkRecord::Wait(wait("waiting", None, 1000)),
    ]);
    observation.live_complete = false;
    assert!(observation.work_item("owner")?.is_some());
    assert!(observation.wait_condition("waiting")?.is_some());
    for (family, id, message) in [
        (
            WorkFamily::Work,
            "owner",
            "captured Work key has a different family",
        ),
        (
            WorkFamily::Wait,
            "waiting",
            "captured Wait key has a different family",
        ),
    ] {
        let key = WorkKey::new(family, id);
        let index = observation
            .records
            .iter()
            .position(|row| row.key == key)
            .unwrap();
        let mut row = observation.records[index].as_ref().clone();
        row.record = match family {
            WorkFamily::Work => CanonicalWorkRecord::Wait(wait(id, None, 1000)),
            WorkFamily::Wait => CanonicalWorkRecord::Work(work(id, WorkItemState::Open)),
            WorkFamily::Frame => unreachable!(),
        };
        observation.records[index] = Arc::new(row);
        assert_lookup_parity(&observation, family, id);
        let error = match family {
            WorkFamily::Work => observation.work_item(id).unwrap_err(),
            WorkFamily::Wait => observation.wait_condition(id).unwrap_err(),
            WorkFamily::Frame => unreachable!(),
        };
        assert_eq!(error.to_string(), message);
    }
    Ok(())
}

#[test]
fn borrowed_lookup_keeps_zero_material_original_phase_latch_and_last_holder() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
    let id = "borrowed-key\0汉字🦀".repeat(1024);
    let mut observation = captured(vec![
        CanonicalWorkRecord::Work(work(&id, WorkItemState::Open)),
        CanonicalWorkRecord::Wait(wait(&id, None, 1000)),
    ]);
    observation.observations.insert(
        WorkKey::new(WorkFamily::Work, "absent"),
        WorkAvailability::Absent,
    );
    observation.observations.insert(
        WorkKey::new(WorkFamily::Wait, "pruned"),
        WorkAvailability::Pruned,
    );
    let view =
        ledger.register_work_view(&scope(), InputMemoryCategory::Pins, &observation.records)?;
    let selected = Arc::downgrade(&view);
    observation.budget_views.push(view);
    observation.budget = Some(ledger.clone());
    let records = observation
        .records
        .iter()
        .map(Arc::downgrade)
        .collect::<Vec<_>>();
    let header = ((scope().agent_id.len() as u64 + 256) * 8, 64);
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), header.0, header.1)?;
    let guard = phase.guard()?;
    let material = Arc::downgrade(&guard);
    let observed = ledger.report()?.observed_bytes;
    let expected_work = observation.work_item(&id)?.unwrap() as *const WorkItemRecord;
    let expected_wait = observation.wait_condition(&id)?.unwrap() as *const WaitConditionRecord;
    {
        let _entry = phase.enter();
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &guard
        ));
        for attempt in 0..2 {
            assert_eq!(
                observation.work_item(&id)?.unwrap() as *const _,
                expected_work
            );
            assert_eq!(
                observation.wait_condition(&id)?.unwrap() as *const _,
                expected_wait
            );
            assert!(observation.work_item("absent")?.is_none());
            assert!(observation.wait_condition("pruned")?.is_none());
            assert_eq!(ledger.report()?.observed_bytes, observed);
            if attempt == 0 {
                assert_eq!(phase.finish()?, header);
                assert!(crate::diagnostics::resource_io::admit_owned_metadata(1, 0).is_err());
            }
            assert!(phase.finish().is_err());
            assert!(ResourceReadMaterial::active_guard().is_err());
        }
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop((phase, guard));
    assert!(material.upgrade().is_none());
    let first = Arc::new(observation);
    let last = first.clone();
    drop(first);
    assert!(selected.upgrade().is_some());
    assert!(records.iter().all(|record| record.upgrade().is_some()));
    assert_eq!(last.work_item(&id)?.unwrap() as *const _, expected_work);
    assert_eq!(
        last.wait_condition(&id)?.unwrap() as *const _,
        expected_wait
    );
    drop(last);
    assert!(selected.upgrade().is_none());
    assert!(records.iter().all(|record| record.upgrade().is_none()));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[tokio::test]
async fn scheduler_typed_lookup_keeps_original_selected_records_and_projection_holder() -> Result<()>
{
    use crate::runtime::{
        tests::support::{context_config, StubProvider},
        WaitForWakeKind,
    };
    let home = tempfile::tempdir()?;
    let runtime = RuntimeHandle::new(
        "borrowed-lookup-caller",
        home.path().into(),
        None::<crate::types::WorkspaceEntry>,
        String::new(),
        Arc::new(StubProvider::new("unused")),
        "borrowed-lookup-caller".into(),
        context_config(),
    )?;
    let owner = runtime
        .create_work_item("borrowed lookup owner".into(), None, None, Vec::new())
        .await?;
    runtime
        .register_wait_for(
            "borrowed-lookup-caller",
            Some(owner.id.clone()),
            WaitForWakeKind::External,
            Some("external:borrowed-lookup".into()),
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
    assert_eq!(read.active_waits.len(), 1);
    let wait_id = read.active_waits[0].id.clone();
    let stored = runtime
        .storage()
        .latest_work_items_for_agent("borrowed-lookup-caller", 128)?
        .into_iter()
        .find(|record| record.id == owner.id)
        .unwrap();
    assert_eq!(read.work.work_item(&owner.id)?, Some(&stored));
    assert_eq!(
        read.work.wait_condition(&wait_id)?,
        Some(&read.active_waits[0])
    );
    let work_ptr = read.work.work_item(&owner.id)?.unwrap() as *const WorkItemRecord;
    let wait_ptr = read.work.wait_condition(&wait_id)?.unwrap() as *const WaitConditionRecord;
    let holder = Arc::downgrade(&read._owned_budget);
    let records = read
        .work
        .records
        .iter()
        .map(Arc::downgrade)
        .collect::<Vec<_>>();
    let first = Arc::new(read);
    let last = first.clone();
    drop(first);
    assert!(holder.upgrade().is_some());
    assert_eq!(
        last.work.work_item(&owner.id)?.unwrap() as *const _,
        work_ptr
    );
    assert_eq!(
        last.work.wait_condition(&wait_id)?.unwrap() as *const _,
        wait_ptr
    );
    let trigger = last.active_waits[0]
        .wake_sources
        .iter()
        .find_map(|source| match source {
            crate::types::WakeSource::ExternalIngress {
                external_trigger_id,
            } => Some(external_trigger_id.as_deref()),
            _ => None,
        })
        .unwrap();
    let trigger_id = trigger.unwrap_or("wildcard-lookup-test");
    assert_eq!(
        runtime.wake_hint_work_item_id(Some(trigger_id)).await?,
        Some(owner.id.clone())
    );
    assert_eq!(
        runtime
            .exact_external_wait_correlation(Some(trigger_id))
            .await?,
        trigger.map(|_| wait_id.clone())
    );
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop(last);
    assert!(holder.upgrade().is_none());
    // Resident roots still own their qualified records after the read is gone.
    assert!(records.iter().all(|record| record.upgrade().is_some()));
    Ok(())
}
