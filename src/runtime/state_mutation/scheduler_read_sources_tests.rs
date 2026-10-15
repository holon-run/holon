use super::*;
use crate::{
    runtime::tests::support::LifecycleHarness,
    runtime_db::input_evidence_snapshot::prepare_named_run_sources_for_fixture,
    types::{
        AuditEvent, AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin,
        Priority, TaskKind, TaskRecord, TaskStatus,
    },
};
const LIMITS: (usize, usize) = (64 * 1024 * 1024, 65536);

async fn actual_sources(runtime: &RuntimeHandle) -> Result<()> {
    let mut held = runtime.inner.mutation_owner.reserve().await?;
    let root = held
        .capture_ready_snapshot()?
        .context("actual Root missing")?;
    let scope = root
        .tasks
        .as_ref()
        .context("actual Task scope missing")?
        .scope();
    let roles = held.input_query_roles(scope, &runtime.inner.input_evidence_budget, LIMITS)?;
    let events = held.current_event_roles(scope, &runtime.inner.input_evidence_budget, LIMITS)?;
    let (inputs, current) = prepare_named_run_sources_for_fixture(
        runtime.runtime_db(),
        scope,
        &runtime.inner.input_evidence_budget,
        roles.as_ref(),
        held.owner_epoch(),
        &root.state,
        events.as_ref(),
    )?;
    held.begin("actual-scheduler-source-fixture".into())?;
    held.committed_prepared_with_current_read_domains(
        root.state.clone(),
        root.episodes.clone(),
        Some(Arc::new(inputs)),
        root.work.clone(),
        root.tasks.clone(),
        root.scheduling.clone(),
        Some(current),
    )?;
    held.publish(root.state.clone())?;
    Ok(())
}
async fn accepted_task(runtime: &RuntimeHandle) -> Result<()> {
    let at = runtime.now();
    runtime
        .persist_task_transition(
            &TaskRecord {
                id: "actual-scheduler-source-task".into(),
                agent_id: runtime.instance_agent_id().into(),
                kind: TaskKind::CommandTask,
                status: TaskStatus::Queued,
                created_at: at,
                updated_at: at,
                parent_message_id: None,
                work_item_id: None,
                summary: None,
                detail: None,
                recovery: None,
            },
            "task_queued",
        )
        .await
}
fn deny_storage(runtime: &RuntimeHandle) -> Result<()> {
    runtime.runtime_db().connection()?.execute_batch(
        "ALTER TABLE agent_states RENAME TO scheduler_source_forbidden_states;
         ALTER TABLE tasks RENAME TO scheduler_source_forbidden_tasks;
         ALTER TABLE work_items RENAME TO scheduler_source_forbidden_work;
         ALTER TABLE wait_conditions RENAME TO scheduler_source_forbidden_waits;
         ALTER TABLE timers RENAME TO scheduler_source_forbidden_timers;
         ALTER TABLE timer_wakes RENAME TO scheduler_source_forbidden_wakes;
         ALTER TABLE external_triggers RENAME TO scheduler_source_forbidden_triggers;
         ALTER TABLE messages RENAME TO scheduler_source_forbidden_messages;
         ALTER TABLE queue_entries RENAME TO scheduler_source_forbidden_queue;
         ALTER TABLE audit_events RENAME TO scheduler_source_forbidden_events;",
    )?;
    Ok(())
}
#[tokio::test]
async fn actual_scheduler_source_prior_is_checked_before_selected_clones_and_has_no_root_or_sql(
) -> Result<()> {
    let h = LifecycleHarness::new();
    let runtime = h.runtime();
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    runtime
        .enqueue(MessageEnvelope::new(
            runtime.instance_agent_id(),
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Json {
                value: serde_json::json!({"actual":vec![0;1024]}),
            },
        ))
        .await?;
    accepted_task(runtime).await?;
    let root = runtime
        .inner
        .mutation_owner
        .capture()?
        .context("actual Root absent")?;
    let scope = root.tasks.as_ref().unwrap().scope();
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let prior = (128 * 1024u64, 32usize);
    let guard = ledger.register_owned_bytes(
        scope,
        InputMemoryCategory::Pins,
        OwnedBytesEstimate {
            payload_bytes: prior.0,
            metadata_estimate_bytes: 0,
        },
    )?;
    let live = vec![3u8; usize::try_from(prior.0)?];
    let strong = Arc::strong_count(&root);
    deny_storage(runtime)?;
    let request = WorkCaptureRequest::current();
    let capture = runtime.capture_scheduler_sources_from_snapshot_with_material(
        &root, &request, LIMITS, prior, None, None,
    )?;
    assert_eq!(capture.token, root.token);
    assert_eq!(
        capture.material_estimate(),
        capture.additional_material_estimate()
    );
    assert_eq!(Arc::strong_count(&root), strong);
    assert!(capture
        .inputs
        .records
        .iter()
        .any(|record| matches!(record.record, PreparedInputEvidence::Message(_))));
    assert!(capture
        .tasks
        .records
        .iter()
        .any(|record| record.locator.key.id == "actual-scheduler-source-task"));
    drop(capture);
    let held = ledger.report()?.observed_bytes;
    assert_eq!(held, baseline + prior.0);
    let fits = |limits| -> Result<bool> {
        match runtime.capture_scheduler_sources_from_snapshot_with_material(
            &root, &request, limits, prior, None, None,
        ) {
            Ok(captured) => {
                drop(captured);
                Ok(true)
            }
            Err(error) => {
                let resource = error.to_string().contains("ResourceExhausted")
                    || error.to_string().contains("resource exhausted");
                anyhow::ensure!(
                    resource,
                    "unexpected preparation instead of actual material gate: {error:#}"
                );
                Ok(false)
            }
        }
    };
    let mut bytes = (0usize, LIMITS.0);
    while bytes.0 < bytes.1 {
        let mid = bytes.0 + (bytes.1 - bytes.0) / 2;
        if fits((mid, LIMITS.1))? {
            bytes.1 = mid
        } else {
            bytes.0 = mid + 1
        }
        assert_eq!(ledger.report()?.observed_bytes, held);
    }
    let exact_bytes = bytes.0;
    let mut nodes = (0usize, LIMITS.1);
    while nodes.0 < nodes.1 {
        let mid = nodes.0 + (nodes.1 - nodes.0) / 2;
        if fits((exact_bytes, mid))? {
            nodes.1 = mid
        } else {
            nodes.0 = mid + 1
        }
        assert_eq!(ledger.report()?.observed_bytes, held);
    }
    // Current's borrowed request sizing includes the decimal limit fields.
    // Narrowing the native cap can shorten its encoded number, so find the
    // byte boundary again at this actual final native cap before asserting it.
    let mut joint_bytes = (0usize, exact_bytes);
    while joint_bytes.0 < joint_bytes.1 {
        let middle = joint_bytes.0 + (joint_bytes.1 - joint_bytes.0) / 2;
        if fits((middle, nodes.0))? {
            joint_bytes.1 = middle
        } else {
            joint_bytes.0 = middle + 1
        }
        assert_eq!(ledger.report()?.observed_bytes, held);
    }
    let exact_bytes = joint_bytes.0;
    assert!(!fits((exact_bytes - 1, nodes.0))?);
    assert!(!fits((exact_bytes, nodes.0 - 1))?);
    assert!(fits((exact_bytes, nodes.0))?);
    assert_eq!(Arc::strong_count(&root), strong);
    assert_eq!(ledger.report()?.observed_bytes, held);
    drop((live, guard));
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}

