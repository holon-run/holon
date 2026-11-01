use super::*;

#[path = "scheduling_output_body_tests.rs"]
mod body_tests;
#[path = "scheduling_output_timestamp_tests.rs"]
mod timestamp_tests;

use crate::{
    config::StateMemoryLimits,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
        resident_scheduling::{
            QualifiedSchedulingFact, ResidentSchedulingDomain, SchedulingCoverage,
            SchedulingHistory, SchedulingLocator,
        },
    },
};

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "output-material".into(),
        identity_incarnation: 1,
    }
}

fn fixture(count: usize) -> Result<CapturedScheduling> {
    let mut coverage = SchedulingCoverage::empty(1);
    for family in [
        SchedulingFamily::Timer,
        SchedulingFamily::TimerWake,
        SchedulingFamily::ExternalTrigger,
    ] {
        coverage.history.insert(
            family,
            SchedulingHistory {
                keys: Vec::new(),
                loaded_keys: Vec::new(),
                is_exhaustive: true,
            },
        );
    }
    let domain = ResidentSchedulingDomain::bootstrap(
        scope(),
        coverage,
        Vec::new(),
        65536,
        64 * 1024 * 1024,
    )?;
    let mut selected = domain.capture(&SchedulingCaptureRequest::current())?;
    // Pure output fixture; these DTOs do not grant production qualification.
    let ids = ["a", "A", "a\0", "汉", "🦀", "z", "é", "beta", "t"];
    for (index, id) in ids.into_iter().take(count).enumerate().rev() {
        let created = chrono::DateTime::from_timestamp(1000, (index % 3) as u32 * 100).unwrap();
        let timer = TimerRecord {
            calendar: None,
            id: id.into(),
            agent_id: scope().agent_id,
            created_at: created,
            duration_ms: 3600000,
            interval_ms: None,
            repeat: false,
            status: TimerStatus::Active,
            summary: Some("汉🦀".repeat(index + 1)),
            next_fire_at: None,
            last_fired_at: Some(created),
            fire_count: 1,
        };
        let trigger = ExternalTriggerRecord {
            external_trigger_id: id.into(),
            target_agent_id: scope().agent_id,
            scope: ExternalTriggerScope::Agent,
            delivery_mode: CallbackDeliveryMode::WakeHint,
            token: Some("fixture-only".into()),
            token_hash: "fixture-only".into(),
            status: ExternalTriggerStatus::Active,
            created_at: created,
            revoked_at: None,
            last_delivered_at: Some(created),
            delivery_count: 1,
        };
        for (family, fact) in [
            (
                SchedulingFamily::Timer,
                CanonicalSchedulingFact::Timer(timer),
            ),
            (
                SchedulingFamily::ExternalTrigger,
                CanonicalSchedulingFact::ExternalTrigger(trigger),
            ),
        ] {
            let key = SchedulingKey::new(family, id);
            match family {
                SchedulingFamily::Timer => {
                    selected.coverage.active_timers.insert(key.clone());
                }
                SchedulingFamily::ExternalTrigger => {
                    selected.coverage.active_triggers.insert(key.clone());
                }
                _ => unreachable!(),
            }
            selected.records.push(Arc::new(QualifiedSchedulingFact {
                locator: SchedulingLocator {
                    scope: scope(),
                    key,
                    revision: 1,
                    content_digest: "a".repeat(64),
                    projection_digest: "b".repeat(64),
                },
                resident_bytes: serde_json::to_vec(&fact)?.len(),
                fact,
                _source_material: None,
            }));
        }
    }
    Ok(selected)
}

fn ledger() -> Result<InputEvidenceBudgetLedger> {
    Ok(InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?)
}

fn output_cost(
    selected: &CapturedScheduling,
    timer_limit: usize,
    cold: bool,
) -> Result<(u64, usize)> {
    // Independent structured output formula, not the production cost builder.
    let (timers, triggers) = expected(selected, timer_limit, cold);
    let bodies = timers
        .iter()
        .map(serde_json::to_vec)
        .chain(triggers.iter().map(serde_json::to_vec))
        .try_fold(0u64, |bytes, body| {
            Ok::<_, anyhow::Error>(bytes + body?.len() as u64)
        })?;
    let slots = timers.len() * std::mem::size_of::<TimerRecord>()
        + triggers.len() * std::mem::size_of::<ExternalTriggerRecord>();
    let count = timers.len() + triggers.len();
    Ok(((512 + bodies + slots as u64) * 4, (count + 1) * 4))
}

