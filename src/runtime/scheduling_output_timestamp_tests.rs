use super::*;
use crate::runtime_db::repositories::{timer_updated_at, timestamp};
use chrono::{NaiveDate, TimeZone};

fn calendar_edges() -> Vec<DateTime<Utc>> {
    let mut values = vec![DateTime::<Utc>::MIN_UTC, DateTime::<Utc>::MAX_UTC];
    for year in [
        -262143, -100000, -99999, -10000, -9999, -1000, -999, -1, 0, 1, 999, 1000, 9999, 10000,
        99999, 100000, 262142,
    ] {
        for (month, day) in [(1, 1), (12, 31)] {
            values.push(
                NaiveDate::from_ymd_opt(year, month, day)
                    .unwrap()
                    .and_hms_nano_opt(23, 59, 59, 999_999_999)
                    .unwrap()
                    .and_utc(),
            );
        }
    }
    for seconds in [-1, 0, 1, 1_483_228_799] {
        for nanos in [0, 1, 999_999, 1_000_000, 123_456_789, 999_999_999] {
            values.push(DateTime::from_timestamp(seconds, nanos).unwrap());
        }
    }
    for nanos in [1_000_000_000, 1_000_999_999, 1_999_999_999] {
        values.push(DateTime::from_timestamp(1_483_228_799, nanos).unwrap());
    }
    values
}

#[test]
fn stack_timestamp_matches_original_bytes_at_calendar_and_precision_edges() {
    let mut values = calendar_edges();
    // Deterministic dates between the extremes, not just four-digit years.
    for index in 0..1024 {
        let seconds = -8_000_000_000_000 + index * 15_625_000_000;
        let nanos = (index as u32).wrapping_mul(982_451_653) % 1_000_000_000;
        values.push(DateTime::from_timestamp(seconds, nanos).unwrap());
    }
    for value in values {
        let key = ColdTimestamp::new(value);
        assert_eq!(key.as_bytes(), timestamp(value).as_bytes(), "{value:?}");
        assert!(key.len <= 27);
    }
    for value in [DateTime::<Utc>::MIN_UTC, DateTime::<Utc>::MAX_UTC] {
        assert_eq!(ColdTimestamp::new(value).len, 27);
    }
    // The fixed writer rejects overflow without modifying its existing bytes.
    let mut key = ColdTimestamp::new(DateTime::<Utc>::MAX_UTC);
    let before = key.bytes;
    assert!(std::fmt::Write::write_str(&mut key, "x").is_err());
    assert_eq!(key.bytes, before);
    assert_eq!(key.len, 27);
}

#[test]
fn stack_comparison_preserves_text_order_millisecond_ties_and_leap_seconds() {
    let values = calendar_edges();
    for a in &values {
        for b in &values {
            assert_eq!(
                compare_cold_timestamps(*a, *b),
                timestamp(*a).cmp(&timestamp(*b)),
                "{a:?} / {b:?}"
            );
        }
    }
    let early = DateTime::from_timestamp(-1, 1).unwrap();
    let late = DateTime::from_timestamp(-1, 999_999).unwrap();
    assert!(early < late);
    assert_eq!(
        compare_cold_timestamps(early, late),
        std::cmp::Ordering::Equal
    );
    let four_digit = Utc.with_ymd_and_hms(9999, 12, 31, 23, 59, 59).unwrap();
    let expanded = Utc.with_ymd_and_hms(10000, 1, 1, 0, 0, 0).unwrap();
    assert!(four_digit < expanded);
    assert_eq!(
        compare_cold_timestamps(four_digit, expanded),
        std::cmp::Ordering::Greater
    );
    let leap = DateTime::from_timestamp(1_483_228_799, 1_123_456_789).unwrap();
    assert_eq!(
        ColdTimestamp::new(leap).as_bytes(),
        b"2016-12-31T23:59:60.123Z"
    );
}