#[tokio::test]
async fn actual_run_source_loans_only_same_publication_body_arcs_and_retains_new_metadata(
) -> Result<()> {
    let h = LifecycleHarness::new();
    let runtime = h.runtime();
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    actual_sources(runtime).await?;
    let sessions = runtime.begin_run_birth_sessions().await?;
    accepted_task(runtime).await?;
    runtime
        .enqueue(MessageEnvelope::new(
            runtime.instance_agent_id(),
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "actual born Input".into(),
            },
        ))
        .await?;
    runtime
        .append_owned_runtime_audit_event(AuditEvent::legacy(
            "runtime_error",
            serde_json::json!({"actual":true}),
        ))
        .await?;
    actual_sources(runtime).await?;
    let root = runtime
        .inner
        .mutation_owner
        .capture()?
        .context("actual Root missing")?;
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let mut born = runtime.capture_run_birth_sessions_from_snapshot(&sessions, &root, LIMITS)?;
    let prior = born.material_estimate()?;
    let loans = SchedulerReadBodyLoans::from_run_birth(runtime, &root, &born)?;
    let strong = Arc::strong_count(&root);
    deny_storage(runtime)?;
    let request = WorkCaptureRequest::current();
    let read = runtime.capture_scheduler_sources_from_snapshot_with_material(
        &root,
        &request,
        LIMITS,
        prior,
        Some(&loans),
        None,
    )?;
    let own = read.material_estimate();
    let added = read.additional_material_estimate();
    assert!(own.0 > added.0 && own.1 > added.1);
    assert!(
        added.0 > 0 && added.1 > 0,
        "new metadata was mistaken for a physical body loan"
    );
    for record in &read.tasks.records {
        if let Some(original) = born
            .tasks
            .records
            .iter()
            .find(|original| original.locator == record.locator)
        {
            assert!(Arc::ptr_eq(record, original));
        }
    }
    assert_eq!(Arc::strong_count(&root), strong);
    assert!(runtime
        .capture_scheduler_sources_from_snapshot_with_material(
            &root,
            &request,
            LIMITS,
            (prior.0 - 1, prior.1),
            Some(&loans),
            None
        )
        .is_err());
    drop((loans, read));
    // An actual already-held guard precedes this private adversarial fixture copy.
    // Equal locator/body text in a different Arc cannot pass the source factory.
    let original = born
        .tasks
        .records
        .first()
        .context("born Task absent")?
        .clone();
    let (_, native) = borrowed_json_material(&original.fact)?;
    let copy_guard = ledger.register_owned_bytes(
        &born.tasks.scope,
        InputMemoryCategory::Pins,
        OwnedBytesEstimate {
            payload_bytes: u64::try_from(original.resident_bytes)?,
            metadata_estimate_bytes: u64::try_from(native)?
                .checked_mul(256)
                .context("fixture nodes overflow")?,
        },
    )?;
    let replacement = Arc::new(original.as_ref().clone());
    let old = std::mem::replace(&mut born.tasks.records[0], replacement);
    assert!(SchedulerReadBodyLoans::from_run_birth(runtime, &root, &born).is_err());
    let replacement = std::mem::replace(&mut born.tasks.records[0], old);
    drop((replacement, original, copy_guard));
    let other = LifecycleHarness::new();
    assert!(SchedulerReadBodyLoans::from_run_birth(other.runtime(), &root, &born).is_err());
    assert_eq!(Arc::strong_count(&root), strong);
    let loans = SchedulerReadBodyLoans::from_run_birth(runtime, &root, &born)?;
    let read = runtime.capture_scheduler_sources_from_snapshot_with_material(
        &root,
        &request,
        LIMITS,
        prior,
        Some(&loans),
        None,
    )?;
    drop(loans);
    drop(born);
    assert!(
        ledger.report()?.observed_bytes > baseline,
        "selected source guards ended before their last holder"
    );
    drop(read);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}