fn expected(
    selected: &CapturedScheduling,
    limit: usize,
    cold: bool,
) -> (Vec<TimerRecord>, Vec<ExternalTriggerRecord>) {
    let mut timers = Vec::new();
    let mut triggers = Vec::new();
    for row in &selected.records {
        let historical = |family| {
            selected
                .coverage
                .history
                .get(&family)
                .is_some_and(|history| {
                    let keys = if cold {
                        &history.keys
                    } else {
                        &history.loaded_keys
                    };
                    keys.contains(&row.locator.key)
                })
        };
        match &row.fact {
            CanonicalSchedulingFact::Timer(timer)
                if limit > 0
                    && (selected.coverage.active_timers.contains(&row.locator.key)
                        || historical(SchedulingFamily::Timer)) =>
            {
                timers.push(timer.clone())
            }
            CanonicalSchedulingFact::ExternalTrigger(trigger)
                if selected.coverage.active_triggers.contains(&row.locator.key)
                    || historical(SchedulingFamily::ExternalTrigger) =>
            {
                triggers.push(trigger.clone())
            }
            _ => {}
        }
    }
    // Original stable comparators, including normalized SQL text on cold reads.
    if cold {
        use crate::runtime_db::repositories::{timer_updated_at, timestamp};
        timers.sort_by(|a, b| {
            timestamp(timer_updated_at(b))
                .cmp(&timestamp(timer_updated_at(a)))
                .then_with(|| timestamp(b.created_at).cmp(&timestamp(a.created_at)))
                .then_with(|| a.id.cmp(&b.id))
        });
        triggers.sort_by(|a, b| {
            timestamp(b.created_at)
                .cmp(&timestamp(a.created_at))
                .then_with(|| a.external_trigger_id.cmp(&b.external_trigger_id))
        });
    } else {
        timers.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        triggers.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| a.external_trigger_id.cmp(&b.external_trigger_id))
        });
    }
    timers.truncate(limit);
    if cold {
        timers.reverse();
    }
    (timers, triggers)
}

