use super::*;
use crate::runtime::tests::support::{tempdir, ContextConfig, StubProvider, TempDir};
use crate::runtime_db::prepared_input_evidence::PreparedInputEvidence as PreparedEvidence;
use crate::types::{MessageBody, Priority, TaskKind};
fn fixture() -> Result<(TempDir, TempDir, RuntimeHandle)> {
    let home = tempdir()?;
    let workspace = tempdir()?;
    let runtime = RuntimeHandle::new(
        "default",
        home.path().to_owned(),
        workspace.path().to_owned(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        ContextConfig::default(),
    )?;
    Ok((home, workspace, runtime))
}
fn source(id: &str, status: TaskStatus) -> MessageEnvelope {
    let mut message = MessageEnvelope::new(
        "default",
        MessageKind::TaskResult,
        MessageOrigin::Task { task_id: id.into() },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "finished".into(),
        },
    );
    message.task_id = Some(id.into());
    message.admission_context = Some(AdmissionContext::RuntimeOwned);
    message.delivery_surface = Some(MessageDeliverySurface::TaskRejoin);
    message.metadata = Some(
        serde_json::json!({"task_id":id,"task_kind":"child_agent_task","task_status":status,"task_summary":"finished"}),
    );
    message
}
async fn accepted(runtime: &RuntimeHandle, message: MessageEnvelope) -> Result<MessageEnvelope> {
    let mut mutation = runtime.begin_mutation().await?;
    let original = PreparedEvidence::message(
        mutation.input_evidence_scope(),
        crate::ids::audit_event_id(),
        runtime.now(),
        message.clone(),
        Vec::new(),
    )?;
    mutation.commit_prepared_evidence(original, None).await?;
    let message = runtime
        .storage()
        .read_message_by_id(&message.id)?
        .context("actual source ACK is absent")?;
    assert!(message.message_seq.is_some_and(|seq| seq > 0));
    // This is the named actual accepted-source preparation boundary. It does
    // not persist a Task merely to grant its later read.
    mutation
        .prepare_owner_task_request(&exact_task_request(BTreeSet::from([TaskKey::new(
            TaskFamily::Task,
            message.task_id.as_ref().unwrap(),
        )])))
        .await?;
    mutation
        .prepare_owner_work_request(&work_request(None))
        .await?;
    drop(mutation);
    let at = runtime.now();
    runtime
        .apply_task_result_operation((&message.id, at), || {
            crate::runtime_db::transitions::TaskResultOperation::EnsurePending {
                task_id: message.task_id.clone().unwrap(),
                message: Box::new(message.clone()),
                at,
            }
        })
        .await?
        .record()?;
    Ok(message)
}
#[tokio::test]
async fn original_source_ack_with_global_task_absence_keeps_first_create_timing_and_pure_route(
) -> Result<()> {
    let (_home, _workspace, runtime) = fixture()?;
    let message = accepted(&runtime, source("first-task", TaskStatus::Completed)).await?;
    let candidate = super::super::tasks::task_from_message(&message, "default")?;
    let read = runtime
        .acquire_task_result_routing(&message, &candidate)
        .await?;
    assert!(!read.should_ignore());
    assert!(!read.parent_delivered()?);
    assert!(
        runtime
            .runtime_db()
            .tasks()
            .latest(&candidate.id)?
            .is_none(),
        "routing does not pre-create the original first Task"
    );
    runtime.runtime_db().transaction(|tx| {
        for table in ["tasks", "work_items", "turn_records", "briefs", "messages"] {
            tx.execute_batch(&format!(
                "ALTER TABLE {table} RENAME TO forbidden_route_{table}"
            ))?;
        }
        Ok(())
    })?;
    assert!(!read.should_ignore());
    assert!(!read.parent_delivered()?);
    Ok(())
}
#[tokio::test]
async fn original_task_ack_duplicate_route_is_pure_and_does_not_acquire_parent_body() -> Result<()>
{
    let (_home, _workspace, runtime) = fixture()?;
    let mut task = TaskRecord {
        id: "duplicate-task".into(),
        agent_id: "default".into(),
        kind: TaskKind::ChildAgentTask,
        status: TaskStatus::Completed,
        created_at: runtime.now(),
        updated_at: runtime.now(),
        parent_message_id: None,
        work_item_id: None,
        summary: Some("finished".into()),
        detail: None,
        recovery: None,
    };
    runtime
        .persist_task_transition(&task, "task_completed")
        .await?;
    let message = accepted(&runtime, source(&task.id, TaskStatus::Running)).await?;
    task.status = TaskStatus::Running;
    let read = runtime.acquire_task_result_routing(&message, &task).await?;
    assert!(read.should_ignore());
    runtime.runtime_db().transaction(|tx| {
        for table in ["tasks", "work_items", "turn_records", "briefs", "messages"] {
            tx.execute_batch(&format!(
                "ALTER TABLE {table} RENAME TO forbidden_duplicate_{table}"
            ))?;
        }
        Ok(())
    })?;
    assert!(read.should_ignore());
    assert!(!read.parent_delivered()?);
    Ok(())
}
#[tokio::test]
async fn absent_task_route_rejects_forged_candidate_source_and_wrong_runtime_authority_without_aliases(
) -> Result<()> {
    let (_home, _workspace, runtime) = fixture()?;
    let message = accepted(
        &runtime,
        source("absent-source-task", TaskStatus::Completed),
    )
    .await?;
    let candidate = super::super::tasks::task_from_message(&message, "default")?;
    // Establish the named exact read selector before measuring failed parser
    // materials. Its legitimate Current metadata is retained by the owner.
    let warm = runtime
        .acquire_task_result_routing(&message, &candidate)
        .await?;
    assert!(!warm.should_ignore());
    assert!(!warm.parent_delivered()?);
    drop(warm);
    let before = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let mut forged = candidate.clone();
    forged.detail = Some(serde_json::json!({"parent_turn_id":"untrusted-hint"}));
    assert!(runtime
        .acquire_task_result_routing(&message, &forged)
        .await
        .is_err());
    let mut changed = message.clone();
    changed.body = MessageBody::Text {
        text: "not accepted".into(),
    };
    let changed_candidate = super::super::tasks::task_from_message(&changed, "default")?;
    assert!(runtime
        .acquire_task_result_routing(&changed, &changed_candidate)
        .await
        .is_err());
    let mut wrong = message.clone();
    wrong.authority_class = AuthorityClass::OperatorInstruction;
    assert!(runtime
        .acquire_task_result_routing(&wrong, &candidate)
        .await
        .is_err());
    assert!(runtime
        .runtime_db()
        .tasks()
        .latest(&candidate.id)?
        .is_none());
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        before,
        "all failed parser/read materials release"
    );
    Ok(())
}
#[test]
fn parsed_source_guard_is_same_ledger_bounded_and_releases_after_bad_candidate() -> Result<()> {
    let (_home, _workspace, runtime) = fixture()?;
    let message = source("guarded-source", TaskStatus::Completed);
    let candidate = super::super::tasks::task_from_message(&message, "default")?;
    let scope = resident_input_evidence::InputEvidenceScope {
        agent_id: "default".into(),
        identity_incarnation: 1,
    };
    let before = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    assert!(parsed_source(&runtime, &scope, &message, &candidate, None, 1, 64).is_err());
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        before
    );
    let mut wrong = candidate.clone();
    wrong.summary = Some("forged summary".into());
    assert!(parsed_source(
        &runtime,
        &scope,
        &message,
        &wrong,
        None,
        65536,
        64 * 1024 * 1024
    )
    .is_err());
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        before
    );
    Ok(())
}
#[tokio::test]
async fn actual_result_ack_uses_qualified_closed_work_owner_and_original_unavailable_settlement(
) -> Result<()> {
    let (_home, _workspace, runtime) = fixture()?;
    let owner = runtime
        .create_work_item("original owner".into(), None, None, Vec::new())
        .await?;
    let mut message = source("owner-task", TaskStatus::Completed);
    message.work_item_id = Some(owner.id.clone());
    message.metadata.as_mut().unwrap()["work_item_id"] = serde_json::json!(&owner.id);
    message.metadata.as_mut().unwrap()["task_detail"] = serde_json::json!({"rejoin_obligation_id":"owner-task","rejoin_generation":1,"parent_turn_id":"original-owner-parent"});
    let mut candidate = super::super::tasks::task_from_message(&message, "default")?;
    candidate.parent_message_id = None;
    runtime
        .persist_task_transition(&candidate, "task_completed")
        .await?;
    let message = accepted(&runtime, message).await?;
    let at = runtime.now();
    let result = runtime
        .apply_task_result_operation((&message.id, at), || {
            crate::runtime_db::transitions::TaskResultOperation::EnsurePending {
                task_id: candidate.id.clone(),
                message: Box::new(message.clone()),
                at,
            }
        })
        .await?;
    let settlement = result
        .record()?
        .as_ref()
        .context("actual original Result ACK is absent")?;
    runtime
        .complete_work_item(owner.id.clone(), Vec::new())
        .await?;
    let owner_read = runtime.acquire_task_result_owner(settlement).await?;
    runtime.runtime_db().transaction(|tx| {
        for table in ["tasks", "work_items", "turn_records", "briefs"] {
            tx.execute_batch(&format!(
                "ALTER TABLE {table} RENAME TO forbidden_owner_{table}"
            ))?;
        }
        assert_eq!(
            owner_read.disposition()?,
            Some(TaskResultSettlementDisposition::OwnerClosed)
        );
        for table in ["tasks", "work_items", "turn_records", "briefs"] {
            tx.execute_batch(&format!(
                "ALTER TABLE forbidden_owner_{table} RENAME TO {table}"
            ))?;
        }
        Ok(())
    })?;
    drop(owner_read);
    let at = runtime.now();
    assert!(runtime
        .apply_task_result_operation((&message.id, at), || {
            crate::runtime_db::transitions::TaskResultOperation::SettleOwnerUnavailable {
                message_id: message.id.clone(),
                disposition: TaskResultSettlementDisposition::OwnerClosed,
                at,
            }
        })
        .await?
        .changed()?);
    let actual = runtime
        .runtime_db()
        .task_result_settlements()
        .latest_for_message(&message.id)?
        .context("original settled Result")?;
    assert_eq!(
        actual.disposition,
        Some(TaskResultSettlementDisposition::OwnerClosed)
    );
    Ok(())
}

