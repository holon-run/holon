use super::*;
use crate::runtime::{
    current_read_facts::{CurrentReadFact, CurrentReadRequest},
    tests::support::persist_task_transition,
};

pub(super) fn assert_published(
    host: &RuntimeHost,
    runtime: &RuntimeHandle,
    kind: &str,
    task_id: &str,
) -> Result<()> {
    let owner = host
        .bridge()
        .mutation_owner_for(runtime.instance_agent_id(), runtime.instance_incarnation())?;
    let root = owner
        .capture()?
        .context("Task recovery audit lost published root")?;
    let current = root
        .current_reads
        .as_ref()
        .context("Task recovery audit invalidated Current")?;
    let selected = current.capture(&CurrentReadRequest::current(64 * 1024 * 1024, 65536))?;
    let matching = selected
        .events
        .iter()
        .filter_map(|record| match &record.fact {
            CurrentReadFact::Event(event)
                if event.kind == kind
                    && (event.data.get("task_id").and_then(Value::as_str) == Some(task_id)
                        || event
                            .data
                            .get("task_ids")
                            .and_then(Value::as_array)
                            .is_some_and(|ids| ids.iter().any(|id| id == task_id))) =>
            {
                Some(event)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        matching.len(),
        1,
        "original recovery observation must publish once"
    );
    let event = matching[0];
    assert!(event.event_seq > 0);
    assert_eq!(host.runtime_db().connection()?.query_row("SELECT COUNT(*) FROM current_event_native_births WHERE agent_id=?1 AND identity_incarnation=?2 AND event_id=?3 AND birth_seq=?4", rusqlite::params![runtime.instance_agent_id(),runtime.instance_incarnation(),event.id,event.event_seq], |row|row.get::<_,usize>(0))?,1);
    Ok(())
}
fn task(runtime: &RuntimeHandle, target: &str) -> TaskRecord {
    let at = Utc::now();
    TaskRecord {
        id: crate::ids::task_id(),
        agent_id: runtime.instance_agent_id().into(),
        kind: crate::types::TaskKind::ActorInvocation,
        status: TaskStatus::Running,
        created_at: at,
        updated_at: at,
        parent_message_id: None,
        work_item_id: None,
        summary: Some("original invocation recovery".into()),
        detail: Some(json!({"target_agent_id":target,"custom":"original"})),
        recovery: Some(TaskRecoverySpec::AgentInvocation {
            summary: "original invocation recovery".into(),
            prompt: "original accepted work".into(),
            authority_class: AuthorityClass::OperatorInstruction,
            target_agent_id: Some(target.into()),
            created_new_subagent: false,
            workspace_mode: crate::types::ChildAgentWorkspaceMode::Inherit,
            lifecycle_disposition: crate::types::AgentLifecycleDisposition::Retain,
        }),
    }
}
#[tokio::test]
async fn original_invocation_recovery_failure_keeps_task_and_whole_audit_rollback() -> Result<()> {
    for rollback in [false, true] {
        let (_home, host) = test_host();
        host.create_named_agent("actual-recovery-target", None)
            .await?;
        let parent = host.default_runtime().await?;
        let task = task(&parent, "actual-recovery-target");
        persist_task_transition(&parent, &task, "recovery_actual_invocation_task").await?;
        let target_before = host
            .runtime_db()
            .agent_states()
            .latest("actual-recovery-target")?;
        let receipts: usize = host.runtime_db().connection()?.query_row(
            "SELECT COUNT(*) FROM input_mutation_receipts",
            [],
            |r| r.get(0),
        )?;
        if rollback {
            host.runtime_db().connection()?.execute_batch("CREATE TRIGGER actual_recovery_audit_abort BEFORE INSERT ON audit_events WHEN NEW.kind='supervised_child_task_recovery_failed' BEGIN SELECT RAISE(ABORT,'actual Task recovery Audit rollback'); END;")?;
            let error = parent
                .recover_supervised_child_tasks(vec![task.clone()])
                .await
                .err()
                .context("actual Audit fault must propagate at original recovery boundary")?;
            assert!(format!("{error:#}").contains("actual Task recovery Audit rollback"));
            assert_eq!(host.runtime_db().connection()?.query_row("SELECT COUNT(*) FROM audit_events WHERE kind='supervised_child_task_recovery_failed'",[],|r|r.get::<_,usize>(0))?,0);
            assert_eq!(
                host.runtime_db().connection()?.query_row(
                    "SELECT COUNT(*) FROM input_mutation_receipts",
                    [],
                    |r| r.get::<_, usize>(0)
                )?,
                receipts
            );
            assert_eq!(
                host.runtime_db().tasks().latest(&task.id)?,
                Some(task.clone())
            );
            host.runtime_db()
                .connection()?
                .execute_batch("DROP TRIGGER actual_recovery_audit_abort;")?;
        }
        let (reattached, remaining) = parent
            .recover_supervised_child_tasks(vec![task.clone()])
            .await?;
        assert!(reattached.is_empty());
        assert_eq!(remaining, vec![task.clone()]);
        assert_eq!(
            host.runtime_db().tasks().latest(&task.id)?,
            Some(task.clone())
        );
        assert_eq!(
            host.runtime_db()
                .agent_states()
                .latest("actual-recovery-target")?,
            target_before
        );
        assert_published(
            &host,
            &parent,
            "supervised_child_task_recovery_failed",
            &task.id,
        )?;
        let events = parent.storage().read_recent_events(64)?;
        let event = events
            .iter()
            .find(|event| event.kind == "supervised_child_task_recovery_failed")
            .unwrap();
        assert_eq!(event.data["target_agent_id"], "actual-recovery-target");
        assert!(event.data["error"]
            .as_str()
            .unwrap()
            .contains("missing its durable turn baseline"));
        host.shutdown().await?;
    }
    Ok(())
}
struct PendingRecoveryProvider;
#[async_trait]
impl AgentProvider for PendingRecoveryProvider {
    async fn complete_turn(&self, _: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        std::future::pending().await
    }
}
#[tokio::test]
async fn original_message_wait_recovery_task_write_fault_publishes_separate_error_observation(
) -> Result<()> {
    let (_home, original) = test_host();
    let config = original.config().as_ref().clone();
    original.shutdown().await?;
    drop(original);
    let host = RuntimeHost::new_with_provider(config, Arc::new(PendingRecoveryProvider))?;
    host.ensure_default_agent_identity()?;
    let parent = host.default_runtime().await?;
    host.create_named_agent("original-message-wait-target", None)
        .await?;
    let receipt = parent
        .agent_invocation_service()
        .invoke(InvokeAgentRequest {
            target: InvokeAgentTarget::ExistingAgent {
                agent_id: "original-message-wait-target".into(),
            },
            message: "real accepted MessageWait source".into(),
            authority_class: AuthorityClass::OperatorInstruction,
        })
        .await?;
    let task = host
        .runtime_db()
        .tasks()
        .latest(&receipt.task_handle.task_id)?
        .context("actual MessageWait Task ACK missing")?;
    assert!(matches!(
        &task.recovery,
        Some(TaskRecoverySpec::AgentMessageWait {
            delivery_id: Some(_),
            after_delivery_rowid: Some(_),
            ..
        })
    ));
    parent.abort_async_task_monitor_for_test(&task.id).await;
    let command=format!("CREATE TRIGGER actual_message_wait_recovery_abort BEFORE UPDATE ON tasks WHEN NEW.task_id='{}' BEGIN SELECT RAISE(ABORT,'actual MessageWait recovery Task rollback'); END;",task.id);
    host.runtime_db().connection()?.execute_batch(&command)?;
    let (reattached, remaining) = parent
        .recover_supervised_child_tasks(vec![task.clone()])
        .await?;
    host.runtime_db()
        .connection()?
        .execute_batch("DROP TRIGGER actual_message_wait_recovery_abort;")?;
    assert!(reattached.is_empty());
    assert_eq!(remaining, vec![task.clone()]);
    assert_eq!(
        host.runtime_db().tasks().latest(&task.id)?,
        Some(task.clone())
    );
    assert_published(
        &host,
        &parent,
        "agent_message_wait_recovery_failed",
        &task.id,
    )?;
    let events = parent.storage().read_recent_events(64)?;
    let event = events
        .iter()
        .find(|event| event.kind == "agent_message_wait_recovery_failed")
        .unwrap();
    assert_eq!(
        event.data["target_agent_id"],
        "original-message-wait-target"
    );
    assert!(event.data["error"]
        .as_str()
        .unwrap()
        .contains("actual MessageWait recovery Task rollback"));
    assert_eq!(
        host.runtime_db().connection()?.query_row(
            "SELECT COUNT(*) FROM agent_message_deliveries WHERE delivery_id=?1",
            [match task.recovery.as_ref().unwrap() {
                TaskRecoverySpec::AgentMessageWait {
                    delivery_id: Some(id),
                    ..
                } => id,
                _ => unreachable!(),
            }],
            |r| r.get::<_, usize>(0)
        )?,
        1
    );
    host.shutdown().await?;
    Ok(())
}
