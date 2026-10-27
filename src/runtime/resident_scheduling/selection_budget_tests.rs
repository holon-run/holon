use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
    types::TimerStatus,
};

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "selection-budget".into(),
        identity_incarnation: 1,
    }
}

// Scalar JSON envelopes, not calls to admission/bootstrap/loader.
fn cost(bytes: usize, slots: usize) -> (u64, usize) {
    (
        (bytes as u64 * 4 + slots as u64 * 128 + 512) * 8,
        (slots + 16) * 8,
    )
}

fn key_cost(key: &SchedulingKey) -> Result<(u64, usize)> {
    // Four string starts, two braces, two colons, one comma.
    Ok(cost(serde_json::to_vec(key)?.len(), 9))
}

fn sum(parts: &[(u64, usize)]) -> (u64, usize) {
    parts.iter().fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1))
}

fn assert_gates<T: std::fmt::Debug>(
    parts: &[(u64, usize)],
    action: impl Fn() -> Result<T>,
) -> Result<T> {
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let limits = (16 * 1024 * 1024, 16384);
    for next in 0..parts.len() {
        let before = sum(&parts[..next]);
        for bytes_bound in [false, true] {
            let phase =
                ResourceReadMaterial::with_test_limits(&ledger, &scope(), limits.0, limits.1)?;
            let baseline = phase.finish()?;
            let weak = Arc::downgrade(&phase.guard()?);
            let error = {
                let _entry = phase.enter();
                if bytes_bound {
                    charge_owned_metadata(
                        (limits.0 - baseline.0 - before.0 - parts[next].0) / 8 + 1,
                        0,
                    )?;
                } else {
                    charge_owned_metadata(
                        0,
                        (limits.1 - baseline.1 - before.1 - parts[next].1) / 8 + 1,
                    )?;
                }
                action().unwrap_err()
            };
            assert!(
                error
                    .to_string()
                    .contains("ResourceExhausted: context resource read material"),
                "gate {next}, bytes={bytes_bound}: {error:#}"
            );
            assert!(phase.finish().is_err());
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            drop(phase);
            assert!(weak.upgrade().is_none());
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), limits.0, limits.1)?;
    let baseline = phase.finish()?;
    let value = {
        let _entry = phase.enter();
        action()?
    };
    let total = sum(parts);
    assert_eq!(
        phase.finish()?,
        (baseline.0 + total.0, baseline.1 + total.1)
    );
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(value)
}

fn coverage() -> SchedulingCoverage {
    let mut coverage = SchedulingCoverage::empty(1);
    for family in [
        SchedulingFamily::Timer,
        SchedulingFamily::TimerWake,
        SchedulingFamily::ExternalTrigger,
    ] {
        coverage.history.insert(
            family,
            SchedulingHistory {
                keys: vec![],
                loaded_keys: vec![],
                is_exhaustive: true,
            },
        );
    }
    coverage
}

#[test]
fn owning_roots_clone_each_actual_key_before_insertion() -> Result<()> {
    let mut coverage = coverage();
    let timer = SchedulingKey::new(SchedulingFamily::Timer, "timer\"雪");
    let wake = SchedulingKey::wake(&timer.id, "message\n");
    let trigger = SchedulingKey::new(SchedulingFamily::ExternalTrigger, "trigger\\");
    coverage.active_timers.insert(timer.clone());
    coverage.pending_wakes.insert(wake.clone());
    coverage.active_triggers.insert(trigger.clone());
    for key in [&timer, &wake, &trigger] {
        let history = coverage.history.get_mut(&key.family).unwrap();
        history.keys.push(key.clone());
        history.loaded_keys.push(key.clone());
    }
    let required = BTreeSet::from([timer.clone()]);
    let sequence = [
        &timer, &timer, &wake, &trigger, &timer, &timer, &wake, &wake, &trigger, &trigger,
    ];
    let addresses: Vec<_> = required
        .iter()
        .chain(&coverage.active_timers)
        .chain(&coverage.pending_wakes)
        .chain(&coverage.active_triggers)
        .chain(
            coverage
                .history
                .values()
                .flat_map(|history| history.keys.iter().chain(&history.loaded_keys)),
        )
        .collect();
    let visited = assert_gates(&[], || {
        let mut visited = 0;
        coverage.visit_roots(true, 1, &required, |key| {
            assert!(addresses
                .iter()
                .any(|original| std::ptr::eq(*original, key)));
            visited += 1;
            Ok(())
        })?;
        Ok(visited)
    })?;
    assert_eq!(visited, sequence.len());
    let costs: Vec<_> = sequence.into_iter().map(key_cost).collect::<Result<_>>()?;
    let roots = assert_gates(&costs, || coverage.roots(true, 1, &required))?;
    assert_eq!(
        roots,
        BTreeSet::from([timer.clone(), wake.clone(), trigger.clone()])
    );
    Ok(())
}

