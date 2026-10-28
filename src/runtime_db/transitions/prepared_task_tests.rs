use super::*;
use crate::runtime_db::input_mutation_receipts::{
    with_fresh_input_mutation_resolution, InputMutationResolution, InputTaskDeletionResult,
    OriginalInputMutationAttempt,
};
use crate::types::{AgentRegistryStatus, TaskKind, TaskStatus};

fn task_record(f: &Fixture, status: TaskStatus) -> TaskRecord {
    TaskRecord {
        id: "task:original-child".into(),
        agent_id: f.scope.agent_id.clone(),
        kind: TaskKind::ChildAgentTask,
        status,
        created_at: f.at,
        updated_at: f.at,
        parent_message_id: None,
        work_item_id: None,
        summary: Some("original child result".into()),
        detail: Some(serde_json::json!({"child_agent_id":"child:original-task"})),
        recovery: None,
    }
}
fn task_command(f: &Fixture) -> TaskTransitionCommand {
    TaskTransitionCommand {
        agent_id: f.scope.agent_id.clone(),
        expected_owner_incarnation: f.scope.identity_incarnation,
        task: task_record(f, TaskStatus::Completed),
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
        notify_scheduler: true,
        commit_on_idempotent: false,
        fault: None,
    }
}
fn create_child(f: &Fixture) -> Result<AgentIdentityRecord> {
    let mut identity = AgentIdentityRecord::new(
        "child:original-task",
        AgentKind::Child,
        AgentVisibility::Private,
        AgentOwnership::ParentSupervised,
        AgentProfilePreset::PrivateChild,
        Some(f.scope.agent_id.clone()),
        Some("task:original-child".into()),
    )
    .with_lineage_parent_agent_id(Some(f.scope.agent_id.clone()));
    identity.created_at = "2026-10-01T00:00:00Z".parse()?;
    identity.updated_at = identity.created_at;
    f.db.agent_identities().create_with_relations(
        &identity,
        &crate::runtime_db::agent_relations::supervised_creation_records(
            &identity,
            &f.scope.agent_id,
            "task:original-child",
            None,
        ),
    )?;
    Ok(identity)
}
fn child_bundle(
    f: &Fixture,
    child: AgentIdentityRecord,
    nonce: &str,
    command: TaskTransitionCommand,
) -> Result<PreparedInputTransition> {
    let mut command = command;
    command.agent_deletion = Some(AgentDeletionRequest {
        agent_id: child.agent_id.clone(),
        admission: repositories::AgentDeletionAdmission::TerminalEphemeralChild {
            parent_agent_id: f.scope.agent_id.clone(),
            task_id: command.task.id.clone(),
        },
        requested_by: "original-task-settlement".into(),
        cascade_private_children: false,
    });
    PreparedInputTransition::task(
        f.scope.clone(),
        nonce.into(),
        f.at,
        PreparedTaskTransition::new(
            command,
            Default::default(),
            true,
            Some(child),
            Some("deletion:original-task".into()),
        )?,
    )
}
fn task_rows(f: &Fixture) -> Result<BTreeMap<String, Vec<String>>> {
    let mut rows = namespace_rows(f)?;
    let connection = f.db.connection()?;
    for table in [
        "tasks",
        "agent_deletion_jobs",
        "agent_lineages",
        "agent_supervisions",
        "agent_durability_records",
        "agent_lifecycle_attachment_records",
    ] {
        // Table names are the actual canonical relation tables, not JSON-only
        // fixture state. A rollback must preserve their entire rows as well.
        let mut statement = connection.prepare(&format!("SELECT * FROM {table}"))?;
        let columns = statement.column_count();
        let mut values = statement
            .query_map([], |row| {
                (0..columns)
                    .map(|column| row.get::<_, rusqlite::types::Value>(column))
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map(|values| format!("{values:?}"))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        values.sort();
        rows.insert(table.into(), values);
    }
    Ok(rows)
}
fn original_deletion(receipt: &InputMutationReceipt) -> &InputTaskDeletionResult {
    let InputMutationResult::Task {
        task_id,
        deletion: Some(deletion),
        ..
    } = &receipt.outcome.result
    else {
        panic!("Task receipt must return its original child-deletion ACK");
    };
    assert_eq!(task_id, "task:original-child");
    deletion
}

#[test]
fn task_child_whole_receipt_returns_exact_ack_and_replay_keeps_later_identity() -> Result<()> {
    let f = Fixture::new()?;
    let child = create_child(&f)?;
    let prepared = child_bundle(&f, child.clone(), "task-original", task_command(&f))?;
    assert_eq!(prepared.descriptor().scopes().len(), 2);
    assert!(prepared.descriptor().scopes().contains(&f.scope));
    let child_scope = prepared.task_deletion_scope().unwrap().clone();
    let attempt = OriginalInputMutationAttempt::new(prepared.descriptor().clone());
    let InputMutationExecution::Committed { value, receipt } =
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&prepared)?
    else {
        panic!("first Task must commit");
    };
    let original = original_deletion(&receipt);
    assert!(original.created);
    assert_eq!(original.identity.status, AgentRegistryStatus::Deleting);
    assert_eq!(original.identity.incarnation, child.incarnation);
    assert_eq!(original.job.deletion_id, "deletion:original-task");
    assert_eq!(original.job.created_at, f.at);
    assert_eq!(
        value.effects.agent_identities,
        vec![original.identity.clone()]
    );
    assert_eq!(
        f.db.agent_identities().latest(&child.agent_id)?,
        Some(original.identity.clone())
    );
    assert_eq!(
        f.db.agent_deletions().latest_for_agent(&child.agent_id)?,
        Some(original.job.clone())
    );
    assert_eq!(
        receipt
            .outcome
            .scopes
            .iter()
            .map(|outcome| &outcome.scope)
            .collect::<Vec<_>>(),
        prepared.descriptor().scopes().iter().collect::<Vec<_>>()
    );
    assert!(receipt
        .outcome
        .scopes
        .iter()
        .find(|outcome| outcome.scope == child_scope)
        .unwrap()
        .audit_events
        .is_empty());
    attempt.finish_original_attempt();
    with_fresh_input_mutation_resolution(&mut f.db.connection()?, &attempt, |_, resolution| {
        assert!(
            matches!(resolution, InputMutationResolution::Present(ref actual) if actual == &receipt)
        );
        Ok(())
    })?;
    let mut later = original.identity.clone();
    later.revision += 1;
    later.updated_at += chrono::Duration::milliseconds(1);
    f.db.transaction(|tx| upsert_agent_identity_tx(tx, &later))?;
    let before = task_rows(&f)?;
    let InputMutationExecution::Replayed {
        receipt: replay, ..
    } =
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&prepared)?
    else {
        panic!("receipt must precede stale expected child");
    };
    assert_eq!(replay, receipt);
    assert_eq!(original_deletion(&replay).identity, original.identity);
    assert_eq!(
        f.db.agent_identities().latest(&child.agent_id)?,
        Some(later)
    );
    assert_eq!(
        task_rows(&f)?,
        before,
        "old ACK is never current identity installation authority"
    );
    Ok(())
}