#[tokio::test]
async fn duplicate_route_combines_live_selector_with_actual_selected_capture() -> Result<()> {
    let (_home, _workspace, runtime) = fixture()?;
    let task = TaskRecord {
        id: "budget-duplicate".into(),
        agent_id: "default".into(),
        kind: TaskKind::ChildAgentTask,
        status: TaskStatus::Completed,
        created_at: runtime.now(),
        updated_at: runtime.now(),
        parent_message_id: None,
        work_item_id: None,
        summary: Some("finished".into()),
        detail: None,
        recovery: None,
    };
    runtime
        .persist_task_transition(&task, "task_completed")
        .await?;
    let message = accepted(&runtime, source(&task.id, TaskStatus::Running)).await?;
    let mut candidate = task.clone();
    candidate.status = TaskStatus::Running;
    let read = runtime
        .acquire_task_result_routing(&message, &candidate)
        .await?;
    assert!(read.should_ignore());
    let selected = selected_capture_material(&read.tasks, &read._work)?;
    drop(read);
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let (token, state) = (root.token, root.state.clone());
    drop(root);
    let before = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    // Every selected allocation fits by itself. The still-live selector is the
    // extra material that this old duplicate early return used to omit.
    let error = runtime
        .acquire_task_result_routing_inner(&message, &candidate, Some((65536, selected.0)), None)
        .await
        .err()
        .context("duplicate route ignored its live selector")?;
    assert!(
        matches!(
            error.downcast_ref::<TaskReadError>(),
            Some(TaskReadError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert!(
        format!("{error:#}").contains("combined selected/selector/outcome"),
        "{error:#}"
    );
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    assert_eq!(root.token, token);
    assert_eq!(root.state, state);
    drop(root);
    assert_eq!(
        runtime.runtime_db().tasks().latest(&task.id)?.unwrap(),
        task
    );
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        before
    );
    Ok(())
}

#[tokio::test]
async fn owner_route_combines_original_result_ack_selected_work_and_live_selector() -> Result<()> {
    let (_home, _workspace, runtime) = fixture()?;
    let owner = runtime
        .create_work_item("budget original owner".into(), None, None, Vec::new())
        .await?;
    let mut message = source("budget-owner-task", TaskStatus::Completed);
    message.work_item_id = Some(owner.id.clone());
    message.metadata.as_mut().unwrap()["work_item_id"] = serde_json::json!(&owner.id);
    message.metadata.as_mut().unwrap()["task_detail"] = serde_json::json!({"rejoin_obligation_id":"budget-owner-task","rejoin_generation":1,"parent_turn_id":"budget-original-parent"});
    let mut task = super::super::tasks::task_from_message(&message, "default")?;
    task.parent_message_id = None;
    runtime
        .persist_task_transition(&task, "task_completed")
        .await?;
    let message = accepted(&runtime, message).await?;
    let settlement = runtime
        .runtime_db()
        .task_result_settlements()
        .latest_for_message(&message.id)?
        .context("actual original pending ACK")?;
    runtime
        .complete_work_item(owner.id.clone(), Vec::new())
        .await?;
    let read = runtime.acquire_task_result_owner(&settlement).await?;
    assert_eq!(
        read.disposition()?,
        Some(TaskResultSettlementDisposition::OwnerClosed)
    );
    let selected =
        selected_capture_material(read.tasks.as_ref().unwrap(), read.work.as_ref().unwrap())?;
    let without_selector = selected
        .0
        .checked_add(usize::try_from(serialized_bytes(&settlement)?)?)
        .unwrap();
    drop(read);
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let (token, state) = (root.token, root.state.clone());
    drop(root);
    let before = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let error = runtime
        .acquire_task_result_owner_inner(&settlement, Some((65536, without_selector)))
        .await
        .err()
        .context("owner route ignored its live selector")?;
    assert!(
        matches!(
            error.downcast_ref::<TaskReadError>(),
            Some(TaskReadError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert!(
        format!("{error:#}").contains("combined selected/selector/outcome"),
        "{error:#}"
    );
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    assert_eq!(root.token, token);
    assert_eq!(root.state, state);
    drop(root);
    assert_eq!(
        runtime
            .runtime_db()
            .task_result_settlements()
            .latest_for_message(&message.id)?,
        Some(settlement)
    );
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        before
    );
    Ok(())
}

#[test]
fn task_result_phase_clamps_total_before_combining_selectors_and_parser() -> Result<()> {
    let limits = restricted_route_limits((80000, 80 * 1024 * 1024), None)?;
    assert_eq!(limits, (65536, 64 * 1024 * 1024));
    assert!(
        combined_route_material((63 * 1024 * 1024, 32), 2 * 1024 * 1024, (0, 0), limits).is_err()
    );
    assert!(
        combined_route_material((8 * 1024 * 1024, 32), 1024, (57 * 1024 * 1024, 16), limits)
            .is_err()
    );
    assert!(combined_route_material((1, 65520), 1, (0, 1), limits).is_err());
    assert!(combined_route_material((8 * 1024 * 1024, 32), 1024, (1024, 16), limits).is_ok());
    Ok(())
}