fn domain() -> Result<ResidentSchedulingDomain> {
    let now = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
    let timer = CanonicalSchedulingFact::Timer(TimerRecord {
        calendar: None,
        id: "parent\"雪".into(),
        agent_id: scope().agent_id,
        created_at: now,
        duration_ms: 1000,
        interval_ms: None,
        repeat: false,
        status: TimerStatus::Active,
        summary: None,
        next_fire_at: Some(now),
        last_fired_at: None,
        fire_count: 0,
    });
    let wake = CanonicalSchedulingFact::TimerWake(SchedulingWakeFact {
        agent_id: scope().agent_id,
        timer_id: "parent\"雪".into(),
        message_id: "message".into(),
        fire_count: 1,
        status: SchedulingWakeStatus::Pending,
        created_at: now,
        updated_at: now,
        incorporated_at: None,
        cancelled_at: None,
    });
    let mut coverage = coverage();
    let mut records = BTreeMap::new();
    for fact in [timer, wake] {
        let key = fact.key();
        coverage.observations.push(SchedulingObservation {
            key: key.clone(),
            availability: SchedulingAvailability::Present,
        });
        records.insert(
            key.clone(),
            Arc::new(QualifiedSchedulingFact {
                locator: SchedulingLocator {
                    scope: scope(),
                    key,
                    revision: 1,
                    content_digest: "a".repeat(64),
                    projection_digest: "b".repeat(64),
                },
                resident_bytes: serialized_bytes(&fact)? as usize,
                fact,
                _source_material: None,
            }),
        );
    }
    Ok(ResidentSchedulingDomain {
        scope: scope(),
        coverage,
        records,
        record_limit: 4096,
        byte_limit: 16 * 1024 * 1024,
        budget: None,
        views: vec![],
        context_material: None,
        replacement_material: None,
    })
}

#[test]
fn closure_clone_and_both_parent_ids_share_original_remainder() -> Result<()> {
    let domain = domain()?;
    let wake = domain
        .records
        .keys()
        .find(|k| k.family == SchedulingFamily::TimerWake)
        .unwrap();
    let roots = BTreeSet::from([wake.clone()]);
    // One key (9 slots) plus two brackets: 11 slots.
    let initial = cost(serde_json::to_vec(&roots)?.len(), 11);
    let parent_id = cost(serde_json::to_vec("parent\"雪")?.len(), 1);
    let selected = assert_gates(&[initial, parent_id, parent_id], || {
        domain.required_closure(roots.clone())
    })?;
    assert_eq!(selected.len(), 2);
    assert!(selected.contains(&SchedulingKey::new(SchedulingFamily::Timer, "parent\"雪")));
    Ok(())
}

#[test]
fn selection_lookup_error_key_is_admitted_before_owned_error() -> Result<()> {
    let mut domain = domain()?;
    let key = SchedulingKey::new(SchedulingFamily::Timer, "missing".repeat(2048));
    for availability in [
        None,
        Some(SchedulingAvailability::Present),
        Some(SchedulingAvailability::Pruned),
        Some(SchedulingAvailability::HistoricalUnavailable),
    ] {
        domain.coverage.observations.retain(|o| o.key != key);
        if let Some(availability) = availability {
            domain.coverage.observations.push(SchedulingObservation {
                key: key.clone(),
                availability,
            });
        }
        // Map a correctly formed lookup error to success so assert_gates can
        // also check the exact successful admission (not a relaxed lookup).
        assert_gates(&[key_cost(&key)?], || match domain.lookup(&key) {
            Err(error) if error.downcast_ref::<SchedulingReadError>().is_some() => Ok(()),
            Err(error) => Err(error),
            Ok(_) => anyhow::bail!("lookup unexpectedly succeeded"),
        })?;
    }
    Ok(())
}