#[test]
fn task_child_fault_rolls_back_parent_queue_work_child_job_audit_and_receipt() -> Result<()> {
    let f = Fixture::new()?;
    let child = create_child(&f)?;
    commit_original_wait(&f, "task-rollback-active-wait")?;
    let queue = f.queue()?;
    let mut command = task_command(&f);
    command.queue_entry = Some(queue_mutation_record(&queue.command.mutation).clone());
    command.message_evidence = queue.command.message_evidence;
    command.agent_state = queue.command.agent_state;
    command
        .work_items
        .push(original_work_command(&f, None).mutation);
    command.audit_events.push(AuditEvent::legacy(
        "task_completed",
        serde_json::json!({"task_id":command.task.id}),
    ));
    command.fault = Some(TransitionFaultPoint::AfterCanonicalWrites);
    let prepared = child_bundle(&f, child.clone(), "task-rollback", command)?;
    let attempt = OriginalInputMutationAttempt::new(prepared.descriptor().clone());
    let before = task_rows(&f)?;
    let error =
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&prepared)
            .unwrap_err();
    assert!(
        format!("{error:#}").contains("AfterCanonicalWrites"),
        "{error:#}"
    );
    attempt.finish_original_attempt();
    assert_eq!(task_rows(&f)?, before);
    assert_eq!(
        f.db.agent_identities().latest(&child.agent_id)?,
        Some(child)
    );
    with_fresh_input_mutation_resolution(&mut f.db.connection()?, &attempt, |_, resolution| {
        assert!(matches!(resolution, InputMutationResolution::Absent));
        Ok(())
    })?;
    Ok(())
}

