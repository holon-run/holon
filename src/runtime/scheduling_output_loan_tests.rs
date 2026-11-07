use super::*;

#[test]
fn loaded_loans_admit_handle_storage_in_the_original_phase() -> Result<()> {
    for shortage in [None, Some(true), Some(false)] {
        let selected = fixture(9)?;
        let ledger = ledger()?;
        // Both fixtures select three Timers and nine Triggers. There is one
        // phase header, one output-ledger registration and one shared owner.
        let body = body_cost(&selected, 3, true)?;
        let registration = (((scope().agent_id.len() + 256) * 8) as u64, 64);
        let bytes = body.0 + registration.0;
        let records = body.1 + registration.1;
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope(),
            bytes - u64::from(shortage == Some(true)),
            records - usize::from(shortage == Some(false)),
        )?;
        let result = phase.read(|| {
            project_scheduling_records(
                selected,
                3,
                &ledger,
                64 * 1024 * 1024,
                65536,
                (0, 0),
                Vec::new(),
                false,
            )
        });
        if shortage.is_none() {
            let output = result?;
            assert_eq!(phase.finish()?, (bytes, records));
            let weak = Arc::downgrade(&phase.guard()?);
            let last = output.0[0].clone();
            drop((output, phase));
            assert!(weak.upgrade().is_some());
            drop(last);
            assert!(weak.upgrade().is_none());
        } else {
            assert!(result.is_err());
            assert!(phase.finish().is_err());
            drop(phase);
        }
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn detached_loans_share_bodies_and_late_material_until_last_holder() -> Result<()> {
    for cold in [false, true] {
        let selected = fixture(9)?;
        let original = selected
            .records
            .iter()
            .map(Arc::downgrade)
            .collect::<Vec<_>>();
        let ledger = ledger()?;
        let (timers, triggers, material) = project_scheduling_records(
            selected,
            3,
            &ledger,
            64 * 1024 * 1024,
            65536,
            (0, 0),
            Vec::new(),
            cold,
        )?;
        let timer = timers[0].clone();
        let trigger = triggers[0].clone();
        let records = &material._selected.as_ref().unwrap().records;
        let CanonicalSchedulingFact::Timer(timer_body) = &records[timer.index].fact else {
            unreachable!()
        };
        let CanonicalSchedulingFact::ExternalTrigger(trigger_body) = &records[trigger.index].fact
        else {
            unreachable!()
        };
        assert!(std::ptr::eq(&*timer, timer_body));
        assert!(std::ptr::eq(&*trigger, trigger_body));
        let source = Arc::downgrade(&material.source);
        let acquisition = ResourceReadMaterial::current_or_new(&ledger, &scope())?;
        let scheduler = ResourceReadMaterial::current_or_new(&ledger, &scope())?;
        let queue = ResourceReadMaterial::current_or_new(&ledger, &scope())?;
        let weak = [
            Arc::downgrade(&acquisition.guard()?),
            Arc::downgrade(&scheduler.guard()?),
            Arc::downgrade(&queue.guard()?),
        ];
        let material = material
            .with_acquisition_material(acquisition.guard()?)?
            .with_scheduler_work_material(scheduler.guard()?)?
            .with_work_queue_material(queue.guard()?)?;
        let held = ledger.report()?.observed_bytes;
        drop((material, timers, triggers, acquisition, scheduler, queue));
        assert!(source.upgrade().is_some());
        assert!(original.iter().all(|row| row.upgrade().is_some()));
        assert!(weak.iter().all(|guard| guard.upgrade().is_some()));
        assert_eq!(ledger.report()?.observed_bytes, held);
        drop(timer);
        let last = trigger.clone();
        drop(trigger);
        assert!(weak.iter().all(|guard| guard.upgrade().is_some()));
        assert_eq!(ledger.report()?.observed_bytes, held);
        drop(last);
        assert!(source.upgrade().is_none());
        assert!(original.iter().all(|row| row.upgrade().is_none()));
        assert!(weak.iter().all(|guard| guard.upgrade().is_none()));
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn loan_trigger_dto_copy_has_exact_admission_and_no_private_fields() -> Result<()> {
    let ledger = ledger()?;
    let (_, triggers, material) = project(fixture(1)?, 0, &ledger)?;
    let loan = &triggers[0];
    let expected = ExternalTriggerStateSnapshot::from((**loan).clone());
    let fields = serde_json::to_value((
        &loan.external_trigger_id,
        &loan.target_agent_id,
        &loan.scope,
        &loan.delivery_mode,
        &loan.status,
        loan.delivery_count,
        loan.created_at,
        loan.revoked_at,
        loan.last_delivered_at,
    ))?;
    let cells = shape(&fields);
    let header = ((scope().agent_id.len() + 256) * 8) as u64;
    let bytes =
        header + (serde_json::to_vec(&fields)?.len() as u64 * 4 + cells as u64 * 128 + 512) * 8;
    let records = 64 + (cells + 16) * 8;
    let baseline = ledger.report()?.observed_bytes;
    for shortage in [None, Some(true), Some(false)] {
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope(),
            bytes - u64::from(shortage == Some(true)),
            records - usize::from(shortage == Some(false)),
        )?;
        let result = phase.read(|| loan.state_snapshot());
        if shortage.is_none() {
            let dto = result?;
            assert_eq!(dto, expected);
            let wire = serde_json::to_value(dto)?;
            assert!(wire.get("token").is_none());
            assert!(wire.get("token_hash").is_none());
            assert_eq!(phase.finish()?, (bytes, records));
        } else {
            assert!(result.is_err());
            assert!(phase.finish().is_err());
            let held = ledger.report()?.observed_bytes;
            assert!(phase.read(|| loan.state_snapshot()).is_err());
            assert_eq!(ledger.report()?.observed_bytes, held);
        }
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, baseline);
    }
    drop((triggers, material));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