fn timestamp_fixture(variant: usize) -> Result<CapturedScheduling> {
    let times = if variant == 0 {
        (0..9)
            .map(|index| DateTime::from_timestamp(-1, index * 100).unwrap())
            .collect::<Vec<_>>()
    } else {
        calendar_edges()
    };
    let mut selected = fixture(9)?;
    for (index, record) in selected.records.iter_mut().enumerate() {
        let row = Arc::get_mut(record).unwrap();
        let created = times[(index * 7 + variant) % times.len()];
        let updated = times[(index * 11 + variant) % times.len()];
        match &mut row.fact {
            CanonicalSchedulingFact::Timer(timer) => {
                timer.created_at = created;
                // Exercise the unchanged last_fired / next_fire / created
                // precedence, including last_fired winning over next_fire.
                timer.last_fired_at = (index % 3 == 0).then_some(updated);
                timer.next_fire_at = (index % 3 != 2).then_some(created);
                let expected = if index % 3 == 0 { updated } else { created };
                assert_eq!(timer_updated_at(timer), expected);
            }
            CanonicalSchedulingFact::ExternalTrigger(trigger) => {
                trigger.created_at = created;
            }
            _ => unreachable!(),
        }
        row.resident_bytes = serde_json::to_vec(&row.fact)?.len();
    }
    Ok(selected)
}

#[test]
fn timestamp_projection_keeps_original_orders_limits_cost_and_last_holders() -> Result<()> {
    for cold in [false, true] {
        for variant in 0..4 {
            for limit in [0, 1, 3, 9] {
                let selected = timestamp_fixture(variant)?;
                let expected = expected(&selected, limit, cold);
                let cost = output_cost(&selected, limit, cold)?;
                let body = body_tests::body_cost(&selected, limit, cold)?.0;
                let sources = selected
                    .records
                    .iter()
                    .map(Arc::downgrade)
                    .collect::<Vec<_>>();
                let selected_ptr = selected.records.as_ptr();
                let ledger = ledger()?;
                let read = ledger.register_owned_bytes(
                    &scope(),
                    InputMemoryCategory::Pins,
                    OwnedBytesEstimate {
                        payload_bytes: 37,
                        metadata_estimate_bytes: 0,
                    },
                )?;
                let read_weak = Arc::downgrade(&read);
                let (timers, triggers, material) = project_scheduling_records(
                    selected,
                    limit,
                    &ledger,
                    64 * 1024 * 1024,
                    65536,
                    (37, 3),
                    vec![read.clone()],
                    cold,
                )?;
                assert_eq!(timers, expected.0);
                assert_eq!(triggers, expected.1);
                assert_eq!(ledger.report()?.observed_bytes, 37 + cost.0 + body);
                assert_eq!(
                    material._selected.as_ref().unwrap().records.as_ptr(),
                    selected_ptr
                );
                assert!(Arc::ptr_eq(&material._read[0], &read));
                let output = Arc::downgrade(&material._owned);
                let first = Arc::new((timers, triggers, material));
                let last = first.clone();
                drop((read, first));
                let wire = serde_json::to_value((&last.0, &last.1))?;
                assert_eq!(wire, serde_json::to_value((&expected.0, &expected.1))?);
                assert!(sources.iter().all(|source| source.upgrade().is_some()));
                assert!(read_weak.upgrade().is_some());
                assert!(output.upgrade().is_some());
                assert_eq!(ledger.report()?.observed_bytes, 37 + cost.0 + body);
                drop((wire, last));
                assert!(sources.iter().all(|source| source.upgrade().is_none()));
                assert!(read_weak.upgrade().is_none());
                assert!(output.upgrade().is_none());
                assert_eq!(ledger.report()?.observed_bytes, 0);
            }
        }
    }
    Ok(())
}

#[test]
fn timestamp_projection_keeps_both_combined_refusal_boundaries() -> Result<()> {
    for axis in [None, Some(true), Some(false)] {
        let selected = timestamp_fixture(1)?;
        let cost = output_cost(&selected, 3, true)?;
        let body = body_tests::body_cost(&selected, 3, true)?.0;
        let retained = selected.selected_material_estimate()?;
        let ledger = ledger()?;
        let sources = selected
            .records
            .iter()
            .map(Arc::downgrade)
            .collect::<Vec<_>>();
        let result = project_cold_scheduling_records(
            selected,
            3,
            &ledger,
            usize::try_from(97 + retained.0 + cost.0)? - usize::from(axis == Some(true)),
            7 + retained.1 + cost.1 - usize::from(axis == Some(false)),
            (97, 7),
            Vec::new(),
        );
        if axis.is_none() {
            let (_, _, material) = result?;
            assert_eq!(ledger.report()?.observed_bytes, cost.0 + body);
            drop(material);
        } else {
            assert!(matches!(
                result
                    .err()
                    .context("combined boundary must refuse")?
                    .downcast_ref::<SchedulingReadError>(),
                Some(SchedulingReadError::ResourceExhausted(
                    "cold read/selected Scheduling/output combined material"
                ))
            ));
        }
        assert!(sources.iter().all(|source| source.upgrade().is_none()));
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
