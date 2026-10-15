use super::*;
use crate::runtime::{
    resident_input_evidence::InputEvidenceScope,
    tests::support::{context_config, tempdir},
};
fn prepared(scope: InputEvidenceScope, task: TaskRecord) -> Result<PreparedInputTransition> {
    let command = TaskTransitionCommand {
        agent_id: scope.agent_id.clone(),
        task,
        agent_deletion: None,
        task_result_settlement: None,
        queue_entry: None,
        work_items: Vec::new(),
        expected_wait_conditions: Vec::new(),
        wait_conditions: Vec::new(),
        agent_state: None,
        message_evidence: Vec::new(),
        audit_events: Vec::new(),
        index_changes: Vec::new(),
        notify_scheduler: false,
        commit_on_idempotent: false,
        fault: None,
    };
    PreparedInputTransition::task(
        scope,
        crate::ids::audit_event_id(),
        Utc::now(),
        PreparedTaskTransition::new(command, Default::default(), false, None, None)?,
    )
}
#[tokio::test]
async fn task_workspace_loan_is_accounting_only_for_its_original_closed_metadata_variant(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "physical-task-loan-owner",
        home.path().into(),
        context_config(),
    )?;
    let at = runtime.now();
    let task = TaskRecord {
        id: crate::ids::task_id(),
        agent_id: runtime.instance_agent_id().into(),
        kind: TaskKind::WorktreeSubagentTask,
        status: TaskStatus::Queued,
        created_at: at,
        updated_at: at,
        parent_message_id: None,
        work_item_id: None,
        summary: None,
        detail: Some(serde_json::json!({"workspace_mode":"worktree"})),
        recovery: None,
    };
    runtime
        .persist_task_transition(&task, "physical_task_loan_actual_initial_ack")
        .await?;
    let scope = InputEvidenceScope {
        agent_id: task.agent_id.clone(),
        identity_incarnation: runtime.instance_incarnation(),
    };
    let mut updated = task.clone();
    updated.status = TaskStatus::Running;
    updated.updated_at = at + chrono::Duration::milliseconds(1);
    updated.detail = Some(
        serde_json::json!({"workspace_mode":"worktree","worktree":{"worktree_path":home.path().join("linked"),"worktree_branch":"task-linked"}}),
    );
    let loan =
        runtime.prepare_original_workspace_request_material(&(&updated.id, "task-linked"))?;
    let weak = Arc::downgrade(&loan);
    let before = runtime.runtime_db().tasks().latest(&task.id)?.unwrap();
    let ordinary = prepared(scope.clone(), updated.clone())?;
    assert!(
        ordinary
            .with_original_workspace_request_material(
                loan.clone(),
                runtime.instance_owner_epoch(),
                &runtime.inner.input_evidence_budget
            )
            .is_err(),
        "standalone setter must not become generic"
    );
    let original = prepared(scope.clone(), updated.clone())?;
    let descriptor = original.descriptor().clone();
    let with = original.with_original_task_workspace_request_material(
        loan.clone(),
        runtime.instance_owner_epoch(),
        &runtime.inner.input_evidence_budget,
    )?;
    assert_eq!(
        with.descriptor(),
        &descriptor,
        "accounting cannot change original nonce/digest/clock"
    );
    assert!(Arc::ptr_eq(
        with.original_workspace_request_material().unwrap(),
        &loan
    ));
    let mut non_worktree = updated.clone();
    non_worktree.kind = TaskKind::CommandTask;
    assert!(prepared(scope.clone(), non_worktree)?
        .with_original_task_workspace_request_material(
            loan.clone(),
            runtime.instance_owner_epoch(),
            &runtime.inner.input_evidence_budget
        )
        .is_err());
    let mut no_metadata = updated.clone();
    no_metadata.detail = None;
    let no_metadata = prepared(scope.clone(), no_metadata)?;
    let no_metadata_descriptor = no_metadata.descriptor().clone();
    let no_metadata = no_metadata.with_original_task_workspace_request_material(
        loan.clone(),
        runtime.instance_owner_epoch(),
        &runtime.inner.input_evidence_budget,
    )?;
    assert_eq!(
        no_metadata.descriptor(),
        &no_metadata_descriptor,
        "accounting cannot add a payload eligibility predicate"
    );
    drop(no_metadata);
    assert!(prepared(scope.clone(), updated.clone())?
        .with_original_task_workspace_request_material(
            loan.clone(),
            runtime.instance_owner_epoch() + 1,
            &runtime.inner.input_evidence_budget
        )
        .is_err());
    let other = RuntimeHandle::new_isolated_resource_fixture(
        "physical-task-other-owner",
        home.path().join("other"),
        context_config(),
    )?;
    assert!(prepared(scope.clone(), updated.clone())?
        .with_original_task_workspace_request_material(
            loan.clone(),
            runtime.instance_owner_epoch(),
            &other.inner.input_evidence_budget
        )
        .is_err());
    let mut other_scope = scope;
    other_scope.identity_incarnation += 1;
    assert!(prepared(other_scope, updated)?
        .with_original_task_workspace_request_material(
            loan.clone(),
            runtime.instance_owner_epoch(),
            &runtime.inner.input_evidence_budget
        )
        .is_err());
    assert_eq!(
        runtime.runtime_db().tasks().latest(&task.id)?.unwrap(),
        before,
        "setter grants no SQL or Task update by itself"
    );
    drop(loan);
    assert!(weak.upgrade().is_some());
    drop(with);
    assert!(weak.upgrade().is_none());
    Ok(())
}
