use super::*;
use crate::runtime::resident_work::{CanonicalWorkRecord, WorkFamily, WorkKey};
use crate::runtime::tests::support::{context_config, tempdir};

#[tokio::test]
async fn actual_created_result_and_query_share_remaining_cap_and_wire_guards() -> Result<()> {
    let home = tempdir()?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "work-query-result-prior",
        home.path().into(),
        context_config(),
    )?;
    runtime.stop_context_resource_observer().await?;
    let read = runtime
        .create_work_item_owned_result(
            "actual Work result and query".into(),
            None,
            Some("original file material ".repeat(4096)),
            Vec::new(),
        )
        .await?;
    let material = read.material();
    let cost = material.material_estimate();
    let owner = runtime.begin_mutation().await?;
    let original = runtime.capture_work_query_snapshot_from_owner(
        &owner,
        false,
        None,
        (64 * 1024 * 1024, 65_536),
    )?;
    let query_cost = original.material_estimate()?;
    drop(original);
    let limit = usize::try_from(cost.0.max(query_cost.0))?
        .checked_add(1024)
        .context("fixture byte limit overflow")?;
    assert!(limit < 64 * 1024 * 1024);
    assert!(usize::try_from(cost.0)? < limit && usize::try_from(query_cost.0)? < limit);
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let error = runtime
        .capture_work_query_snapshot_from_owner(&owner, false, Some(material), (limit, 65_536))
        .err()
        .context("individually fitting result and selected query must reject together")?;
    assert!(
        matches!(
            error.downcast_ref::<WorkReadError>(),
            Some(WorkReadError::ResourceExhausted(_))
        ) || matches!(
            error.downcast_ref::<crate::runtime::resident_scheduling::SchedulingReadError>(),
            Some(crate::runtime::resident_scheduling::SchedulingReadError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    let other_home = tempdir()?;
    let other = RuntimeHandle::new_isolated_resource_fixture(
        "other-work-query",
        other_home.path().into(),
        context_config(),
    )?;
    other.stop_context_resource_observer().await?;
    let foreign = other
        .create_work_item_owned_result("foreign real result".into(), None, None, Vec::new())
        .await?;
    assert!(runtime
        .capture_work_query_snapshot_from_owner(
            &owner,
            false,
            Some(foreign.material()),
            (64 * 1024 * 1024, 65_536)
        )
        .is_err());
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    runtime.inner.runtime_db.connection()?.execute_batch(
        "DROP TABLE work_items; DROP TABLE agent_states; DROP TABLE agent_identities;",
    )?;
    let snapshot = runtime.capture_work_query_snapshot_from_owner(
        &owner,
        false,
        Some(material),
        (64 * 1024 * 1024, 65_536),
    )?;
    let weak = Arc::downgrade(&snapshot._owned);
    assert!(snapshot.material_estimate()?.0 > cost.0);
    assert!(runtime.inner.input_evidence_budget.report()?.observed_bytes > baseline);
    let wire = serde_json::to_value(&snapshot.items)?;
    assert_eq!(wire.as_array().context("Work query array")?.len(), 1);
    assert!(
        weak.upgrade().is_some(),
        "query guard survives synchronous serialization"
    );
    drop(wire);
    drop(snapshot);
    assert!(weak.upgrade().is_none());
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    drop(owner);
    drop(read);
    Ok(())
}
#[tokio::test]
async fn actual_unchanged_result_deduplicates_only_the_same_qualified_work_arc() -> Result<()> {
    let home = tempdir()?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "work-query-same-allocation",
        home.path().into(),
        context_config(),
    )?;
    let created = runtime
        .create_work_item_owned_result(
            "actual same allocation".into(),
            None,
            Some("original plan".into()),
            Vec::new(),
        )
        .await?;
    let (record, _created) = created.into_parts();
    runtime.refresh_context_resources().await?;
    let read = runtime
        .update_work_item_owned_result(record.id.clone(), None, None, None)
        .await?;
    runtime.stop_context_resource_observer().await?;
    let owner = runtime.begin_mutation().await?;
    let root = owner.capture_published_root()?;
    let work = root.work.as_ref().context("actual Work domain")?;
    let target=work.borrowed_records().find(|target|matches!(&target.record,CanonicalWorkRecord::Work(actual) if actual.id==record.id)).context("actual accepted target")?;
    assert!(read.material().shares_work_allocation(target));
    let cost = read.material().material_estimate();
    let prior = (usize::try_from(cost.0)?, cost.1);
    let mut request = WorkCaptureRequest::current();
    request.include_live = false;
    request.nonopen_limit = 0;
    request
        .mandatory_roots
        .insert(WorkKey::new(WorkFamily::Work, &record.id));
    let (full, full_cost) = work.capture_with_prior(
        &request,
        &runtime.inner.input_evidence_budget,
        prior,
        (64 * 1024 * 1024, 65_536),
    )?;
    let (shared, shared_cost) = work.capture_with_result_prior(
        &request,
        &runtime.inner.input_evidence_budget,
        prior,
        (64 * 1024 * 1024, 65_536),
        read.material(),
    )?;
    let body = target.record.resident_material_estimate()?;
    assert_eq!(
        (full_cost.0 - shared_cost.0, full_cost.1 - shared_cost.1),
        body
    );
    assert!(Arc::ptr_eq(
        full.lookup(&target.key)?.context("full selected target")?,
        shared
            .lookup(&target.key)?
            .context("shared selected target")?
    ));
    let outside = crate::runtime::resident_work::QualifiedWorkRecord {
        scope: target.scope.clone(),
        key: target.key.clone(),
        revision: target.revision,
        content_digest: target.content_digest.clone(),
        resident_bytes: target.resident_bytes,
        record: target.record.clone(),
    };
    assert!(
        !read.material().shares_work_allocation(&Arc::new(outside)),
        "equal ID/body is not allocation identity"
    );
    drop((full, shared, root, owner, read));
    Ok(())
}
