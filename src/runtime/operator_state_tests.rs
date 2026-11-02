use super::*;
use crate::runtime::tests::support::LifecycleHarness;
use crate::types::WorkItemPlanStatus;

#[tokio::test]
async fn loaded_state_surfaces_keep_full_queue_beyond_scheduler_candidates() -> Result<()> {
    fn queue_wire<W: serde::Serialize>(
        queue: &crate::work_item_scheduling::WorkQueueReadModel<W>,
    ) -> Result<serde_json::Value> {
        Ok(serde_json::to_value((
            &queue.current,
            &queue.queued_blocked,
            &queue.items,
            &queue.current_runnable,
            &queue.triggered_blocked,
            &queue.queued_runnable,
            &queue.yielded,
            &queue.waiting_for_operator,
            &queue.blocked,
            &queue.completing,
            &queue.completed_recent,
        ))?)
    }

    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    for index in 0..9 {
        runtime
            .create_work_item(
                format!("full State inventory {index}"),
                Some(WorkItemPlanStatus::Ready),
                None,
                Vec::new(),
            )
            .await?;
    }
    let read = runtime.capture_scheduler_projection().await?;
    assert_eq!(read.projection.queued_runnable_work_items.len(), 5);
    let full = read.work.work_queue_with_material(
        &read.state,
        &read.triggers.records,
        &read.acquisition,
    )?;
    assert_eq!(full.items.len(), 9);
    let expected = queue_wire(&full)?;
    let lightweight = runtime.lightweight_agent_state_projection().await?;
    assert_eq!(queue_wire(&lightweight.work_queue)?, expected);
    assert!(lightweight._work_budget.is_some());
    let operator = runtime.operator_state_read_projection(10, 10).await?;
    assert_eq!(queue_wire(&operator.agent.work_queue)?, expected);
    assert!(operator.material._work.is_some());
    Ok(())
}

#[tokio::test]
async fn operator_state_keeps_scheduler_window_after_narrow_owner_preparation() -> Result<()> {
    use crate::runtime::resident_input_evidence::{
        InputEvidenceFamily, InputEvidenceKey, InputEvidenceScope, InputWindowSelector,
    };
    use crate::runtime_db::input_evidence_snapshot::{
        InputEvidenceLoadRequest, InputWindowRequest,
    };
    use crate::types::{
        AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority,
    };

    for limit in [0, 1] {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        runtime.await_initial_resources().await;
        let _refresh = runtime.hold_resource_refresh_for_test().await?;
        drop(runtime.operator_state_read_projection(10, 10).await?);
        let before = runtime.inner.mutation_owner.capture()?.unwrap();
        let message = runtime.runtime_db().transaction(|tx| {
            let message = MessageEnvelope::new(
                "default",
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "qualified narrow preparation dependency".into(),
                },
            );
            let (stored, inserted) = crate::runtime_db::evidence::append_message_tx(tx, &message)?;
            assert!(inserted);
            crate::runtime_db::input_evidence_snapshot::register_committed_input_evidence_tx(
                tx,
                &InputEvidenceScope {
                    agent_id: "default".into(),
                    identity_incarnation: runtime.inner.identity_incarnation,
                },
                &[
                    crate::runtime_db::input_evidence_snapshot::CanonicalInputEvidence::Message(
                        stored.clone(),
                    ),
                ],
            )?;
            Ok(stored)
        })?;
        let mut mutation = runtime.begin_mutation().await?;
        let (_, record_limit, byte_limit) = mutation.context_input_evidence_policy()?;
        let request = InputEvidenceLoadRequest {
            query: None,
            windows: if limit == 0 {
                Vec::new()
            } else {
                vec![InputWindowRequest {
                    selector: InputWindowSelector::RecentBriefs,
                    limit,
                }]
            },
            mandatory_roots: std::collections::BTreeSet::from([InputEvidenceKey::new(
                InputEvidenceFamily::Message,
                &message.id,
            )]),
            interrupted_sources: Default::default(),
            selected_completion_brief_ids: Default::default(),
            record_limit,
            byte_limit,
        };
        mutation.prepare_owner_input_request(&request, 0).await?;
        let prepared = mutation.capture_published_root()?;
        assert_ne!(prepared.token, before.token, "exercise actual preparation");
        drop((mutation, prepared, before));

        // The publication, not an ordinary State read, must retain its dependencies.
        runtime.runtime_db().transaction(|tx| {
            tx.execute_batch(
                "ALTER TABLE messages RENAME TO narrow_state_messages_forbidden;
             ALTER TABLE briefs RENAME TO narrow_state_briefs_forbidden;",
            )?;
            Ok(())
        })?;
        let projection = runtime.operator_state_read_projection(10, 10).await?;
        assert_eq!(projection.agent.agent.id, "default");
    }
    Ok(())
}

