use super::*;
use crate::diagnostics::resource_io::{
    assert_retryable_resource_error, charge_owned_metadata, ResourceReadMaterial,
};

#[path = "scheduling_output_loan_tests.rs"]
mod loan_tests;

// Independent tree-shape oracle, not the production streaming counter.
fn shape(value: &serde_json::Value) -> usize {
    use serde_json::Value;
    match value {
        Value::Object(fields) => {
            2 + fields.len().saturating_sub(1)
                + fields.values().map(|value| 2 + shape(value)).sum::<usize>()
        }
        Value::Array(values) => {
            2 + values.len().saturating_sub(1) + values.iter().map(shape).sum::<usize>()
        }
        Value::String(_) => 1,
        _ => 0,
    }
}

pub(super) fn body_cost(
    selected: &CapturedScheduling,
    limit: usize,
    cold: bool,
) -> Result<(u64, usize)> {
    if !cold {
        return Ok((0, 0));
    }
    let mut bytes = ((selected.scope.agent_id.len() + 256) * 8) as u64;
    let mut records = 64;
    let (timers, triggers) = expected(selected, limit, cold);
    bytes += (timers.len() * std::mem::size_of::<TimerRecordLoan>()
        + triggers.len() * std::mem::size_of::<ExternalTriggerRecordLoan>()
        + std::mem::size_of::<SchedulingReadSource>()
        + 128) as u64
        * 8;
    records += (timers.len() + triggers.len() + 1) * 8;
    Ok((bytes, records))
}

