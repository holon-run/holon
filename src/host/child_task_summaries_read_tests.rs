use super::*;
use crate::runtime::tests::support::{host_backed_test_runtime, persist_task_transition};
use crate::types::{TaskKind, TaskRecord, TaskRecoverySpec, TaskStatus};

#[tokio::test]
async fn loaded_child_summary_reads_task_count_state_and_observability_without_body_sql(
) -> Result<()> {
    let (_home, host, parent) = host_backed_test_runtime().await;
    let parent_id = parent.agent_state().await?.id;
    let now = Utc::now();
    let parent_task = TaskRecord {
        id: crate::ids::task_id(),
        agent_id: parent_id.clone(),
        kind: TaskKind::ChildAgentTask,
        status: TaskStatus::Running,
        created_at: now,
        updated_at: now,
        parent_message_id: None,
        work_item_id: None,
        summary: Some("real child summary source".into()),
        detail: Some(json!({"workspace_mode":"inherit"})),
        recovery: Some(TaskRecoverySpec::ChildAgentTask {
            summary: "real child summary source".into(),
            prompt: "accepted child".into(),
            authority_class: crate::types::AuthorityClass::OperatorInstruction,
            workspace_mode: crate::types::ChildAgentWorkspaceMode::Inherit,
            lifecycle_disposition: crate::types::AgentLifecycleDisposition::Retain,
        }),
    };
    persist_task_transition(&parent, &parent_task, "child_summary_parent_ack").await?;
    let child_identity = host
        .create_child_identity(
            &parent_id,
            &parent_task,
            None,
            &host.agent_data_dir(&parent_id),
        )
        .await?;
    let child = host.get_or_create_agent(&child_identity.agent_id).await?;
    let task = TaskRecord {
        id: crate::ids::task_id(),
        agent_id: child_identity.agent_id.clone(),
        kind: TaskKind::AgentMessageWait,
        status: TaskStatus::Running,
        created_at: now,
        updated_at: now,
        parent_message_id: None,
        work_item_id: None,
        summary: Some("actual active child wait".into()),
        detail: Some(json!({"target_agent_id":parent_id,"business_completion":false})),
        recovery: Some(TaskRecoverySpec::AgentMessageWait {
            summary: "actual active child wait".into(),
            message: "original request".into(),
            target_agent_id: parent_id.clone(),
            authority_class: crate::types::AuthorityClass::RuntimeInstruction,
            delivery_id: None,
            after_delivery_rowid: None,
            reply_deadline_at: None,
            requester_incarnation: Some(child.instance_incarnation()),
        }),
    };
    persist_task_transition(&child, &task, "child_summary_active_task_ack").await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if child.agent_state().await?.status == crate::types::AgentStatus::Asleep {
                break Ok::<(), anyhow::Error>(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .context("child fixture did not reach its real idle sleep")??;
    let before = host.child_agent_summaries(&parent_id).await?;
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].identity.agent_id, child_identity.agent_id);
    assert_eq!(before[0].active_task_count, 1);
    assert_eq!(before[0].pending, 0);
    assert_eq!(before[0].current_run_id, None);
    host.runtime_db().transaction(|tx| {
        // Identity inventory remains its original Host adapter. Only the loaded
        // per-child Task/State body reads under test are denied here.
        tx.execute_batch(
            "ALTER TABLE tasks RENAME TO loaded_child_tasks_forbidden;
            ALTER TABLE agent_states RENAME TO loaded_child_states_forbidden;",
        )?;
        Ok(())
    })?;
    let repeated = host.child_agent_summaries(&parent_id).await?;
    assert_eq!(repeated, before);
    assert!(host
        .try_get_loaded_runtime(&child_identity.agent_id)
        .await
        .is_some());
    // Explicitly unprepared Task view cannot turn into a cold Storage fallback.
    let original = child.replace_task_domain_for_reader_test(None).await?;
    let error = host.child_agent_summaries(&parent_id).await.unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<crate::runtime::resident_task::TaskReadError>(),
            Some(crate::runtime::resident_task::TaskReadError::IncompleteActive)
        ),
        "{error:#}"
    );
    child.replace_task_domain_for_reader_test(original).await?;
    assert_eq!(host.child_agent_summaries(&parent_id).await?, before);
    host.runtime_db().transaction(|tx| {
        tx.execute_batch(
            "ALTER TABLE loaded_child_tasks_forbidden RENAME TO tasks;
            ALTER TABLE loaded_child_states_forbidden RENAME TO agent_states;",
        )?;
        Ok(())
    })?;
    host.shutdown().await?;
    Ok(())
}
