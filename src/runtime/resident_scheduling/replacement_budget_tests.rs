use super::*;
use crate::{
    diagnostics::resource_io::{assert_retryable_resource_error, ResourceReadMaterial},
    runtime::{
        tests::support::{context_config, tempdir, StubProvider},
        RuntimeHandle,
    },
};

fn domain(ledger: &InputEvidenceBudgetLedger) -> Result<ResidentSchedulingDomain> {
    let scope = InputEvidenceScope {
        agent_id: "replacement-budget".into(),
        identity_incarnation: 1,
    };
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
    ResidentSchedulingDomain::bootstrap(scope, coverage, vec![], 4096, 16 * 1024 * 1024)?
        .with_budget(ledger.clone())
}

fn delta(domain: &ResidentSchedulingDomain) -> CommittedSchedulingDelta {
    CommittedSchedulingDelta {
        scope: domain.scope.clone(),
        base_revision: domain.coverage.revision,
        revision: domain.coverage.revision,
        coverage: domain.coverage.clone(),
        records: domain.records.values().cloned().collect(),
    }
}

// Independent JSON tree/scalar formula, not a successful replacement sample.
fn clone_cost(value: &impl Serialize) -> Result<(u64, usize)> {
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
    let slots = slots(&serde_json::to_value(value)?);
    Ok((
        (serde_json::to_vec(value)?.len() as u64 * 4 + slots as u64 * 128 + 512) * 8,
        (slots + 16) * 8,
    ))
}

fn sum(parts: &[(u64, usize)]) -> (u64, usize) {
    parts.iter().fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1))
}

#[test]
fn replacement_gates_each_projection_before_invalid_live_validation() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let original = domain(&ledger)?;
    let mut delta = delta(&original);
    delta.coverage.active_timers_complete = false;
    let header = ((original.scope.agent_id.len() as u64 + 256) * 8, 64);
    let parts = [
        header,       // Candidate owning-registration descriptor.
        (128 * 8, 8), // empty Arc vector
        clone_cost(&original.scope)?,
        clone_cost(&delta.coverage)?,
    ];
    let baseline = ledger.report()?.observed_bytes;
    for gate in 0..parts.len() {
        let upto = sum(&parts[..=gate]);
        for bytes_bound in [false, true] {
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &original.scope,
                if bytes_bound {
                    header.0 + upto.0 - 1
                } else {
                    16 * 1024 * 1024
                },
                if bytes_bound {
                    65536
                } else {
                    header.1 + upto.1 - 1
                },
            )?;
            let weak = Arc::downgrade(&phase.guard()?);
            {
                let _entry = phase.enter();
                for _ in 0..2 {
                    let error = original.with_committed(&delta).unwrap_err();
                    assert_retryable_resource_error(&error);
                }
            }
            assert!(phase.finish().is_err());
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            assert!(original.coverage.active_timers_complete);
            drop(phase);
            assert!(weak.upgrade().is_none());
            assert_eq!(ledger.report()?.observed_bytes, baseline);
        }
    }
    let phase = ResourceReadMaterial::new(&ledger, &original.scope)?;
    {
        let _entry = phase.enter();
        assert!(original
            .with_committed(&delta)
            .unwrap_err()
            .downcast_ref::<SchedulingReadError>()
            .is_some_and(|e| matches!(e, SchedulingReadError::IncompleteLive)));
    }
    assert_eq!(
        phase.finish()?,
        (header.0 + sum(&parts).0, header.1 + sum(&parts).1)
    );
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}

#[test]
fn replacement_reuses_exact_original_remainder_and_failed_phase() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let original = domain(&ledger)?;
    let delta = delta(&original);
    let header = ((original.scope.agent_id.len() as u64 + 256) * 8, 64);
    let cost = sum(&[
        header, // Candidate owning-registration descriptor.
        (128 * 8, 8),
        clone_cost(&original.scope)?,
        clone_cost(&delta.coverage)?,
        header, // Published metadata owning-registration descriptor.
    ]);
    let baseline = ledger.report()?.observed_bytes;
    for bytes_bound in [false, true] {
        for exact in [false, true] {
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &original.scope,
                if bytes_bound {
                    header.0 + 2 * cost.0 - u64::from(!exact)
                } else {
                    16 * 1024 * 1024
                },
                if bytes_bound {
                    65536
                } else {
                    header.1 + 2 * cost.1 - usize::from(!exact)
                },
            )?;
            let guard = phase.guard()?;
            let weak = Arc::downgrade(&guard);
            let first;
            {
                let _entry = phase.enter();
                first = original.with_committed(&delta)?;
                assert!(Arc::ptr_eq(
                    first.replacement_material.as_ref().unwrap(),
                    &guard
                ));
                assert_eq!(phase.finish()?, (header.0 + cost.0, header.1 + cost.1));
                let second = first.with_committed(&delta);
                if exact {
                    let second = second?;
                    assert!(Arc::ptr_eq(
                        second.replacement_material.as_ref().unwrap(),
                        &guard
                    ));
                    assert_eq!(
                        phase.finish()?,
                        (header.0 + 2 * cost.0, header.1 + 2 * cost.1)
                    );
                } else {
                    assert_retryable_resource_error(&second.unwrap_err());
                    assert_retryable_resource_error(&phase.finish().unwrap_err());
                    assert_retryable_resource_error(&original.with_committed(&delta).unwrap_err());
                }
            }
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            drop((guard, phase));
            assert!(weak.upgrade().is_some());
            drop(first);
            assert!(weak.upgrade().is_none());
            assert_eq!(ledger.report()?.observed_bytes, baseline);
            assert_eq!(original.coverage, delta.coverage);
        }
    }
    Ok(())
}