#[test]
fn selection_policy_checks_still_precede_unneeded_clones() -> Result<()> {
    let mut coverage = coverage();
    coverage.active_timers_complete = false;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(
        &ledger,
        &scope(),
        (scope().agent_id.len() as u64 + 256) * 8,
        64,
    )?;
    let baseline = phase.finish()?;
    {
        let _entry = phase.enter();
        assert!(coverage
            .roots(true, SCHEDULING_HISTORY_WINDOW + 1, &BTreeSet::new())
            .unwrap_err()
            .to_string()
            .contains("exceeds policy"));
        assert!(matches!(
            coverage
                .roots(true, 0, &BTreeSet::new())
                .unwrap_err()
                .downcast_ref::<SchedulingReadError>(),
            Some(SchedulingReadError::IncompleteLive)
        ));
    }
    assert_eq!(phase.finish()?, baseline);
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

// Count the fixture's JSON tree, independently of the streaming admission sink.
fn capture_clone_cost(value: &impl Serialize) -> Result<(u64, usize)> {
    fn slots(value: &serde_json::Value) -> usize {
        match value {
            serde_json::Value::String(_) => 1,
            serde_json::Value::Array(values) => {
                2 + values.len().saturating_sub(1) + values.iter().map(slots).sum::<usize>()
            }
            serde_json::Value::Object(values) => {
                2 + 2 * values.len()
                    + values.len().saturating_sub(1)
                    + values.values().map(slots).sum::<usize>()
            }
            _ => 0,
        }
    }
    Ok(cost(
        serde_json::to_vec(value)?.len(),
        slots(&serde_json::to_value(value)?),
    ))
}

fn capture_vec_cost<T>(capacity: usize) -> (u64, usize) {
    (
        (128 + capacity as u64 * std::mem::size_of::<T>() as u64) * 8,
        (capacity + 1) * 8,
    )
}

#[test]
fn selected_install_moves_coverage_and_admits_only_body_arc_slots() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    for deny in [true, false] {
        let mut domain = domain()?;
        let timer = domain
            .records
            .keys()
            .find(|key| key.family == SchedulingFamily::Timer)
            .unwrap()
            .clone();
        domain
            .coverage
            .history
            .get_mut(&SchedulingFamily::Timer)
            .unwrap()
            .keys
            .push(timer);
        let original = domain.coverage.history[&SchedulingFamily::Timer]
            .keys
            .as_ptr();
        let count = domain.records.len();
        let expected = capture_vec_cost::<Arc<QualifiedSchedulingFact>>(count);
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), 1024 * 1024, 4096)?;
        let baseline = phase.finish()?;
        let _entry = phase.enter();
        if deny {
            charge_owned_metadata(0, (4096 - baseline.1 - expected.1) / 8 + 1)?;
            assert_eq!(
                domain.into_selected_install().unwrap_err().to_string(),
                "ResourceExhausted: context resource read material"
            );
            assert!(phase.finish().is_err());
        } else {
            let (coverage, records) = domain.into_selected_install()?;
            assert_eq!(
                coverage.history[&SchedulingFamily::Timer].keys.as_ptr(),
                original
            );
            assert_eq!(records.len(), count);
            assert_eq!(
                phase.finish()?,
                (baseline.0 + expected.0, baseline.1 + expected.1,)
            );
        }
    }
    Ok(())
}

fn empty_capture_parts(domain: &ResidentSchedulingDomain) -> Result<Vec<(u64, usize)>> {
    Ok(vec![
        cost(2, 2), // closure's initial empty Vec
        capture_vec_cost::<Arc<QualifiedSchedulingFact>>(0),
        capture_clone_cost(&domain.coverage)?,
        cost(2, 2), // required selector clone
        capture_vec_cost::<Arc<BudgetViewGuard>>(0),
        capture_clone_cost(&domain.scope)?,
    ])
}

fn empty_capture_request() -> SchedulingCaptureRequest {
    SchedulingCaptureRequest {
        include_live: false,
        history_limit: 0,
        required_keys: BTreeSet::new(),
    }
}

