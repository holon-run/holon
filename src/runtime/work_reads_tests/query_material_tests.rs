use super::*;
use crate::{
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::{
        resident_scheduling::SchedulingCaptureRequest,
        tests::support::{context_config, StubProvider},
    },
};

#[path = "query_material_tests/snapshot_scope_tests.rs"]
mod snapshot_scope_tests;

fn runtime(home: &std::path::Path) -> Result<RuntimeHandle> {
    RuntimeHandle::new(
        "work-query-material",
        home.into(),
        None::<crate::types::WorkspaceEntry>,
        String::new(),
        Arc::new(StubProvider::new("unused")),
        "work-query-material".into(),
        context_config(),
    )
}

async fn query_scope(runtime: &RuntimeHandle) -> Result<InputEvidenceScope> {
    Ok(runtime.begin_mutation().await?.input_evidence_scope())
}

fn header(scope: &InputEvidenceScope) -> (u64, usize) {
    ((scope.agent_id.len() as u64 + 256) * 8, 64)
}

fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}

fn clone_cost(work: &WorkItemRecord) -> Result<(u64, usize)> {
    // Independent decoded-tree oracle, not the production streaming counter.
    fn cells(value: &serde_json::Value) -> usize {
        match value {
            serde_json::Value::String(_) => 1,
            serde_json::Value::Array(values) => {
                2 + values.len().saturating_sub(1) + values.iter().map(cells).sum::<usize>()
            }
            serde_json::Value::Object(values) => {
                2 + 2 * values.len()
                    + values.len().saturating_sub(1)
                    + values.values().map(cells).sum::<usize>()
            }
            _ => 0,
        }
    }
    let count = cells(&serde_json::to_value(work)?);
    Ok((
        (serde_json::to_vec(work)?.len() as u64 * 4 + count as u64 * 128 + 512) * 8,
        (count + 16) * 8,
    ))
}

async fn projection_oracle(
    runtime: &RuntimeHandle,
) -> Result<(serde_json::Value, (u64, usize), (u64, usize))> {
    let owner = runtime.begin_mutation().await?;
    let scope = owner.input_evidence_scope();
    // Separate source selection from queue assembly, including their owning
    // registration descriptors in the same allowance as the real query.
    let capture_phase = ResourceReadMaterial::new(&runtime.inner.input_evidence_budget, &scope)?;
    let (captured, triggers) = capture_phase.read(|| {
        let captured = owner.capture_work_read(
            &crate::runtime::context_capture::context_work_capture_request(&owner.state),
        )?;
        let scheduling = owner.capture_scheduling_read(&SchedulingCaptureRequest::current())?;
        let (_, records, bytes) = owner.context_input_evidence_policy()?;
        let triggers = scheduling.scheduling.clone_trigger_records(
            &runtime.inner.input_evidence_budget,
            records,
            bytes,
        )?;
        Ok((captured, triggers))
    })?;
    let capture_total = capture_phase.finish()?;
    let capture_header = header(&scope);
    let capture_cost = (
        capture_total.0 - capture_header.0,
        capture_total.1 - capture_header.1,
    );
    assert!(capture_cost.0 > 0 && capture_cost.1 > 0);
    let phase = ResourceReadMaterial::new(&runtime.inner.input_evidence_budget, &scope)?;
    let snapshot = phase.read(|| {
        let queue = captured
            .work
            .work_queue(&captured.state, triggers.records)?;
        captured.into_query_snapshot(
            queue.items,
            phase.guard()?,
            &runtime.inner.input_evidence_budget,
        )
    })?;
    Ok((
        serde_json::to_value(&snapshot.items)?,
        phase.finish()?,
        capture_cost,
    ))
}