#[test]
fn replacement_namespace_and_revision_reject_before_new_phase() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let original = domain(&ledger)?;
    let baseline = ledger.report()?.observed_bytes;
    for invalid in 0..3 {
        let mut delta = delta(&original);
        match invalid {
            0 => delta.scope.identity_incarnation += 1,
            1 => delta.revision += 1,
            _ => delta.base_revision -= 1,
        }
        let error = original.with_committed(&delta).unwrap_err();
        if invalid == 2 {
            assert!(matches!(
                error.downcast_ref::<SchedulingReadError>(),
                Some(SchedulingReadError::RevisionMismatch)
            ));
        } else {
            assert!(error.to_string().contains("namespace/revision differs"));
        }
        assert_eq!(ledger.report()?.observed_bytes, baseline);
        assert!(ResourceReadMaterial::active_guard()?.is_none());
    }
    Ok(())
}

#[tokio::test]
async fn replacement_owner_publication_shares_arcs_and_retains_both_last_holders() -> Result<()> {
    let home = tempdir()?;
    let runtime = RuntimeHandle::new(
        "replacement-budget",
        home.path().into(),
        None::<crate::types::WorkspaceEntry>,
        String::new(),
        Arc::new(StubProvider::new("unused")),
        "replacement-budget".into(),
        context_config(),
    )?;
    runtime.join_resource_worker().await?;
    {
        let mut owner = runtime.begin_mutation().await?;
        owner
            .prepare_owner_scheduling_request(&SchedulingCaptureRequest {
                include_live: true,
                history_limit: 0,
                required_keys: BTreeSet::from([SchedulingKey::new(
                    SchedulingFamily::Timer,
                    "unseen-source",
                )]),
            })
            .await?;
    }
    let first_timer = runtime.schedule_timer(3_600_000, None, None).await?;
    runtime.join_resource_worker().await?;
    let first = runtime.inner.mutation_owner.capture()?.unwrap();
    let first_domain = first.scheduling.as_ref().unwrap();
    let old_replacement = Arc::downgrade(first_domain.replacement_material.as_ref().unwrap());
    let source = Arc::downgrade(first_domain.context_material.as_ref().unwrap());
    runtime.schedule_timer(7_200_000, None, None).await?;
    runtime.join_resource_worker().await?;
    let second = runtime.inner.mutation_owner.capture()?.unwrap();
    let second_domain = second.scheduling.as_ref().unwrap();
    let replacement = Arc::downgrade(second_domain.replacement_material.as_ref().unwrap());
    assert!(!old_replacement.ptr_eq(&replacement));
    assert!(source.ptr_eq(&Arc::downgrade(
        second_domain.context_material.as_ref().unwrap()
    )));
    let key = SchedulingKey::new(SchedulingFamily::Timer, &first_timer.id);
    assert!(Arc::ptr_eq(
        &first_domain.records[&key],
        &second_domain.records[&key]
    ));
    assert_ne!(first.token, second.token);
    let read = Arc::new(
        runtime
            .capture_scheduling_read(&SchedulingCaptureRequest::current())
            .await?,
    );
    assert_eq!(read.token, second.token);
    assert!(Arc::ptr_eq(&read.state, &second.state));
    assert_eq!(read.scheduling.timer(&first_timer.id)?, Some(&first_timer));
    for weak in [&source, &replacement] {
        assert!(read
            .scheduling
            ._views
            .iter()
            .any(|g| Arc::as_ptr(g) == weak.as_ptr()));
    }
    drop(first);
    assert!(old_replacement.upgrade().is_none());
    // Timer producers retain the runtime and its published source guard.
    // Cancel and join them without changing the committed Scheduling state.
    runtime.join_idle_owned_tasks().await?;
    let drained = runtime.inner.mutation_owner.capture()?.unwrap();
    assert_eq!(drained.token, second.token);
    assert!(Arc::ptr_eq(&drained.state, &second.state));
    drop(drained);
    runtime.inner.runtime_db.transaction(|tx| {
        tx.execute_batch("ALTER TABLE timers RENAME TO timers_forbidden; ALTER TABLE timer_wakes RENAME TO wakes_forbidden; ALTER TABLE external_triggers RENAME TO triggers_forbidden")?;
        Ok(())
    })?;
    let owner = runtime.begin_mutation().await?;
    let again = owner.capture_scheduling_read(&SchedulingCaptureRequest::current())?;
    assert_eq!(again.token, second.token);
    assert_eq!(again.scheduling.timer(&first_timer.id)?, Some(&first_timer));
    let last = read.clone();
    drop((again, read, owner, second, runtime));
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    assert!(source.upgrade().is_some());
    assert!(replacement.upgrade().is_some());
    drop(last);
    assert!(source.upgrade().is_none());
    assert!(replacement.upgrade().is_none());
    Ok(())
}