#[test]
fn capture_owning_projections_gate_each_copy_and_capacity_before_allocation() -> Result<()> {
    let domain = domain()?;
    let request = empty_capture_request();
    let parts = empty_capture_parts(&domain)?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let limits = (16 * 1024 * 1024, 16384);
    for next in 0..parts.len() {
        let before = sum(&parts[..next]);
        for bytes_bound in [false, true] {
            let phase =
                ResourceReadMaterial::with_test_limits(&ledger, &scope(), limits.0, limits.1)?;
            let baseline = phase.finish()?;
            let weak = Arc::downgrade(&phase.guard()?);
            let error = {
                let _entry = phase.enter();
                if bytes_bound {
                    charge_owned_metadata(
                        (limits.0 - baseline.0 - before.0 - parts[next].0) / 8 + 1,
                        0,
                    )?;
                } else {
                    charge_owned_metadata(
                        0,
                        (limits.1 - baseline.1 - before.1 - parts[next].1) / 8 + 1,
                    )?;
                }
                domain.capture(&request).unwrap_err()
            };
            assert!(
                error
                    .to_string()
                    .contains("ResourceExhausted: context resource read material"),
                "gate {next}, bytes={bytes_bound}: {error:#}"
            );
            assert!(phase.finish().is_err());
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            drop(phase);
            assert!(weak.upgrade().is_none());
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), limits.0, limits.1)?;
    let baseline = phase.finish()?;
    let captured = {
        let _entry = phase.enter();
        domain.capture(&request)?
    };
    let total = sum(&parts);
    assert_eq!(
        phase.finish()?,
        (baseline.0 + total.0, baseline.1 + total.1)
    );
    assert!(captured.records.is_empty());
    assert!(captured.coverage.observations.is_empty());
    assert!(!captured.coverage.active_timers_complete);
    let weak = Arc::downgrade(&phase.guard()?);
    drop(phase);
    assert!(weak.upgrade().is_some());
    drop(captured);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn capture_owning_projections_share_bodies_and_keep_source_and_capture_phases() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let source =
        ResourceReadMaterial::with_test_limits(&ledger, &scope(), 16 * 1024 * 1024, 16384)?;
    let source_weak = Arc::downgrade(&source.guard()?);
    let domain = domain()?.with_context_material(Some(source.guard()?));
    let wake = domain
        .records
        .keys()
        .find(|k| k.family == SchedulingFamily::TimerWake)
        .unwrap();
    let request = SchedulingCaptureRequest {
        required_keys: BTreeSet::from([wake.clone()]),
        ..empty_capture_request()
    };
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), 16 * 1024 * 1024, 16384)?;
    let capture_weak = Arc::downgrade(&phase.guard()?);
    let captured = Arc::new({
        let _entry = phase.enter();
        domain.capture(&request)?
    });
    assert_eq!(captured.records.len(), 2);
    assert_eq!(captured.coverage.required_keys, request.required_keys);
    for record in &captured.records {
        assert!(Arc::ptr_eq(record, &domain.records[&record.locator.key]));
    }
    assert!(Arc::ptr_eq(
        captured._capture_material.as_ref().unwrap(),
        &phase.guard()?,
    ));
    let last = captured.clone();
    drop((source, phase, domain, captured));
    assert!(source_weak.upgrade().is_some());
    assert!(capture_weak.upgrade().is_some());
    assert!(ledger.report()?.observed_bytes > 0);
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop(last);
    assert!(source_weak.upgrade().is_none());
    assert!(capture_weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn capture_owning_projections_cannot_reset_original_remainder() -> Result<()> {
    let domain = domain()?;
    let request = empty_capture_request();
    let total = sum(&empty_capture_parts(&domain)?);
    let header = ((scope().agent_id.len() as u64 + 256) * 8, 64);
    for bytes_bound in [false, true] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope(),
            if bytes_bound {
                header.0 + total.0
            } else {
                16 * 1024 * 1024
            },
            if bytes_bound {
                16384
            } else {
                header.1 + total.1
            },
        )?;
        let captured = {
            let _entry = phase.enter();
            let captured = domain.capture(&request)?;
            assert_eq!(phase.finish()?, (header.0 + total.0, header.1 + total.1));
            assert!(domain
                .capture(&request)
                .unwrap_err()
                .to_string()
                .contains("ResourceExhausted: context resource read material"));
            captured
        };
        let weak = Arc::downgrade(captured._capture_material.as_ref().unwrap());
        assert!(phase.finish().is_err());
        drop(phase);
        assert!(weak.upgrade().is_some());
        drop(captured);
        assert!(weak.upgrade().is_none());
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn capture_owning_projections_preserve_policy_lookup_and_no_phase_path() -> Result<()> {
    let domain = domain()?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), 16 * 1024 * 1024, 16384)?;
    let baseline = phase.finish()?;
    {
        let _entry = phase.enter();
        let request = SchedulingCaptureRequest {
            history_limit: SCHEDULING_HISTORY_WINDOW + 1,
            ..empty_capture_request()
        };
        assert!(domain
            .capture(&request)
            .unwrap_err()
            .to_string()
            .contains("exceeds policy"));
        assert_eq!(phase.finish()?, baseline);
        let missing = SchedulingKey::new(SchedulingFamily::Timer, "not-installed");
        let request = SchedulingCaptureRequest {
            required_keys: BTreeSet::from([missing.clone()]),
            ..empty_capture_request()
        };
        assert!(
            matches!(domain.capture(&request).unwrap_err().downcast_ref::<SchedulingReadError>(),
            Some(SchedulingReadError::NeedsPreparation(key)) if key == &missing)
        );
        let key = key_cost(&missing)?;
        assert_eq!(phase.finish()?, (baseline.0 + key.0, baseline.1 + key.1));
    }
    drop(phase);
    let captured = domain.capture(&empty_capture_request())?;
    assert!(captured._capture_material.is_none());
    assert!(captured._views.is_empty());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn validation_reference_nodes_charge_only_distinct_borrowed_cells() -> Result<()> {
    let keys = [
        SchedulingKey::new(SchedulingFamily::Timer, "small"),
        SchedulingKey::new(SchedulingFamily::Timer, "\"雪\\".repeat(32768)),
    ];
    // Independent fixed metadata cost, not the serialized size of either key.
    let node = (128 * 8, 8);
    let selected = assert_gates(&[node, node], || {
        let mut selected = BTreeSet::new();
        for key in &keys {
            assert!(insert_validation_reference(&mut selected, key)?);
            assert!(!insert_validation_reference(&mut selected, key)?);
        }
        Ok(selected)
    })?;
    for key in &keys {
        assert!(std::ptr::eq(*selected.get(key).unwrap(), key));
    }
    Ok(())
}