#[tokio::test]
async fn standalone_query_preserves_projection_and_keeps_material_through_consumption() -> Result<()>
{
    let home = tempfile::tempdir()?;
    let runtime = runtime(home.path())?;
    for text in ["first 中🦀", "second \"[]{}\\"] {
        runtime
            .create_work_item(text.into(), None, None, vec![])
            .await?;
    }
    runtime.join_resource_worker().await?;
    let (expected, cost, _) = projection_oracle(&runtime).await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let mut snapshot = runtime.capture_work_query_snapshot().await?;
    assert_eq!(serde_json::to_value(&snapshot.items)?, expected);
    assert_eq!(snapshot.scope, query_scope(&runtime).await?);
    assert!(Arc::ptr_eq(&snapshot.state, &root.state));
    let material = Arc::downgrade(&snapshot._queue_material);
    let registration = Arc::downgrade(&snapshot._owned);
    assert!(ledger.report()?.observed_bytes >= baseline + cost.0);
    // List moves its selected projections out while keeping the snapshot alive.
    let items = std::mem::take(&mut snapshot.items);
    assert_eq!(serde_json::to_value(&items)?, expected);
    let last = Arc::new(snapshot);
    let snapshot = last.clone();
    drop(last);
    assert!(material.upgrade().is_some());
    assert!(registration.upgrade().is_some());
    assert_eq!(serde_json::to_value(&items)?, expected);
    assert_eq!(
        runtime.inner.mutation_owner.capture()?.unwrap().token,
        root.token
    );
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop((items, snapshot));
    assert!(material.upgrade().is_none());
    assert!(registration.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}

#[tokio::test]
async fn empty_standalone_query_still_retains_its_original_phase() -> Result<()> {
    let home = tempfile::tempdir()?;
    let runtime = runtime(home.path())?;
    runtime.join_resource_worker().await?;
    let scope = query_scope(&runtime).await?;
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let snapshot = runtime.capture_work_query_snapshot().await?;
    assert!(snapshot.items.is_empty());
    assert_eq!(serde_json::to_string(&snapshot.items)?, "[]");
    let material = Arc::downgrade(&snapshot._queue_material);
    assert!(ledger.report()?.observed_bytes >= baseline + header(&scope).0);
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop(snapshot);
    assert!(material.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}

#[tokio::test]
async fn query_reuses_exact_existing_phase_without_new_header_or_allowance() -> Result<()> {
    let home = tempfile::tempdir()?;
    let runtime = runtime(home.path())?;
    runtime
        .create_work_item("original phase 中🦀".into(), None, None, vec![])
        .await?;
    runtime.join_resource_worker().await?;
    let (expected, cost, capture_cost) = projection_oracle(&runtime).await?;
    let scope = query_scope(&runtime).await?;
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let prior = (37 * 8, 3 * 8);
    let total = add(add(cost, capture_cost), prior);
    let phase = ResourceReadMaterial::with_test_limits(ledger, &scope, total.0, total.1)?;
    let holder = phase.guard()?;
    let snapshot = {
        let _entry = phase.enter();
        crate::diagnostics::resource_io::charge_owned_metadata(37, 3)?;
        let snapshot = runtime.capture_work_query_snapshot().await?;
        assert!(Arc::ptr_eq(&holder, &snapshot._queue_material));
        assert!(Arc::ptr_eq(
            &holder,
            &ResourceReadMaterial::active_guard()?.unwrap()
        ));
        snapshot
    };
    assert_eq!(phase.finish()?, total);
    assert_eq!(serde_json::to_value(&snapshot.items)?, expected);
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop((phase, holder));
    assert!(ledger.report()?.observed_bytes >= baseline + total.0);
    drop(snapshot);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}

#[tokio::test]
async fn query_both_axes_reject_at_original_record_gate_and_latch_without_partial_snapshot(
) -> Result<()> {
    let home = tempfile::tempdir()?;
    let runtime = runtime(home.path())?;
    let record = runtime
        .create_work_item("boundary \0中🦀\"[]{}".into(), None, None, vec![])
        .await?;
    runtime.join_resource_worker().await?;
    let (_, _, capture_cost) = projection_oracle(&runtime).await?;
    let scope = query_scope(&runtime).await?;
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let slots = (std::mem::size_of::<&str>() as u64 * 8, 8);
    let key = (
        (std::mem::size_of::<String>() + record.id.len()) as u64 * 8,
        8,
    );
    let prefix = add(add(add(header(&scope), capture_cost), slots), key);
    let full = add(prefix, clone_cost(&record)?);
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    for bytes in [true, false] {
        let phase = ResourceReadMaterial::with_test_limits(
            ledger,
            &scope,
            full.0 - u64::from(bytes),
            full.1 - usize::from(!bytes),
        )?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            let error = match runtime.capture_work_query_snapshot().await {
                Ok(_) => panic!("query must reject before cloning its WorkItem"),
                Err(error) => error,
            };
            assert_eq!(error.to_string(), "resident queue WorkItem clone admission");
            assert_eq!(
                error.downcast_ref::<std::io::Error>().unwrap().kind(),
                std::io::ErrorKind::Other
            );
            assert_eq!(
                crate::runtime_error::describe_runtime_error(&error).code,
                "io_error"
            );
            assert!(ResourceReadMaterial::active_guard().is_err());
            let retry = match runtime.capture_work_query_snapshot().await {
                Ok(_) => panic!("latched query must not acquire another phase"),
                Err(error) => error,
            };
            assert_eq!(
                crate::runtime_error::describe_runtime_error(&retry).code,
                "context_resource_exhausted"
            );
        }
        assert_eq!(
            crate::runtime_error::describe_runtime_error(&phase.finish().unwrap_err()).code,
            "context_resource_exhausted"
        );
        assert_eq!(ledger.report()?.observed_bytes, baseline + prefix.0);
        assert_eq!(
            runtime.inner.mutation_owner.capture()?.unwrap().token,
            root.token
        );
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, baseline + prefix.0);
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, baseline);
    }
    let recovered = runtime.capture_work_query_snapshot().await?;
    assert_eq!(recovered.items.len(), 1);
    assert_eq!(recovered.items[0].work_item, record);
    drop(recovered);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}