#[test]
fn task_group_child_incarnation_and_pending_body_are_frozen_before_sql() -> Result<()> {
    let f = Fixture::new()?;
    let child = create_child(&f)?;
    let prepared = child_bundle(&f, child.clone(), "task-group", task_command(&f))?;
    assert!(prepared.with_scope_group(vec![f.scope.clone()]).is_err());
    let mut command = task_command(&f);
    let queue = f.queue()?;
    command.queue_entry = Some(queue_mutation_record(&queue.command.mutation).clone());
    command.message_evidence = queue.command.message_evidence;
    command.agent_state = queue.command.agent_state;
    let prepared = child_bundle(&f, child.clone(), "task-pending", command.clone())?
        .with_scope_group(vec![
            f.scope.clone(),
            InputEvidenceScope {
                agent_id: child.agent_id.clone(),
                identity_incarnation: child.incarnation,
            },
            absent_secondary(),
        ])?;
    assert_eq!(
        prepared.pending_messages()?,
        command.message_evidence.iter().collect::<Vec<_>>()
    );
    let missing = child_bundle(
        &f,
        child.clone(),
        "task-missing-body",
        TaskTransitionCommand {
            message_evidence: Vec::new(),
            ..command
        },
    )?;
    assert!(missing.pending_messages().is_err());
    // Conflicting restoration is not an ordinary identity mutation. It tests
    // that the original group cannot write through to a successor incarnation.
    let mut successor = child.clone();
    successor.incarnation += 1;
    successor.revision += 1;
    f.db.transaction(|tx| {
        assert_eq!(
            tx.execute(
                "UPDATE agent_identities SET payload_json=?1 WHERE agent_id=?2",
                params![serde_json::to_string(&successor)?, successor.agent_id]
            )?,
            1
        );
        Ok(())
    })?;
    let before = task_rows(&f)?;
    let error =
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&prepared)
            .unwrap_err();
    assert!(
        format!("{error:#}").contains("original current scope"),
        "{error:#}"
    );
    assert_eq!(task_rows(&f)?, before);
    assert_eq!(f.receipt_count()?, 0);
    Ok(())
}

#[test]
fn task_idempotent_child_result_is_still_a_new_receipt_and_group_authorization() -> Result<()> {
    let f = Fixture::new()?;
    let child = create_child(&f)?;
    let original = child_bundle(&f, child.clone(), "first-task", task_command(&f))?;
    let InputMutationExecution::Committed { receipt: first, .. } =
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&original)?
    else {
        panic!("first Task must commit");
    };
    let current = original_deletion(&first).identity.clone();
    let again = child_bundle(&f, current.clone(), "second-task", task_command(&f))?;
    let InputMutationExecution::Committed {
        value,
        receipt: second,
    } =
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&again)?
    else {
        panic!("new original no-op still needs a receipt");
    };
    assert!(!value.applied);
    assert!(!second.outcome.applied);
    assert!(!original_deletion(&second).created);
    assert_eq!(
        original_deletion(&second).job,
        original_deletion(&first).job
    );
    assert_eq!(
        f.db.agent_identities().latest(&child.agent_id)?,
        Some(current)
    );
    assert_eq!(f.receipt_count()?, 2);
    Ok(())
}

