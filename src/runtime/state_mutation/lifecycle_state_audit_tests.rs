use super::*;
use crate::runtime::{
    current_read_facts::{CurrentReadFact, CurrentReadRequest},
    tests::support::{context_config, tempdir},
};

async fn fixture() -> Result<(tempfile::TempDir, RuntimeHandle, String)> {
    let home = tempdir()?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "lifecycle-state-tail-owner",
        home.path().into(),
        context_config(),
    )?;
    let workspace_id = "actual-detachment-original-workspace".to_owned();
    let workspace_path = home.path().join("original-detachment-workspace");
    std::fs::create_dir_all(&workspace_path)?;
    runtime
        .inner
        .storage
        .append_workspace_entry(&crate::types::WorkspaceEntry::new(
            &workspace_id,
            workspace_path,
            Some("actual original detachment workspace".into()),
        ))?;
    // Existing original State ACK setup avoids invoking the separately unmigrated
    // attach audit. It grants no body alias or new workspace execution role.
    {
        let mut owner = runtime.begin_mutation().await?;
        owner.state.attached_workspaces.push(workspace_id.clone());
        owner.persist_state(&runtime.inner.storage).await?;
    }
    runtime.refresh_context_resources().await?;
    Ok((home, runtime, workspace_id))
}
fn counts(runtime: &RuntimeHandle) -> Result<(usize, usize, usize, usize)> {
    Ok(runtime.runtime_db().connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM audit_events),(SELECT COUNT(*) FROM current_read_members),
         (SELECT COUNT(*) FROM current_event_native_births),(SELECT COUNT(*) FROM input_mutation_receipts)", [],
        |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)))?)
}

