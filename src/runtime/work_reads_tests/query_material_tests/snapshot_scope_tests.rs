use super::*;
use crate::{
    runtime::input_evidence_budget::{
        BudgetViewGuard, InputEvidenceBudgetError, InputMemoryCategory,
    },
    work_item_scheduling::WorkItemSchedulingProjection,
};

async fn selected_parts(
    runtime: &RuntimeHandle,
) -> Result<(
    WorkReadSnapshot,
    Vec<WorkItemSchedulingProjection>,
    ResourceReadMaterial,
)> {
    let owner = runtime.begin_mutation().await?;
    let read = owner.capture_work_read(
        &crate::runtime::context_capture::context_work_capture_request(&owner.state),
    )?;
    let phase = ResourceReadMaterial::new(&runtime.inner.input_evidence_budget, &read.work.scope)?;
    let queue = {
        let _entry = phase.enter();
        read.work.work_queue(&read.state, vec![])?
    };
    Ok((read, queue.items, phase))
}

fn pins(ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger) -> Result<u64> {
    Ok(ledger
        .report()?
        .categories
        .get(&InputMemoryCategory::Pins)
        .map_or(0, |usage| usage.bytes))
}

#[tokio::test]
async fn finalization_moves_selected_allocations_and_retains_original_registration() -> Result<()> {
    let home = tempfile::tempdir()?;
    let runtime = runtime(home.path())?;
    for text in ["scope 中🦀", "second \0\"[]{}\\"] {
        runtime
            .create_work_item(text.into(), None, None, vec![])
            .await?;
    }
    runtime.join_resource_worker().await?;
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let (read, items, phase) = selected_parts(&runtime).await?;
    let scope = read.work.scope.clone();
    let scope_pointer = read.work.scope.agent_id.as_ptr();
    let state_pointer = Arc::as_ptr(&read.state);
    let items_pointer = items.as_ptr();
    let capacity = items.capacity();
    let expected = serde_json::to_value(&items)?;
    let registration_bytes = serde_json::to_vec(&items)?.len() as u64 * 3;
    // Retain the selected source views separately to isolate the new Pins entry.
    let source_holders = read.work.budget_views.clone();
    let before = ledger.report()?.observed_bytes;
    let pins_before = pins(ledger)?;
    let guard = phase.guard()?;
    let material = Arc::downgrade(&guard);
    let mut snapshot = read.into_query_snapshot(items, guard.clone(), ledger)?;
    assert_eq!(snapshot.scope, scope);
    assert_eq!(snapshot.scope.agent_id.as_ptr(), scope_pointer);
    assert_eq!(Arc::as_ptr(&snapshot.state), state_pointer);
    assert_eq!(snapshot.items.as_ptr(), items_pointer);
    assert_eq!(snapshot.items.capacity(), capacity);
    assert_eq!(serde_json::to_value(&snapshot.items)?, expected);
    assert!(Arc::ptr_eq(&snapshot._queue_material, &guard));
    assert_eq!(ledger.report()?.observed_bytes, before + registration_bytes);
    assert_eq!(pins(ledger)?, pins_before + registration_bytes);
    let registration = Arc::downgrade(&snapshot._owned);
    let items = std::mem::take(&mut snapshot.items);
    let snapshot = Arc::new(snapshot);
    let last = snapshot.clone();
    drop((snapshot, phase, guard, source_holders));
    assert!(material.upgrade().is_some());
    assert!(registration.upgrade().is_some());
    assert_eq!(serde_json::to_value(&items)?, expected);
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop((items, last));
    assert!(material.upgrade().is_none());
    assert!(registration.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}

#[tokio::test]
async fn empty_finalization_moves_scope_and_preserves_empty_pins_estimate() -> Result<()> {
    let home = tempfile::tempdir()?;
    let runtime = runtime(home.path())?;
    runtime.join_resource_worker().await?;
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let (read, items, phase) = selected_parts(&runtime).await?;
    assert!(items.is_empty());
    let scope_pointer = read.work.scope.agent_id.as_ptr();
    let state_pointer = Arc::as_ptr(&read.state);
    let source_holders = read.work.budget_views.clone();
    let before = ledger.report()?.observed_bytes;
    let pins_before = pins(ledger)?;
    let guard = phase.guard()?;
    let material = Arc::downgrade(&guard);
    let snapshot = read.into_query_snapshot(items, guard, ledger)?;
    assert_eq!(snapshot.scope.agent_id.as_ptr(), scope_pointer);
    assert_eq!(Arc::as_ptr(&snapshot.state), state_pointer);
    assert!(snapshot.items.is_empty());
    // Existing policy registers the streamed "[]" estimate, even when empty.
    assert_eq!(ledger.report()?.observed_bytes, before + 6);
    assert_eq!(pins(ledger)?, pins_before + 6);
    let registration = Arc::downgrade(&snapshot._owned);
    drop((phase, source_holders));
    assert!(material.upgrade().is_some());
    assert!(registration.upgrade().is_some());
    drop(snapshot);
    assert!(material.upgrade().is_none());
    assert!(registration.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}

#[tokio::test]
async fn rejected_registration_releases_consumed_sources_and_original_material() -> Result<()> {
    let home = tempfile::tempdir()?;
    let runtime = runtime(home.path())?;
    runtime
        .create_work_item("registration rejection 中🦀".into(), None, None, vec![])
        .await?;
    runtime.join_resource_worker().await?;
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let pins_before = pins(ledger)?;
    let (mut read, items, phase) = selected_parts(&runtime).await?;
    assert!(!items.is_empty());
    let sources: Vec<std::sync::Weak<BudgetViewGuard>> =
        read.work.budget_views.iter().map(Arc::downgrade).collect();
    assert!(!sources.is_empty());
    let guard = phase.guard()?;
    let material = Arc::downgrade(&guard);
    drop(phase);
    // Inject only the final registration error after a valid selected read.
    read.work.scope.identity_incarnation = 0;
    let error = match read.into_query_snapshot(items, guard, ledger) {
        Ok(_) => anyhow::bail!("invalid registration returned a partial snapshot"),
        Err(error) => error,
    };
    assert!(matches!(
        error.downcast_ref::<InputEvidenceBudgetError>(),
        Some(InputEvidenceBudgetError::InvalidInput(
            "scope needs agent ID and nonzero incarnation"
        ))
    ));
    assert!(sources.iter().all(|source| source.upgrade().is_none()));
    assert!(material.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    assert_eq!(pins(ledger)?, pins_before);
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}

#[tokio::test]
async fn production_query_retains_exact_existing_item_registration_through_last_holder(
) -> Result<()> {
    let home = tempfile::tempdir()?;
    let runtime = runtime(home.path())?;
    runtime
        .create_work_item("production finalization 中🦀".into(), None, None, vec![])
        .await?;
    runtime.join_resource_worker().await?;
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let pins_before = pins(ledger)?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let snapshot = runtime.capture_work_query_snapshot().await?;
    let expected = serde_json::to_vec(&snapshot.items)?.len() as u64 * 3;
    assert_eq!(pins(ledger)?, pins_before + expected);
    assert!(Arc::ptr_eq(&snapshot.state, &root.state));
    assert_eq!(snapshot.scope, query_scope(&runtime).await?);
    let registration = Arc::downgrade(&snapshot._owned);
    let material = Arc::downgrade(&snapshot._queue_material);
    let last = Arc::new(snapshot);
    let other = last.clone();
    drop(last);
    assert!(registration.upgrade().is_some());
    assert!(material.upgrade().is_some());
    assert_eq!(pins(ledger)?, pins_before + expected);
    drop(other);
    assert!(registration.upgrade().is_none());
    assert!(material.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    assert_eq!(pins(ledger)?, pins_before);
    assert_eq!(
        runtime.inner.mutation_owner.capture()?.unwrap().token,
        root.token
    );
    Ok(())
}