#[test]
fn task_final_wait_footprint_and_trigger_audit_are_in_the_same_original_receipt() -> Result<()> {
    use crate::runtime::resident_work::{CanonicalWorkRecord, WorkFamily, WorkKey};
    let f = Fixture::new()?;
    let mut original = original_wait_command(&f, None)?;
    original.wait_conditions[0].id = "task-old-wait".into();
    original.wait_conditions[0].wake_sources = vec![crate::types::WakeSource::OperatorInput];
    let original = PreparedInputTransition::wait(
        f.scope.clone(),
        "receipt:task-old-wait".into(),
        f.at,
        original,
        Default::default(),
        None,
    )?;
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition(&original)?;
    let queue = f.queue()?;
    let mut command = task_command(&f);
    command.queue_entry = Some(queue_mutation_record(&queue.command.mutation).clone());
    command.message_evidence = queue.command.message_evidence;
    command.agent_state = queue.command.agent_state;
    let mut newer = original_wait_command(&f, None)?.wait_conditions.remove(0);
    newer.id = "task-new-wait".into();
    command.wait_conditions.push(newer);
    let prepared = PreparedInputTransition::task(
        f.scope.clone(),
        "task-final-waits".into(),
        f.at,
        PreparedTaskTransition::new(command, Default::default(), true, None, None)?,
    )?;
    let InputMutationExecution::Committed { value, receipt } =
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&prepared)?
    else {
        panic!("first Task must commit");
    };
    let delta = value
        .effects
        .work_delta
        .as_ref()
        .context("Task Work finalizer omitted")?;
    assert_eq!(
        delta.coverage.unresolved_wait_ids,
        BTreeSet::from(["task-new-wait".into()])
    );
    for (id, status) in [
        ("task-old-wait", WaitConditionStatus::Cancelled),
        ("task-new-wait", WaitConditionStatus::Active),
    ] {
        let actual = delta
            .records
            .iter()
            .find(|record| record.key == WorkKey::new(WorkFamily::Wait, id))
            .context("implicit Wait omitted")?;
        let CanonicalWorkRecord::Wait(wait) = &actual.record else {
            panic!("expected Wait");
        };
        assert_eq!(wait.status, status);
        assert!(receipt.outcome.scopes[0]
            .install_keys
            .wait_conditions
            .contains(id));
    }
    let event_id = format!("audit:wait-triggered:{}", value.effects.messages[0].id);
    let notification = receipt.outcome.scopes[0]
        .audit_events
        .iter()
        .find(|event| event.id == event_id)
        .context("original trigger audit missing from receipt")?;
    assert_eq!(notification.created_at, f.at);
    assert!(notification.event_seq > 0);
    assert_eq!(
        notification
            .data
            .get("wait_condition_id")
            .and_then(serde_json::Value::as_str),
        Some("task-old-wait"),
        "the audit must describe the original wait that was actually triggered"
    );
    assert!(value.effects.audit_events.contains(notification));
    let before = task_rows(&f)?;
    assert!(matches!(
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&prepared)?,
        InputMutationExecution::Replayed { .. }
    ));
    assert_eq!(task_rows(&f)?, before);
    Ok(())
}

#[test]
fn deleting_parent_can_finish_existing_task_but_cannot_admit_new_task() -> Result<()> {
    let f = Fixture::new()?;
    let mut command = task_command(&f);
    command.task.status = TaskStatus::Running;
    let prepared = PreparedInputTransition::task(
        f.scope.clone(),
        "existing-task".into(),
        f.at,
        PreparedTaskTransition::new(command.clone(), Default::default(), true, None, None)?,
    )?;
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition(&prepared)?;
    f.deleting()?;
    command.task.status = TaskStatus::Cancelled;
    command.task.updated_at += chrono::Duration::milliseconds(1);
    let cleanup = PreparedInputTransition::task(
        f.scope.clone(),
        "cleanup-task".into(),
        f.at,
        PreparedTaskTransition::new(command.clone(), Default::default(), true, None, None)?,
    )?;
    let InputMutationExecution::Committed { value, .. } =
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&cleanup)?
    else {
        panic!("original accepted Task cleanup must commit");
    };
    assert_eq!(value.effects.tasks[0].status, TaskStatus::Cancelled);
    command.task.id = "task:new-after-deleting".into();
    command.task.status = TaskStatus::Running;
    let invalid = PreparedInputTransition::task(
        f.scope.clone(),
        "new-task-denied".into(),
        f.at,
        PreparedTaskTransition::new(command, Default::default(), true, None, None)?,
    )?;
    let before = task_rows(&f)?;
    assert!(f
        .db
        .transitions_in_scope(&f.scope)
        .commit_prepared_transition(&invalid)
        .is_err());
    assert_eq!(task_rows(&f)?, before);
    Ok(())
}