#[tokio::test]
async fn actual_workspace_detachment_state_and_tail_share_one_ack_rollback_and_known_publication(
) -> Result<()> {
    for mode in ["normal", "rollback", "known"] {
        let (_home, runtime, id) = fixture().await?;
        let _pause = runtime.pause_resource_observer_for_test().await;
        let before = runtime.inner.mutation_owner.capture()?.unwrap();
        let before_counts = counts(&runtime)?;
        if mode == "rollback" {
            runtime.inject_next_transition_fault(TransitionFaultPoint::AfterAuditWrites);
        }
        if mode == "known" {
            runtime.inject_next_transition_fault(TransitionFaultPoint::BeforeCacheUpdate);
        }
        let result = runtime.detach_workspace(&id).await;
        if mode == "rollback" {
            assert!(result.is_err());
            assert_eq!(counts(&runtime)?, before_counts);
            assert_eq!(
                runtime
                    .runtime_db()
                    .agent_states()
                    .latest(runtime.instance_agent_id())?
                    .as_ref(),
                Some(before.state.as_ref())
            );
            assert!(Arc::ptr_eq(
                &before,
                &runtime.inner.mutation_owner.capture()?.unwrap()
            ));
            continue;
        }
        if mode == "known" {
            assert!(result.is_err());
            assert!(runtime.inner.mutation_owner.capture().is_err());
            runtime.runtime_db().connection()?.execute_batch("ALTER TABLE audit_events RENAME TO lifecycle_tail_audit_forbidden; ALTER TABLE queue_entries RENAME TO lifecycle_tail_queue_forbidden;")?;
            drop(runtime.begin_mutation().await?);
        } else {
            result?;
        }
        let after = runtime.inner.mutation_owner.capture()?.unwrap();
        assert!(!after.state.attached_workspaces.contains(&id));
        assert_eq!(
            after.current_reads.as_ref().unwrap().coverage().revision,
            before.current_reads.as_ref().unwrap().coverage().revision + 1
        );
        let selected = after
            .current_reads
            .as_ref()
            .unwrap()
            .capture(&CurrentReadRequest::current(64 * 1024 * 1024, 65536))?;
        let tails = selected
            .events
            .iter()
            .filter_map(|record| match &record.fact {
                CurrentReadFact::Event(event) if event.kind == "workspace_detached" => Some(event),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(tails.len(), 1);
        assert_eq!(
            tails[0].data,
            serde_json::json!({"agent_id":before.state.id,"workspace_id":id})
        );
        let mutation = selected
            .events
            .iter()
            .filter_map(|record| match &record.fact {
                CurrentReadFact::Event(event)
                    if event.kind == "agent_state_mutated"
                        && event.event_seq < tails[0].event_seq =>
                {
                    Some(event.event_seq)
                }
                _ => None,
            })
            .max();
        assert!(
            mutation.is_some(),
            "original State audit must precede its Lifecycle tail"
        );
        if mode == "known" {
            runtime.runtime_db().connection()?.execute_batch("ALTER TABLE lifecycle_tail_audit_forbidden RENAME TO audit_events; ALTER TABLE lifecycle_tail_queue_forbidden RENAME TO queue_entries;")?;
        }
        let after_counts = counts(&runtime)?;
        assert_eq!(after_counts.0, before_counts.0 + 2);
        assert_eq!(after_counts.1, before_counts.1 + 2);
        assert_eq!(after_counts.2, before_counts.2 + 2);
        assert_eq!(after_counts.3, before_counts.3 + 1);
        drop(runtime.begin_mutation().await?);
        assert_eq!(counts(&runtime)?, after_counts);
    }
    Ok(())
}

#[tokio::test]
async fn workspace_detachment_batch_is_preclone_bounded_and_nonempty_noop_is_rejected() -> Result<()>
{
    let (_home, runtime, id) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let before = runtime.inner.mutation_owner.capture()?.unwrap();
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let error = runtime
        .prepare_original_workspace_detachment_audits(
            runtime.instance_agent_id(),
            std::iter::repeat(id.as_str()).take(20000),
            Some("stale_workspace_anchor"),
        )
        .err()
        .context("original borrowed ID batch must reject before Event allocation")?;
    assert!(matches!(
        error.downcast_ref::<crate::runtime::current_read_facts::CurrentReadError>(),
        Some(crate::runtime::current_read_facts::CurrentReadError::ResourceExhausted)
    ));
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    let mut owner = runtime.begin_mutation().await?;
    let audits = runtime.prepare_original_workspace_detachment_audits(
        runtime.instance_agent_id(),
        std::iter::once(id.as_str()),
        None,
    )?;
    let weak = Arc::downgrade(audits._material.as_ref().unwrap());
    let error = owner
        .persist_state_with_original_lifecycle_audits(audits)
        .await
        .err()
        .context("nonempty tail may not manufacture a noop State write")?;
    assert!(format!("{error:#}").contains("actual State change"));
    assert!(weak.upgrade().is_none());
    drop(owner);
    assert!(Arc::ptr_eq(
        &before,
        &runtime.inner.mutation_owner.capture()?.unwrap()
    ));
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    Ok(())
}

#[tokio::test]
async fn actual_changed_state_detachment_batch_preserves_order_once_revision_and_last_copy_guard(
) -> Result<()> {
    let (_home, runtime, id) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let second = "actual-second-original-workspace";
    {
        let mut owner = runtime.begin_mutation().await?;
        owner.state.attached_workspaces.push(second.into());
        owner.persist_state(&runtime.inner.storage).await?;
    }
    let before = runtime.inner.mutation_owner.capture()?.unwrap();
    let baseline = counts(&runtime)?;
    let mut owner = runtime.begin_mutation().await?;
    owner
        .state
        .attached_workspaces
        .retain(|workspace| workspace != &id && workspace != second);
    let ids = [id.as_str(), second];
    let audits = runtime.prepare_original_workspace_detachment_audits(
        runtime.instance_agent_id(),
        ids.into_iter(),
        Some("stale_workspace_anchor"),
    )?;
    let weak = Arc::downgrade(audits._material.as_ref().unwrap());
    owner
        .persist_state_with_original_lifecycle_audits(audits)
        .await?;
    assert!(weak.upgrade().is_none());
    drop(owner);
    let after = runtime.inner.mutation_owner.capture()?.unwrap();
    assert_eq!(
        after.current_reads.as_ref().unwrap().coverage().revision,
        before.current_reads.as_ref().unwrap().coverage().revision + 1
    );
    let selected = after
        .current_reads
        .as_ref()
        .unwrap()
        .capture(&CurrentReadRequest::current(64 * 1024 * 1024, 65536))?;
    let events = selected
        .events
        .iter()
        .filter_map(|record| match &record.fact {
            CurrentReadFact::Event(event) if event.kind == "workspace_detached" => Some(event),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 2);
    assert!(events
        .iter()
        .all(|event| event.data["reason"] == "stale_workspace_anchor"));
    let mut ordered = events.iter().map(|event| *event).collect::<Vec<_>>();
    ordered.sort_by_key(|event| event.event_seq);
    assert_eq!(ordered[0].data["workspace_id"], id);
    assert_eq!(ordered[1].data["workspace_id"], second);
    let after_counts = counts(&runtime)?;
    assert_eq!(after_counts.0, baseline.0 + 3);
    assert_eq!(after_counts.1, baseline.1 + 3);
    assert_eq!(after_counts.2, baseline.2 + 3);
    assert_eq!(after_counts.3, baseline.3 + 1);
    Ok(())
}