#[tokio::test]
async fn empty_query_reuses_innermost_phase_and_restores_outer_without_duplicate_header(
) -> Result<()> {
    let home = tempfile::tempdir()?;
    let runtime = runtime(home.path())?;
    runtime.join_resource_worker().await?;
    let (_, queue_cost, capture_cost) = projection_oracle(&runtime).await?;
    let scope = query_scope(&runtime).await?;
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let cost = header(&scope);
    assert_eq!(queue_cost, add(cost, cost));
    let inner_cost = add(queue_cost, capture_cost);
    let outer = ResourceReadMaterial::with_test_limits(ledger, &scope, cost.0, cost.1)?;
    let inner = ResourceReadMaterial::with_test_limits(ledger, &scope, inner_cost.0, inner_cost.1)?;
    let outer_guard = outer.guard()?;
    let inner_guard = inner.guard()?;
    let snapshot = {
        let _outer = outer.enter();
        let snapshot = {
            let _inner = inner.enter();
            let snapshot = runtime.capture_work_query_snapshot().await?;
            assert!(snapshot.items.is_empty());
            assert!(Arc::ptr_eq(&snapshot._queue_material, &inner_guard));
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &inner_guard
            ));
            snapshot
        };
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &outer_guard
        ));
        snapshot
    };
    assert_eq!(outer.finish()?, cost);
    assert_eq!(inner.finish()?, inner_cost);
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop((inner, inner_guard));
    assert!(ledger.report()?.observed_bytes >= baseline + cost.0 + inner_cost.0);
    drop(snapshot);
    assert_eq!(ledger.report()?.observed_bytes, baseline + cost.0);
    drop((outer, outer_guard));
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}