#[tokio::test]
async fn operator_state_metadata_uses_original_window_without_granting_event_bodies() -> Result<()>
{
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    for index in 0..64 {
        runtime
            .inner
            .storage
            .append_event(&crate::types::AuditEvent::legacy(
                if index == 0 {
                    "runtime_error"
                } else {
                    "metadata_only"
                },
                serde_json::json!({"raw":"no original body qualification"}),
            ))?;
    }
    drop(runtime.operator_state_read_projection(10, 10).await?);
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let query = root.current.as_ref().unwrap().query.as_ref().unwrap();
    assert!(query.scheduler.latest_runtime_error.is_some());
    assert!(
        query
            .capture(
                &crate::runtime_db::current_query::CurrentSelection {
                    events_after: Some(0),
                    ..Default::default()
                },
                &runtime.inner.input_evidence_budget,
            )
            .is_err(),
        "metadata must not grant raw Event bodies"
    );
    drop(root);
    runtime
        .append_original_audit_event(crate::types::AuditEvent::legacy(
            "metadata_only",
            serde_json::json!({}),
        ))
        .await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let query = root.current.as_ref().unwrap().query.as_ref().unwrap();
    assert_eq!(
        query.scheduler.latest_runtime_error, None,
        "the 65th event must move the old error outside the original window"
    );
    Ok(())
}

#[tokio::test]
async fn operator_state_loaded_same_cut_needs_no_body_sql_or_old_root() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let timer = runtime
        .schedule_timer(3_600_000, None, Some("State cut".into()))
        .await?;
    let first = runtime.operator_state_read_projection(10, 10).await?;
    assert_eq!(first.timers[0].id, timer.id);
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let old = Arc::downgrade(&root);
    drop(root);
    runtime
        .append_original_audit_event(crate::types::AuditEvent::legacy(
            "runtime_error",
            serde_json::json!({}),
        ))
        .await?;
    assert!(old.upgrade().is_none(), "projection must not retain a Root");
    // Body repositories are unavailable after preparation.
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch(
            "ALTER TABLE agent_states RENAME TO state_endpoint_states_forbidden;
             ALTER TABLE agent_identities RENAME TO state_endpoint_identity_forbidden;
             ALTER TABLE queue_entries RENAME TO state_endpoint_queue_forbidden;
             ALTER TABLE audit_events RENAME TO state_endpoint_events_forbidden;
             ALTER TABLE tasks RENAME TO state_endpoint_tasks_forbidden;
             ALTER TABLE wait_conditions RENAME TO state_endpoint_waits_forbidden;
             ALTER TABLE timers RENAME TO state_endpoint_timers_forbidden;
             ALTER TABLE timer_wakes RENAME TO state_endpoint_wakes_forbidden;
             ALTER TABLE external_triggers RENAME TO state_endpoint_triggers_forbidden;",
        )?;
        Ok(())
    })?;
    let _agent = runtime.inner.agent.lock().await;
    let next = tokio::time::timeout(
        Duration::from_secs(5),
        runtime.operator_state_read_projection(10, 10),
    )
    .await??;
    assert_eq!(next.agent.agent, first.agent.agent);
    assert_eq!(next.agent.identity, first.agent.identity);
    assert_eq!(next.timers, first.timers);
    assert_eq!(next.agent.active_task_count, next.tasks.len());
    let guard = next.material.acquisition.guard()?;
    let weak = Arc::downgrade(&guard);
    drop(guard);
    let timer_holder = next.timers[0].clone();
    drop((next.timers, next.external_triggers));
    let bytes = next.material.into_json_bytes(&next.agent.agent)?;
    let cached = bytes.clone();
    let slice = bytes.slice(1..bytes.len() - 1);
    drop(bytes);
    drop(next.agent);
    assert!(weak.upgrade().is_some());
    drop(cached);
    assert!(weak.upgrade().is_some());
    drop(slice);
    assert!(
        weak.upgrade().is_some(),
        "detached Timer loan is still live"
    );
    drop(timer_holder);
    assert!(weak.upgrade().is_none());
    Ok(())
}

#[tokio::test]
async fn operator_state_ready_root_rejects_foreign_scope_before_projection() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    drop(runtime.operator_state_read_projection(10, 10).await?);
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let mut foreign = root.as_ref().clone();
    foreign.token.identity_incarnation += 1;
    let error = runtime
        .operator_state_from_root(&foreign, 10, 10)
        .err()
        .context("foreign State root must fail")?;
    assert!(error.to_string().contains("State root scope differs"));
    Ok(())
}

#[tokio::test]
async fn operator_state_json_rejects_one_byte_short_without_mutating_the_prior_view() -> Result<()>
{
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let scope = super::super::resident_input_evidence::InputEvidenceScope {
        agent_id: runtime.agent_id().await?,
        identity_incarnation: runtime.inner.identity_incarnation,
    };
    let ledger = &runtime.inner.input_evidence_budget;
    let value = serde_json::json!({"state":"bounded original bytes"});
    let size = super::super::input_evidence_budget::serialized_bytes(&value)?;
    let holder = u64::try_from(std::mem::size_of::<StateWire>() + 128)?;
    let make = |limit| -> Result<_> {
        Ok(StateProjectionMaterial {
            acquisition: ResourceReadMaterial::with_test_limits(ledger, &scope, limit, 128)?,
            _work: None,
            _tasks: None,
            _scheduling: None,
            _children: Vec::new(),
        })
    };
    let phase = make(8192)?;
    let used = phase.acquisition.finish()?.0;
    drop(phase);
    let required = used + (holder + size) * 8;
    let good = make(required)?.into_json_bytes(&value)?;
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&good)?, value);
    let bad = make(required - 1)?;
    let failed = bad.acquisition.clone();
    assert!(bad.into_json_bytes(&value).is_err());
    assert!(
        failed.finish().is_err(),
        "the rejected original phase remains latched"
    );
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&good)?, value);
    Ok(())
}