#[test]
fn validation_reference_nodes_share_the_successful_validation_phase() -> Result<()> {
    let domain = domain()?;
    let node = (128 * 8, 8);
    // Only the two distinct observation reference nodes are allocated.
    assert_gates(&[node, node], || domain.validate())?;
    Ok(())
}

#[test]
fn borrowed_empty_roots_and_validation_preserve_failed_phase_latch() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let mut domain =
        ResidentSchedulingDomain::bootstrap(scope(), coverage(), vec![], 4096, 16 * 1024 * 1024)?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), 16 * 1024 * 1024, 4096)?;
    let weak = Arc::downgrade(&phase.guard()?);
    {
        let _entry = phase.enter();
        assert!(charge_owned_metadata(0, 4096).is_err());
        for error in [
            domain
                .coverage
                .visit_roots(true, 0, &domain.coverage.required_keys, |_| {
                    panic!("empty roots must not invoke the visitor")
                })
                .unwrap_err(),
            domain.validate().unwrap_err(),
            domain.refresh_views().unwrap_err(),
        ] {
            let descriptor = crate::runtime_error::describe_runtime_error(&error);
            assert_eq!(
                descriptor.domain,
                crate::runtime_error::RuntimeErrorDomain::Io
            );
            assert_eq!(descriptor.code, "context_resource_exhausted");
            assert!(descriptor.retryable);
        }
    }
    assert!(phase.finish().is_err());
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    assert!(weak.upgrade().is_some());
    drop(phase);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn live_budget_classification_borrows_indexes_and_includes_wake_parent() -> Result<()> {
    let mut domain = domain()?;
    let timer = domain
        .records
        .keys()
        .find(|key| key.family == SchedulingFamily::Timer)
        .unwrap()
        .clone();
    let wake = domain
        .records
        .keys()
        .find(|key| key.family == SchedulingFamily::TimerWake)
        .unwrap()
        .clone();
    let trigger = SchedulingKey::new(SchedulingFamily::ExternalTrigger, &timer.id);
    let unrelated = SchedulingKey::new(SchedulingFamily::Timer, "historical");
    domain.coverage.pending_wakes.insert(wake.clone());
    domain.coverage.required_keys.insert(unrelated.clone());
    domain.coverage.observations.push(SchedulingObservation {
        key: unrelated.clone(),
        availability: SchedulingAvailability::Absent,
    });
    domain.validate()?;
    assert_gates(&[], || {
        assert!(domain.is_live_view_key(&wake));
        // The parent is Current even when it is not an active Timer root.
        assert!(!domain.coverage.active_timers.contains(&timer));
        assert!(domain.is_live_view_key(&timer));
        assert!(!domain.is_live_view_key(&trigger));
        assert!(!domain.is_live_view_key(&unrelated));
        Ok(())
    })?;
    domain.coverage.pending_wakes.clear();
    domain.coverage.active_timers.insert(timer.clone());
    domain.coverage.active_triggers.insert(trigger.clone());
    assert_gates(&[], || {
        assert!(domain.is_live_view_key(&timer));
        assert!(domain.is_live_view_key(&trigger));
        assert!(!domain.is_live_view_key(&wake));
        assert!(!domain.is_live_view_key(&unrelated));
        Ok(())
    })?;
    domain.coverage.active_timers.clear();
    domain.coverage.active_triggers.clear();
    assert_gates(&[], || {
        assert!(!domain.is_live_view_key(&timer));
        assert!(!domain.is_live_view_key(&wake));
        assert!(!domain.is_live_view_key(&trigger));
        Ok(())
    })?;
    Ok(())
}