fn project(
    selected: CapturedScheduling,
    limit: usize,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<(
    Vec<TimerRecordLoan>,
    Vec<ExternalTriggerRecordLoan>,
    SchedulingReadMaterial,
)> {
    project_cold_scheduling_records(
        selected,
        limit,
        ledger,
        64 * 1024 * 1024,
        65536,
        (0, 0),
        Vec::new(),
    )
}

#[test]
fn cold_body_standalone_exact_cost_wire_and_original_last_holder() -> Result<()> {
    for count in [0, 9] {
        for limit in [0, 3, 40] {
            let selected = fixture(count)?;
            let expected = expected(&selected, limit, true);
            let body = body_cost(&selected, limit, true)?;
            let pins = output_cost(&selected, limit, true)?.0;
            let sources = selected
                .records
                .iter()
                .map(Arc::downgrade)
                .collect::<Vec<_>>();
            let ledger = ledger()?;
            let (timers, triggers, material) = project(selected, limit, &ledger)?;
            assert_eq!(timers, expected.0);
            assert_eq!(triggers, expected.1);
            assert_eq!(ledger.report()?.observed_bytes, pins + body.0);
            let original = Arc::downgrade(material._body.as_ref().context("cold body guard")?);
            let first = Arc::new((timers, triggers, material));
            let last = first.clone();
            drop(first);
            assert!(original.upgrade().is_some());
            assert!(sources.iter().all(|source| source.upgrade().is_some()));
            assert_eq!(
                serde_json::to_value((&last.0, &last.1))?,
                serde_json::to_value(expected)?
            );
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            drop(last);
            assert!(original.upgrade().is_none());
            assert!(sources.iter().all(|source| source.upgrade().is_none()));
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn cold_body_inherits_exact_remaining_bytes_records_and_failure_latch() -> Result<()> {
    for shortage in [None, Some(true), Some(false)] {
        let selected = fixture(9)?;
        let body = body_cost(&selected, 3, true)?;
        let ledger = ledger()?;
        // The output's ledger view is registered inside the caller's phase.
        let registration = ((scope().agent_id.len() + 256) as u64 * 8, 8 * 8);
        let usage = (
            body.0 + registration.0 + 13 * 8,
            body.1 + registration.1 + 2 * 8,
        );
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope(),
            usage.0 - u64::from(shortage == Some(true)),
            usage.1 - usize::from(shortage == Some(false)),
        )?;
        let original = phase.guard()?;
        let weak = Arc::downgrade(&original);
        let result = {
            let _entry = phase.enter();
            charge_owned_metadata(13, 2)?;
            project(selected, 3, &ledger)
        };
        if shortage.is_none() {
            let (timers, triggers, material) = result?;
            assert_eq!(phase.finish()?, usage);
            assert!(Arc::ptr_eq(material._body.as_ref().unwrap(), &original));
            let last = Arc::new((timers, triggers, material));
            drop((phase, original));
            assert!(weak.upgrade().is_some());
            drop(last);
        } else {
            let error = result.err().context("body must refuse shortage")?;
            assert_retryable_resource_error(&error);
            let latched = phase.finish().unwrap_err();
            assert_retryable_resource_error(&latched);
            let failed_bytes = ledger.report()?.observed_bytes;
            let retry_source = fixture(9)?;
            let retry = {
                let _entry = phase.enter();
                project(retry_source, 3, &ledger)
                    .err()
                    .context("retry must refuse")?
            };
            assert_retryable_resource_error(&retry);
            assert_eq!(ledger.report()?.observed_bytes, failed_bytes);
            drop((phase, original));
        }
        assert!(weak.upgrade().is_none());
        assert_eq!(ledger.report()?.observed_bytes, 0);
        assert!(ResourceReadMaterial::active_guard()?.is_none());
    }
    Ok(())
}

#[test]
fn cold_body_rejects_slots_before_growth_and_releases_partial_output() -> Result<()> {
    for slots_only in [true, false] {
        let selected = fixture(9)?;
        let sources = selected
            .records
            .iter()
            .map(Arc::downgrade)
            .collect::<Vec<_>>();
        let header = ((scope().agent_id.len() + 256) * 8) as u64;
        let registration = header;
        let slots = 3 * std::mem::size_of::<TimerRecordLoan>()
            + 9 * std::mem::size_of::<ExternalTriggerRecordLoan>()
            + std::mem::size_of::<SchedulingReadSource>()
            + 128;
        let bytes = if slots_only {
            header + registration + slots as u64 * 8 - 1
        } else {
            body_cost(&selected, 3, true)?.0 + registration - 1
        };
        let ledger = ledger()?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), bytes, 65536)?;
        let weak = Arc::downgrade(&phase.guard()?);
        let error = {
            let _entry = phase.enter();
            project(selected, 3, &ledger)
                .err()
                .context("must refuse before return")?
        };
        assert_retryable_resource_error(&error);
        assert_retryable_resource_error(&phase.finish().unwrap_err());
        if slots_only {
            assert_eq!(ledger.report()?.observed_bytes, header + registration);
        }
        assert!(sources.iter().all(|source| source.upgrade().is_none()));
        drop(phase);
        assert!(weak.upgrade().is_none());
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn cold_top_k_does_not_copy_discarded_large_timer_but_keeps_its_source() -> Result<()> {
    let mut selected = fixture(9)?;
    let expected = expected(&selected, 3, true);
    let omitted = selected
        .records
        .iter_mut()
        .find(|row| {
            matches!(&row.fact, CanonicalSchedulingFact::Timer(timer)
            if !expected.0.iter().any(|output| output.id == timer.id))
        })
        .context("unselected timer")?;
    let row = Arc::get_mut(omitted).context("unique timer source")?;
    let CanonicalSchedulingFact::Timer(timer) = &mut row.fact else {
        unreachable!()
    };
    timer.summary = Some("unselected payload 汉🦀".repeat(4096));
    row.resident_bytes = serde_json::to_vec(&row.fact)?.len();
    let omitted_source = Arc::downgrade(omitted);
    let mut body = body_cost(&selected, 3, true)?;
    body.0 += (scope().agent_id.len() + 256) as u64 * 8;
    body.1 += 8 * 8;
    let ledger = ledger()?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), body.0, body.1)?;
    let output = {
        let _entry = phase.enter();
        project(selected, 3, &ledger)?
    };
    assert_eq!(output.0, expected.0);
    assert_eq!(output.1, expected.1);
    assert_eq!(phase.finish()?, body);
    let guard = Arc::downgrade(&phase.guard()?);
    let first = Arc::new(output);
    let last = first.clone();
    drop((phase, first));
    assert!(omitted_source.upgrade().is_some());
    assert!(guard.upgrade().is_some());
    assert_eq!(
        serde_json::to_value((&last.0, &last.1))?,
        serde_json::to_value(expected)?
    );
    drop(last);
    assert!(omitted_source.upgrade().is_none());
    assert!(guard.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn cold_body_shares_large_history_text_and_admits_only_output_handles() -> Result<()> {
    for include_history in [false, true] {
        let mut selected = fixture(1)?;
        selected.coverage.active_timers.clear();
        selected.coverage.active_triggers.clear();
        for row in &mut selected.records {
            let row = Arc::get_mut(row).context("unique fixture body")?;
            match &mut row.fact {
                CanonicalSchedulingFact::Timer(timer) => {
                    timer.summary = Some("{[,:\\\" 汉🦀".repeat(2000));
                }
                CanonicalSchedulingFact::ExternalTrigger(trigger) => {
                    trigger.token = Some("{[,:\\\" 汉🦀".repeat(2000));
                    trigger.token_hash = "hash 汉🦀".repeat(2000);
                }
                _ => unreachable!(),
            }
            if include_history {
                selected
                    .coverage
                    .history
                    .get_mut(&row.locator.key.family)
                    .unwrap()
                    .keys
                    .push(row.locator.key.clone());
            }
        }
        let mut body = body_cost(&selected, 40, true)?;
        body.0 += (scope().agent_id.len() + 256) as u64 * 8;
        body.1 += 8 * 8;
        let ledger = ledger()?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), body.0, body.1)?;
        let (timers, triggers, material) = {
            let _entry = phase.enter();
            project(selected, 40, &ledger)?
        };
        assert_eq!(
            (timers.len(), triggers.len()),
            if include_history { (1, 1) } else { (0, 0) }
        );
        assert_eq!(phase.finish()?, body);
        if include_history {
            assert!(timers[0].summary.as_ref().unwrap().contains("汉🦀"));
            assert!(triggers[0].token.as_ref().unwrap().contains("汉🦀"));
        }
        drop((timers, triggers, material, phase));
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
