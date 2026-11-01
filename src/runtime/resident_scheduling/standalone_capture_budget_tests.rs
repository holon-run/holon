use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::{
        tests::support::{context_config, tempdir, StubProvider},
        RuntimeHandle,
    },
};

fn runtime(home: &std::path::Path) -> Result<RuntimeHandle> {
    RuntimeHandle::new(
        "capture-budget",
        home.into(),
        None::<crate::types::WorkspaceEntry>,
        String::new(),
        Arc::new(StubProvider::new("unused")),
        "capture-budget".into(),
        context_config(),
    )
}

// Independent JSON tree/scalar formula; never sample a successful capture.
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

fn empty_capture_cost(domain: &ResidentSchedulingDomain) -> Result<(u64, usize)> {
    let views = usize::from(domain.context_material.is_some())
        + usize::from(domain.replacement_material.is_some())
        + 2 * usize::from(domain.budget.is_some());
    let parts = [
        ((domain.scope.agent_id.len() as u64 + 256) * 8, 64),
        clone_cost(&Vec::<SchedulingKey>::new())?, // closure worklist
        (128 * 8, 8),                              // empty records Vec
        clone_cost(&domain.coverage)?,
        clone_cost(&BTreeSet::<SchedulingKey>::new())?,
        (
            (128 + views as u64 * std::mem::size_of::<Arc<BudgetViewGuard>>() as u64) * 8,
            (views + 1) * 8,
        ),
        clone_cost(&domain.scope)?,
    ];
    Ok(parts.iter().fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1)))
}

#[tokio::test]
async fn standalone_capture_has_its_own_phase_and_last_holder_without_sql() -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let timer = runtime.schedule_timer(3_600_000, None, None).await?;
    runtime.join_resource_worker().await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let read = Arc::new(
        runtime
            .capture_scheduling_read(&SchedulingCaptureRequest::current())
            .await?,
    );
    assert_eq!(read.token, root.token);
    assert!(Arc::ptr_eq(&read.state, &root.state));
    assert_eq!(read.scheduling.timer(&timer.id)?, Some(&timer));
    for record in &read.scheduling.records {
        assert!(Arc::ptr_eq(
            record,
            &root.scheduling.as_ref().unwrap().records[&record.locator.key]
        ));
    }
    let weak = Arc::downgrade(read.scheduling._capture_material.as_ref().unwrap());
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    runtime.inner.runtime_db.transaction(|tx| {
        tx.execute_batch("ALTER TABLE timers RENAME TO timers_forbidden; ALTER TABLE timer_wakes RENAME TO wakes_forbidden; ALTER TABLE external_triggers RENAME TO triggers_forbidden")?;
        Ok(())
    })?;
    let owner = runtime.begin_mutation().await?;
    let next = owner.capture_scheduling_read(&SchedulingCaptureRequest::current())?;
    assert_eq!(next.token, root.token);
    assert_eq!(next.scheduling.timer(&timer.id)?, Some(&timer));
    assert!(!Arc::ptr_eq(
        read.scheduling._capture_material.as_ref().unwrap(),
        next.scheduling._capture_material.as_ref().unwrap(),
    ));
    assert_eq!(owner.capture_published_root()?.token, root.token);
    let last = read.clone();
    drop((read, next, owner, root, runtime));
    assert!(weak.upgrade().is_some());
    drop(last);
    assert!(weak.upgrade().is_none());
    Ok(())
}

#[tokio::test]
async fn owner_capture_reuses_active_bytes_and_records_without_fresh_credit() -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    runtime.join_resource_worker().await?;
    let owner = runtime.begin_mutation().await?;
    let root = owner.capture_published_root()?;
    let domain = root.scheduling.as_ref().unwrap();
    assert!(domain.records.is_empty());
    let cost = empty_capture_cost(domain)?;
    let header = ((domain.scope.agent_id.len() as u64 + 256) * 8, 64);
    let request = SchedulingCaptureRequest {
        include_live: false,
        history_limit: 0,
        required_keys: BTreeSet::new(),
    };
    for bytes_bound in [false, true] {
        for exact in [false, true] {
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                domain.scope(),
                if bytes_bound {
                    header.0 + 2 * cost.0 - u64::from(!exact)
                } else {
                    64 * 1024 * 1024
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
                first = Arc::new(owner.capture_scheduling_read(&request)?);
                assert!(Arc::ptr_eq(
                    first.scheduling._capture_material.as_ref().unwrap(),
                    &guard,
                ));
                assert_eq!(phase.finish()?, (header.0 + cost.0, header.1 + cost.1));
                let second = owner.capture_scheduling_read(&request);
                if exact {
                    let second = second?;
                    assert!(Arc::ptr_eq(
                        second.scheduling._capture_material.as_ref().unwrap(),
                        &guard,
                    ));
                    assert_eq!(
                        phase.finish()?,
                        (header.0 + 2 * cost.0, header.1 + 2 * cost.1)
                    );
                    assert!(Arc::ptr_eq(
                        &ResourceReadMaterial::active_guard()?.unwrap(),
                        &guard,
                    ));
                } else {
                    let error = second.err().context("second capture must reject")?;
                    assert!(error.to_string().contains("context resource read material"));
                    assert!(phase.finish().is_err());
                    assert!(owner.capture_scheduling_read(&request).is_err());
                    assert!(ResourceReadMaterial::active_guard().is_err());
                }
                assert_eq!(owner.capture_published_root()?.token, root.token);
            }
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            let last = first.clone();
            drop((first, phase, guard));
            assert!(weak.upgrade().is_some());
            drop(last);
            assert!(weak.upgrade().is_none());
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}
