use super::*;
use crate::{
    runtime::{
        resident_task::{ResidentTaskDomain, TaskReadError},
        tests::support::{host_backed_test_runtime, persist_task_transition},
    },
    types::{AgentLifecycleDisposition, ChildAgentWorkspaceMode, TaskRecoverySpec},
};

async fn admitted_invocation_fixture() -> Result<(
    tempfile::TempDir,
    RuntimeHost,
    RuntimeHandle,
    RuntimeHandle,
    AgentIdentityRecord,
    TaskRecord,
    String,
    String,
)> {
    let (home, host, parent) = host_backed_test_runtime().await;
    parent.wait_for_bootstrap().await?;
    host.create_named_agent("task-monitor-target", None).await?;
    let child = host.get_public_agent("task-monitor-target").await?;
    child.wait_for_bootstrap().await?;
    let identity = host
        .active_agent_identity(child.instance_agent_id())
        .map_err(anyhow::Error::from)?;
    let now = Utc::now();
    let task = TaskRecord {
        id: crate::ids::task_id(),
        agent_id: parent.instance_agent_id().into(),
        kind: TaskKind::ActorInvocation,
        status: TaskStatus::Running,
        created_at: now,
        updated_at: now,
        parent_message_id: None,
        work_item_id: None,
        summary: Some("original invocation monitor".into()),
        detail: Some(json!({"workspace_mode":"inherit","created_new_subagent":false})),
        recovery: Some(TaskRecoverySpec::AgentInvocation {
            summary: "original invocation monitor".into(),
            prompt: "finish the original request".into(),
            authority_class: AuthorityClass::OperatorInstruction,
            target_agent_id: Some(identity.agent_id.clone()),
            created_new_subagent: false,
            workspace_mode: ChildAgentWorkspaceMode::Inherit,
            lifecycle_disposition: AgentLifecycleDisposition::Retain,
        }),
    };
    persist_task_transition(&parent, &task, "monitor_original_invocation_ack").await?;
    let admitted = host
        .invoke_existing_agent(
            &task,
            &identity.agent_id,
            "finish the original request".into(),
            AuthorityClass::OperatorInstruction,
        )
        .await?;
    let delivery = admitted
        .delivery_id
        .context("actual invocation delivery missing")?;
    let turn = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let state = child.agent_state().await?;
            let actual = host
                .runtime_db()
                .agent_message_deliveries()
                .latest(&delivery)?
                .context("actual delivery missing")?;
            if actual.state == AgentMessageDeliveryState::Consumed
                && state.current_run_id.is_none()
                && state.pending == 0
                && state.turn_index > admitted.child_turn_baseline
            {
                let terminal = state
                    .last_turn_terminal
                    .as_ref()
                    .context("actual terminal missing")?;
                let message = actual
                    .message_id
                    .as_deref()
                    .context("actual delivery message missing")?;
                if host
                    .runtime_db()
                    .queue_entries()
                    .latest(message)?
                    .is_some_and(|row| row.status == crate::types::QueueEntryStatus::Processed)
                {
                    break Ok::<_, anyhow::Error>(terminal.turn_id.clone());
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .context("actual invocation did not complete its original Queue ACK")??;
    Ok((home, host, parent, child, identity, task, delivery, turn))
}
fn child_task(child: &RuntimeHandle, id: &str, turn: &str, status: TaskStatus) -> TaskRecord {
    let now = Utc::now();
    TaskRecord {
        id: id.into(),
        agent_id: child.instance_agent_id().into(),
        kind: TaskKind::CommandTask,
        status,
        created_at: now,
        updated_at: now,
        parent_message_id: None,
        work_item_id: None,
        summary: Some(id.into()),
        detail: Some(json!({"parent_turn_id":turn})),
        recovery: None,
    }
}
fn deny_task_state(host: &RuntimeHost) -> Result<()> {
    host.runtime_db().transaction(|tx| { tx.execute_batch("ALTER TABLE tasks RENAME TO monitor_task_bodies_forbidden; ALTER TABLE agent_states RENAME TO monitor_state_bodies_forbidden;")?;Ok(()) })
}
fn restore_task_state(host: &RuntimeHost) -> Result<()> {
    host.runtime_db().transaction(|tx| { tx.execute_batch("ALTER TABLE monitor_task_bodies_forbidden RENAME TO tasks; ALTER TABLE monitor_state_bodies_forbidden RENAME TO agent_states;")?;Ok(()) })
}

#[tokio::test]
async fn loaded_monitor_completed_and_delivery_paths_keep_actual_ack_without_task_body_sql(
) -> Result<()> {
    let (_home, host, _parent, child, identity, task, delivery, turn) =
        admitted_invocation_fixture().await?;
    let storage = host.agent_storage(&identity.agent_id)?;
    let mut roles = Vec::new();
    let before = host
        .invocation_delivery_terminal_evidence(
            &storage,
            &identity.agent_id,
            &delivery,
            &task.id,
            identity.incarnation,
            &mut roles,
        )
        .await?
        .context("actual settled invocation missing")?;
    assert_eq!(before.status, TaskStatus::Completed);
    assert_eq!(before.turn_id.as_deref(), Some(turn.as_str()));
    assert_eq!(roles.len(), 1);
    assert!(host
        .completed_child_terminal_from_storage(&storage, &identity, 0)
        .await?
        .is_some());
    deny_task_state(&host)?;
    let repeated = host
        .invocation_delivery_terminal_evidence(
            &storage,
            &identity.agent_id,
            &delivery,
            &task.id,
            identity.incarnation,
            &mut roles,
        )
        .await?
        .context("loaded invocation must remain completed")?;
    assert_eq!(repeated.status, before.status);
    assert_eq!(repeated.turn_id, before.turn_id);
    assert_eq!(
        roles.len(),
        1,
        "ordinary polls must retain the existing role without new preparation"
    );
    assert_eq!(
        host.await_child_terminal_result(&identity.agent_id, 0, false)
            .await?
            .status,
        TaskStatus::Completed
    );
    assert!(!child
        .child_monitor_read(identity.incarnation)
        .await?
        .has_lifecycle_blockers()?);
    let error = child
        .child_monitor_read(identity.incarnation + 1)
        .await
        .err()
        .context("successor incarnation cannot read original facts")?;
    assert!(error.to_string().contains("incarnation changed"));
    restore_task_state(&host)?;
    drop(roles);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn loaded_invocation_role_observes_all_parent_tasks_and_actual_writer_updates_without_body_sql(
) -> Result<()> {
    let (_home, host, _parent, child, identity, task, delivery, turn) =
        admitted_invocation_fixture().await?;
    let mut roles = Vec::new();
    // A real role is registered before writes and retained across every ACK.
    let initial = host
        .invocation_task_rows(&identity.agent_id, identity.incarnation, &turn, &mut roles)
        .await?;
    assert!(initial.is_empty());
    drop(initial);
    for index in 0..130 {
        persist_task_transition(
            &child,
            &child_task(
                &child,
                &format!("monitor-parent-{index:03}"),
                &turn,
                TaskStatus::Completed,
            ),
            "monitor_terminal_member_ack",
        )
        .await?;
    }
    let mut active = child_task(&child, "monitor-active-blocker", &turn, TaskStatus::Running);
    persist_task_transition(&child, &active, "monitor_active_member_ack").await?;
    let rows = host
        .invocation_task_rows(&identity.agent_id, identity.incarnation, &turn, &mut roles)
        .await?;
    assert_eq!(rows.ids().count(), 131);
    assert!(rows.has_active());
    drop(rows);
    assert!(child
        .child_monitor_read(identity.incarnation)
        .await?
        .has_lifecycle_blockers()?);
    let mut ignored = child_task(
        &child,
        "monitor-supervision-task",
        &turn,
        TaskStatus::Running,
    );
    ignored.kind = TaskKind::ChildAgentTask;
    persist_task_transition(&child, &ignored, "monitor_supervision_member_ack").await?;
    active.status = TaskStatus::Completed;
    active.updated_at = Utc::now().max(active.updated_at);
    persist_task_transition(&child, &active, "monitor_active_member_finished_ack").await?;
    assert!(
        !child
            .child_monitor_read(identity.incarnation)
            .await?
            .has_lifecycle_blockers()?,
        "ChildAgentTask keeps its original non-blocking predicate"
    );
    let storage = host.agent_storage(&identity.agent_id)?;
    deny_task_state(&host)?;
    let rows = host
        .invocation_task_rows(&identity.agent_id, identity.incarnation, &turn, &mut roles)
        .await?;
    assert_eq!(rows.ids().count(), 132);
    assert!(
        rows.has_active(),
        "parentTurn status observes ChildAgentTask even though lifecycle blocker excludes it"
    );
    assert!(host
        .invocation_delivery_terminal_evidence(
            &storage,
            &identity.agent_id,
            &delivery,
            &task.id,
            identity.incarnation,
            &mut roles
        )
        .await?
        .is_none());
    assert!(host
        .completed_child_terminal_from_storage(&storage, &identity, 0)
        .await?
        .is_some());
    drop(rows);
    restore_task_state(&host)?;
    ignored.status = TaskStatus::Completed;
    ignored.updated_at = Utc::now().max(ignored.updated_at);
    persist_task_transition(&child, &ignored, "monitor_supervision_member_finished_ack").await?;
    let rows = host
        .invocation_task_rows(&identity.agent_id, identity.incarnation, &turn, &mut roles)
        .await?;
    assert_eq!(rows.ids().count(), 132);
    assert!(!rows.has_active());
    drop(rows);
    // Complete terminal Task IDs without Result continuations preserve Waiting.
    assert!(host
        .invocation_delivery_terminal_evidence(
            &storage,
            &identity.agent_id,
            &delivery,
            &task.id,
            identity.incarnation,
            &mut roles
        )
        .await?
        .is_none());
    drop(roles);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn loaded_monitor_task_none_foreign_and_incomplete_never_fall_back_to_storage() -> Result<()>
{
    let (_home, host, parent, child, identity, _task, _delivery, turn) =
        admitted_invocation_fixture().await?;
    let storage = host.agent_storage(&identity.agent_id)?;
    let mut roles = Vec::new();
    drop(
        host.invocation_task_rows(&identity.agent_id, identity.incarnation, &turn, &mut roles)
            .await?,
    );
    let original = child
        .replace_task_domain_for_reader_test(None)
        .await?
        .context("actual Task view missing")?;
    let mut coverage = original.coverage().clone();
    coverage.active_complete = false;
    let limits = host.inner.input_evidence_budget.limits();
    let incomplete = Arc::new(
        ResidentTaskDomain::bootstrap_with_stops(
            original.scope().clone(),
            coverage,
            original.selected_install_records(),
            original.selected_install_stop_records(),
            usize::try_from(limits.record_limit)?,
            usize::try_from(limits.history_per_agent_bytes + limits.pending_per_agent_bytes)?,
        )?
        .with_budget(host.inner.input_evidence_budget.clone())?,
    );
    let foreign_role = parent
        .begin_parent_turn_task_session(parent.instance_incarnation(), &turn)
        .await?;
    deny_task_state(&host)?;
    for view in [None, Some(incomplete)] {
        child.replace_task_domain_for_reader_test(view).await?;
        let error = host
            .completed_child_terminal_from_storage(&storage, &identity, 0)
            .await
            .err()
            .context("unprepared loaded Task must fail before storage")?;
        assert!(!error.to_string().contains("no such table"), "{error:#}");
    }
    child.replace_task_domain_for_reader_test(None).await?;
    let error = host
        .invocation_task_rows(&identity.agent_id, identity.incarnation, &turn, &mut roles)
        .await
        .err()
        .context("TaskNone cannot acquire a historical role")?;
    assert!(
        matches!(
            error.downcast_ref::<TaskReadError>(),
            Some(TaskReadError::IncompleteActive)
        ),
        "{error:#}"
    );
    child
        .replace_task_domain_for_reader_test(Some(original))
        .await?;
    let error = child
        .capture_parent_turn_tasks(identity.incarnation, &foreign_role)
        .await
        .err()
        .context("foreign live role cannot select child facts")?;
    assert!(
        error.to_string().contains("another runtime scope or owner"),
        "{error:#}"
    );
    drop(foreign_role);
    restore_task_state(&host)?;
    drop(roles);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn parent_turn_capture_retains_real_pins_and_no_state_cannot_hydrate_a_role() -> Result<()> {
    let (_home, host, _parent, child, identity, _task, _delivery, turn) =
        admitted_invocation_fixture().await?;
    let role = child
        .begin_parent_turn_task_session(identity.incarnation, &turn)
        .await?;
    let pins = || -> Result<u64> {
        Ok(host
            .inner
            .input_evidence_budget
            .report()?
            .scopes
            .iter()
            .find(|scope| {
                scope.scope.agent_id == identity.agent_id
                    && scope.scope.identity_incarnation == identity.incarnation
            })
            .and_then(|scope| {
                scope
                    .categories
                    .get(&crate::runtime::input_evidence_budget::InputMemoryCategory::Pins)
            })
            .map_or(0, |usage| usage.bytes))
    };
    let baseline = pins()?;
    let read = child
        .capture_parent_turn_tasks(identity.incarnation, &role)
        .await?;
    assert!(
        pins()? > baseline,
        "returned selected metadata must remain charged during protocol consumption"
    );
    assert!(read.headers().is_empty());
    drop(read);
    assert_eq!(
        pins()?,
        baseline,
        "last capture drop releases its actual selected metadata allocation"
    );
    drop(role);
    let original_state = child.agent_state().await?;
    let original = child.replace_task_domain_for_reader_test(None).await?;
    child.close_execution_admission();
    host.runtime_db().transaction(|tx| {
        tx.execute(
            "DELETE FROM agent_states WHERE agent_id=?1",
            [&identity.agent_id],
        )?;
        Ok(())
    })?;
    let error = child
        .begin_parent_turn_task_session(identity.incarnation, &turn)
        .await
        .err()
        .context("NoState TaskNone must not hydrate current history")?;
    assert!(
        matches!(
            error.downcast_ref::<TaskReadError>(),
            Some(TaskReadError::IncompleteActive)
        ),
        "{error:#}"
    );
    assert!(host
        .runtime_db()
        .agent_states()
        .latest(&identity.agent_id)?
        .is_none());
    host.runtime_db()
        .agent_states()
        .upsert_for_fixture(&original_state)?;
    child.replace_task_domain_for_reader_test(original).await?;
    host.shutdown().await?;
    Ok(())
}