#[test]
fn typed_output_slots_keep_exact_capacity_order_cost_and_last_original_holders() -> Result<()> {
    for cold in [false, true] {
        let selected = fixture(9)?;
        let expected = expected(&selected, 3, cold);
        let cost = output_cost(&selected, 3, cold)?;
        let body = body_tests::body_cost(&selected, 3, cold)?.0;
        let sources = selected
            .records
            .iter()
            .map(Arc::downgrade)
            .collect::<Vec<_>>();
        let ledger = ledger()?;
        let read = ledger.register_owned_bytes(
            &scope(),
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 37,
                metadata_estimate_bytes: 0,
            },
        )?;
        let original_read = Arc::downgrade(&read);
        let selected_ptr = selected.records.as_ptr();
        let (timers, triggers, material) = project_scheduling_records(
            selected,
            3,
            &ledger,
            64 * 1024 * 1024,
            65536,
            (37, 3),
            vec![read.clone()],
            cold,
        )?;
        assert_eq!(timers, expected.0);
        assert_eq!(triggers, expected.1);
        assert_eq!((timers.capacity(), triggers.capacity()), (3, 9));
        assert_eq!(
            material._selected.as_ref().unwrap().records.as_ptr(),
            selected_ptr
        );
        assert!(Arc::ptr_eq(&material._read[0], &read));
        assert_eq!(ledger.report()?.observed_bytes, 37 + cost.0 + body);
        let output = Arc::downgrade(&material._owned);
        let first = Arc::new((timers, triggers, material));
        let last = first.clone();
        drop((first, read));
        assert!(sources.iter().all(|source| source.upgrade().is_some()));
        assert!(output.upgrade().is_some());
        assert!(original_read.upgrade().is_some());
        let wire = serde_json::to_value((&last.0, &last.1))?;
        assert_eq!(wire[0].as_array().unwrap().len(), 3);
        assert_eq!(wire[1].as_array().unwrap().len(), 9);
        assert_eq!(ledger.report()?.observed_bytes, 37 + cost.0 + body);
        drop((wire, last));
        assert!(sources.iter().all(|source| source.upgrade().is_none()));
        assert!(output.upgrade().is_none());
        assert!(original_read.upgrade().is_none());
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn typed_output_rejects_both_combined_boundaries_before_returning_material() -> Result<()> {
    for cold in [false, true] {
        for axis in [None, Some(true), Some(false)] {
            let selected = fixture(9)?;
            let cost = output_cost(&selected, 3, cold)?;
            let body = body_tests::body_cost(&selected, 3, cold)?.0;
            let retained = selected.selected_material_estimate()?;
            let bytes = 97 + retained.0 + cost.0;
            let records = 7 + retained.1 + cost.1;
            let ledger = ledger()?;
            let sources = selected
                .records
                .iter()
                .map(Arc::downgrade)
                .collect::<Vec<_>>();
            let result = project_scheduling_records(
                selected,
                3,
                &ledger,
                usize::try_from(bytes)? - usize::from(axis == Some(true)),
                records - usize::from(axis == Some(false)),
                (97, 7),
                Vec::new(),
                cold,
            );
            if axis.is_none() {
                let (_, _, material) = result?;
                assert_eq!(ledger.report()?.observed_bytes, cost.0 + body);
                drop(material);
            } else {
                let error = result.err().context("combined boundary must refuse")?;
                assert!(matches!(
                    error.downcast_ref::<SchedulingReadError>(),
                    Some(SchedulingReadError::ResourceExhausted(
                        "cold read/selected Scheduling/output combined material"
                    ))
                ));
            }
            assert!(sources.iter().all(|source| source.upgrade().is_none()));
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn typed_output_capacity_preserves_empty_zero_limit_and_selected_members() -> Result<()> {
    for cold in [false, true] {
        for count in [0, 9] {
            let mut selected = fixture(count)?;
            selected
                .coverage
                .active_triggers
                .retain(|key| key.id.as_str() < "z");
            let expected = expected(&selected, 0, cold);
            let cost = output_cost(&selected, 0, cold)?;
            let body = body_tests::body_cost(&selected, 0, cold)?.0;
            let ledger = ledger()?;
            let (timers, triggers, material) = project_scheduling_records(
                selected,
                0,
                &ledger,
                64 * 1024 * 1024,
                65536,
                (0, 0),
                Vec::new(),
                cold,
            )?;
            assert_eq!(
                (timers.capacity(), triggers.capacity()),
                (expected.0.len(), expected.1.len())
            );
            assert_eq!(timers, expected.0);
            assert_eq!(triggers, expected.1);
            drop((timers, triggers));
            assert_eq!(ledger.report()?.observed_bytes, cost.0 + body);
            drop(material);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        let selected = fixture(9)?;
        let ledger = ledger()?;
        let error = project_scheduling_records(
            selected,
            super::super::resident_scheduling::SCHEDULING_HISTORY_WINDOW + 1,
            &ledger,
            64 * 1024 * 1024,
            65536,
            (0, 0),
            Vec::new(),
            cold,
        )
        .err()
        .context("history policy must refuse")?;
        assert!(matches!(
            error.downcast_ref::<SchedulingReadError>(),
            Some(SchedulingReadError::NeedsHistory)
        ));
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn timer_top_k_borrows_beyond_full_window_with_no_material_allowance() -> Result<()> {
    use crate::diagnostics::resource_io::ResourceReadMaterial;
    let mut selected = fixture(9)?;
    let timer = selected
        .records
        .iter()
        .find_map(|row| match &row.fact {
            CanonicalSchedulingFact::Timer(timer) => Some(timer.clone()),
            _ => None,
        })
        .context("fixture timer")?;
    for index in (0..257).rev() {
        let mut timer = timer.clone();
        timer.id = format!("top-{index:03}");
        timer.created_at = DateTime::from_timestamp(1000, (index % 7) * 100).unwrap();
        timer.last_fired_at = Some(DateTime::from_timestamp(1000 + (index % 3) as i64, 0).unwrap());
        let key = SchedulingKey::new(SchedulingFamily::Timer, &timer.id);
        selected.coverage.active_timers.insert(key.clone());
        let fact = CanonicalSchedulingFact::Timer(timer);
        selected.records.push(Arc::new(QualifiedSchedulingFact {
            locator: SchedulingLocator {
                scope: scope(),
                key,
                revision: 1,
                content_digest: "a".repeat(64),
                projection_digest: "b".repeat(64),
            },
            resident_bytes: serde_json::to_vec(&fact)?.len(),
            fact,
            _source_material: None,
        }));
    }
    let ledger = ledger()?;
    for cold in [false, true] {
        for limit in [0, 1, 3, 127, 128] {
            let mut expected = expected(&selected, limit, cold).0;
            if cold {
                expected.reverse();
            }
            let header = ((scope().agent_id.len() + 256) * 8) as u64;
            let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), header, 64)?;
            {
                let _entry = phase.enter();
                let timers = select_timer_output(&selected, limit, cold);
                assert_eq!(timers.iter().flatten().count(), expected.len());
                for (index, expected) in timers.iter().flatten().zip(&expected) {
                    let actual = timer_at(&selected, *index);
                    assert_eq!(actual, expected);
                    assert!(selected.records.iter().any(|row| {
                        matches!(&row.fact, CanonicalSchedulingFact::Timer(timer)
                            if std::ptr::eq(timer, actual))
                    }));
                }
            }
            assert_eq!(phase.finish()?, (header, 64));
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn concurrent_output_vectors_keep_independent_original_pins_until_each_last_holder() -> Result<()> {
    let ledger = ledger()?;
    let first_source = fixture(9)?;
    let cost = output_cost(&first_source, 3, true)?;
    let second_cost = output_cost(&first_source, 3, false)?;
    let body = body_tests::body_cost(&first_source, 3, true)?.0;
    let first = project_scheduling_records(
        first_source,
        3,
        &ledger,
        64 * 1024 * 1024,
        65536,
        (0, 0),
        Vec::new(),
        true,
    )?;
    let second = project_scheduling_records(
        fixture(9)?,
        3,
        &ledger,
        64 * 1024 * 1024,
        65536,
        (0, 0),
        Vec::new(),
        false,
    )?;
    assert_eq!(
        ledger.report()?.observed_bytes,
        cost.0 + second_cost.0 + body
    );
    let first_guard = Arc::downgrade(&first.2._owned);
    let second_guard = Arc::downgrade(&second.2._owned);
    drop(first);
    assert!(first_guard.upgrade().is_none());
    assert!(second_guard.upgrade().is_some());
    assert_eq!(ledger.report()?.observed_bytes, second_cost.0);
    drop(second);
    assert!(second_guard.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