#[test]
fn validation_reference_nodes_preserve_observation_error_order() -> Result<()> {
    let node = (128 * 8, 8);
    for duplicate in [false, true] {
        let mut domain = domain()?;
        let expected = if duplicate {
            let first = domain.coverage.observations[0].clone();
            domain.coverage.observations.insert(1, first);
            "duplicate Scheduling observation"
        } else {
            domain.coverage.observations[0].availability = SchedulingAvailability::Pruned;
            "Scheduling availability/body mismatch"
        };
        // The first reference is admitted; neither error allocates another node.
        assert_gates(&[node], || {
            let error = domain.validate().unwrap_err();
            if error.to_string() == expected {
                Ok(())
            } else {
                Err(error)
            }
        })?;
    }
    Ok(())
}

#[test]
fn validation_reference_nodes_preserve_wake_status_and_parent_deduplication() -> Result<()> {
    let node = (128 * 8, 8);
    for invalid_status in [false, true] {
        let mut domain = domain()?;
        let first_key = domain
            .records
            .keys()
            .find(|key| key.family == SchedulingFamily::TimerWake)
            .unwrap()
            .clone();
        domain.coverage.pending_wakes.insert(first_key.clone());
        if invalid_status {
            let record = domain.records.get_mut(&first_key).unwrap();
            let CanonicalSchedulingFact::TimerWake(wake) = &mut Arc::get_mut(record).unwrap().fact
            else {
                unreachable!()
            };
            wake.status = SchedulingWakeStatus::Cancelled;
        } else {
            let CanonicalSchedulingFact::TimerWake(wake) = &domain.records[&first_key].fact else {
                unreachable!()
            };
            let fact = CanonicalSchedulingFact::TimerWake(SchedulingWakeFact {
                message_id: "second-message".into(),
                ..wake.clone()
            });
            let key = fact.key();
            domain.coverage.pending_wakes.insert(key.clone());
            domain.coverage.observations.push(SchedulingObservation {
                key: key.clone(),
                availability: SchedulingAvailability::Present,
            });
            domain.records.insert(
                key.clone(),
                Arc::new(QualifiedSchedulingFact {
                    locator: SchedulingLocator {
                        scope: scope(),
                        key,
                        revision: 1,
                        content_digest: "a".repeat(64),
                        projection_digest: "b".repeat(64),
                    },
                    resident_bytes: serialized_bytes(&fact)? as usize,
                    fact,
                    _source_material: None,
                }),
            );
        }
        let mut parts = vec![node; domain.coverage.observations.len()];
        if !invalid_status {
            // Only the first pending wake inserts its borrowed parent ID.
            parts.push(node);
        }
        assert_gates(&parts, || {
            let error = domain.validate().unwrap_err();
            if error.to_string() == "complete live Scheduling index/body mismatch" {
                Ok(())
            } else {
                Err(error)
            }
        })?;
    }
    Ok(())
}
