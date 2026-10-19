use anyhow::{ensure, Context, Result};
use rusqlite::{OptionalExtension, Transaction};
use serde_json::json;

use super::{QueueMutation, QueueOperation, QueueTransitionCommand};
use crate::{
    domain::agent::{AgentCanonicalDurability, AgentLifecycleAttachment, AgentSupervisionState},
    runtime_db::{
        agent_relations::canonical_relations_from_connection,
        repositories::{decode_agent_identity_payload, decode_task_payload, is_active_task_status},
    },
    types::{
        AdmissionContext, AgentKind, AgentOwnership, AgentRegistryStatus, AgentVisibility,
        MessageDeliverySurface, MessageEnvelope, MessageKind, MessageOrigin, QueueEntryStatus,
        TaskKind, TaskRecord,
    },
};

// Cross-agent source binding must share the child message admission transaction.
pub(super) fn prepare_admission_tx(
    tx: &Transaction<'_>,
    command: &QueueTransitionCommand,
    expected: &TaskRecord,
    incarnation: u64,
) -> Result<Option<TaskRecord>> {
    let [message] = command.message_evidence.as_slice() else {
        anyhow::bail!("supervised_invocation_requires_one_message");
    };
    let QueueMutation::Upsert(queue) = &command.mutation else {
        anyhow::bail!("supervised_invocation_requires_queue_admission");
    };
    ensure!(
        command.operation == QueueOperation::Admit
            && queue.status == QueueEntryStatus::Queued
            && queue.agent_id == command.agent_id
            && message.agent_id == command.agent_id
            && queue.message_id == message.id
            && !message.id.trim().is_empty(),
        "supervised_invocation_invalid_admission"
    );
    let payload: String = tx
        .query_row(
            "SELECT payload_json FROM tasks WHERE task_id = ?1",
            [&expected.id],
            |row| row.get(0),
        )
        .optional()?
        .context("supervised_invocation_missing_parent_task")?;
    let current = decode_task_payload(&payload)?;
    ensure!(
        current.agent_id == expected.agent_id,
        "supervised_invocation_task_owner_mismatch"
    );
    ensure!(
        is_active_task_status(&current.status)
            && is_active_task_status(&expected.status)
            && (current.kind.is_child_agent() || current.kind == TaskKind::ActorInvocation),
        "supervised_invocation_parent_task_not_active_child"
    );
    let payload: String = tx
        .query_row(
            "SELECT payload_json FROM agent_identities WHERE agent_id = ?1",
            [&message.agent_id],
            |row| row.get(0),
        )
        .optional()?
        .context("supervised_invocation_missing_child")?;
    let child = decode_agent_identity_payload(&payload)?;
    ensure!(
        child.status == AgentRegistryStatus::Active
            && child.deleted_at.is_none()
            && child.kind == AgentKind::Child
            && child.visibility == AgentVisibility::Private
            && child.ownership() == AgentOwnership::ParentSupervised
            && child.agent_id != current.agent_id
            && child.incarnation == incarnation
            && child.parent_agent_id.as_deref() == Some(current.agent_id.as_str())
            && child.delegated_from_task_id.as_deref() == Some(current.id.as_str()),
        "supervised_invocation_child_identity_mismatch"
    );
    let relations = canonical_relations_from_connection(tx, &child.agent_id)?
        .context("supervised_invocation_missing_child_relations")?;
    // The initial task has no child target yet, so its legacy projection can
    // disagree until this transaction writes the binding. Check the actual axes.
    ensure!(
        relations.supervision.as_ref().is_some_and(|supervision| {
            supervision.supervisor_agent_id == current.agent_id
                && supervision.child_agent_id == child.agent_id
                && supervision.delegated_from_task_id.as_deref() == Some(current.id.as_str())
                && supervision.state == AgentSupervisionState::Active
        }) && relations.durability.as_ref().is_some_and(|durability| {
            durability.durability == AgentCanonicalDurability::Ephemeral
        }) && relations
            .lifecycle_attachment
            .as_ref()
            .is_some_and(|attachment| {
                attachment.attachment == AgentLifecycleAttachment::SupervisionAttached
            }),
        "supervised_invocation_child_supervision_mismatch"
    );
    let metadata = message
        .metadata
        .as_ref()
        .context("supervised_invocation_missing_metadata")?;
    ensure!(
        matches!(&message.origin, MessageOrigin::Task { task_id } if task_id == &current.id)
            && message.task_id.as_deref() == Some(current.id.as_str())
            && message.kind == MessageKind::InternalFollowup
            && message.delivery_surface == Some(MessageDeliverySurface::RuntimeSystem)
            && message.admission_context == Some(AdmissionContext::RuntimeOwned)
            && metadata.get("parent_agent_id").and_then(|v| v.as_str())
                == Some(current.agent_id.as_str())
            && metadata.get("child_agent_id").and_then(|v| v.as_str())
                == Some(child.agent_id.as_str())
            && metadata.get("delegated_task_id").and_then(|v| v.as_str())
                == Some(current.id.as_str())
            && metadata.get("supervision_task_id").and_then(|v| v.as_str())
                == Some(current.id.as_str())
            && metadata.get("parent_supervised").and_then(|v| v.as_bool()) == Some(true),
        "supervised_invocation_source_metadata_mismatch"
    );
    let mut bound = bind_source(expected, message, incarnation)?;
    let existing_source = current
        .detail
        .as_ref()
        .and_then(|v| v.get("source_message_id"));
    if let Some(existing_source) = existing_source {
        ensure!(
            existing_source.as_str() == Some(message.id.as_str()),
            "supervised_invocation_source_binding_conflict"
        );
        bound.updated_at = current.updated_at;
        ensure!(
            bound == current,
            "supervised_invocation_parent_task_changed"
        );
        let payload: String = tx
            .query_row(
                "SELECT payload_json FROM messages WHERE message_id = ?1 AND agent_id = ?2",
                rusqlite::params![message.id, message.agent_id],
                |row| row.get(0),
            )
            .optional()?
            .context("supervised_invocation_bound_message_missing")?;
        let persisted: MessageEnvelope = serde_json::from_str(&payload)?;
        let mut replay = message.clone();
        if replay.message_seq.is_none() {
            replay.message_seq = persisted.message_seq;
        }
        ensure!(
            persisted == replay,
            "supervised_invocation_message_conflict"
        );
        let queued: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM queue_entries WHERE message_id = ?1 AND agent_id = ?2)",
            rusqlite::params![message.id, message.agent_id],
            |row| row.get(0),
        )?;
        ensure!(queued, "supervised_invocation_bound_queue_missing");
        return Ok(None);
    }
    ensure!(
        current == *expected,
        "supervised_invocation_parent_task_changed"
    );
    let admitted: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM messages WHERE message_id = ?1)
             OR EXISTS(SELECT 1 FROM queue_entries WHERE message_id = ?1)",
        [&message.id],
        |row| row.get(0),
    )?;
    ensure!(!admitted, "supervised_invocation_source_already_admitted");
    bound.updated_at = std::cmp::max(bound.updated_at, queue.updated_at);
    Ok(Some(bound))
}

fn bind_source(
    expected: &TaskRecord,
    message: &MessageEnvelope,
    incarnation: u64,
) -> Result<TaskRecord> {
    let mut bound = expected.clone();
    let detail = bound.detail.get_or_insert_with(|| json!({}));
    let detail = detail
        .as_object_mut()
        .context("supervised_invocation_invalid_task_detail")?;
    for (key, value) in [
        ("source_message_id", json!(message.id)),
        ("source_turn_id", json!(message.turn_id)),
        ("child_agent_id", json!(message.agent_id)),
        (
            "target",
            json!({
                "kind": "child_agent",
                "agent_id": message.agent_id,
                "incarnation": incarnation,
            }),
        ),
    ] {
        if let Some(existing) = detail.get(key) {
            ensure!(
                existing == &value,
                "supervised_invocation_source_binding_conflict: {key}"
            );
        }
        detail.insert(key.into(), value);
    }
    Ok(bound)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        runtime_db::{
            agent_relations::supervised_creation_records, transitions::TransitionFaultPoint,
            RuntimeDb,
        },
        types::{
            AgentIdentityRecord, AgentProfilePreset, AuthorityClass, MessageBody, Priority,
            QueueEntryRecord, TaskStatus,
        },
    };
    use chrono::Utc;
    use tempfile::TempDir;

    fn fixture() -> Result<(TempDir, RuntimeDb, TaskRecord, QueueTransitionCommand)> {
        let dir = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            dir.path().join("state/runtime.sqlite"),
            dir.path().join("state/runtime.lock"),
        )?;
        let parent = AgentIdentityRecord::new(
            "parent",
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        db.agent_identities().upsert(&parent)?;
        let now = Utc::now();
        let task = TaskRecord {
            id: "invocation".into(),
            agent_id: parent.agent_id.clone(),
            kind: TaskKind::ActorInvocation,
            status: TaskStatus::Queued,
            created_at: now,
            updated_at: now,
            parent_message_id: None,
            work_item_id: None,
            summary: None,
            detail: Some(json!({"sentinel": "preserved"})),
            recovery: None,
        };
        db.tasks().upsert(&task)?;
        let child = AgentIdentityRecord::new(
            "child",
            AgentKind::Child,
            AgentVisibility::Private,
            AgentOwnership::ParentSupervised,
            AgentProfilePreset::PrivateChild,
            Some(parent.agent_id.clone()),
            Some(task.id.clone()),
        )
        .with_lineage_parent_agent_id(Some(parent.agent_id.clone()));
        let relations = supervised_creation_records(&child, &parent.agent_id, &task.id, None);
        db.agent_identities()
            .create_with_relations(&child, &relations)?;
        let mut message = MessageEnvelope::new(
            child.agent_id.clone(),
            MessageKind::InternalFollowup,
            MessageOrigin::Task {
                task_id: task.id.clone(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "supervised invocation".into(),
            },
        )
        .with_admission(
            MessageDeliverySurface::RuntimeSystem,
            AdmissionContext::RuntimeOwned,
        );
        message.turn_id = Some("source-turn".into());
        message.metadata = Some(json!({
            "parent_agent_id": parent.agent_id,
            "child_agent_id": child.agent_id,
            "delegated_task_id": task.id,
            "supervision_task_id": task.id,
            "parent_supervised": true,
        }));
        message.normalize_admission_fields();
        let command = QueueTransitionCommand {
            agent_id: child.agent_id,
            operation: QueueOperation::Admit,
            mutation: QueueMutation::Upsert(QueueEntryRecord {
                message_id: message.id.clone(),
                agent_id: message.agent_id.clone(),
                priority: message.priority.clone(),
                status: QueueEntryStatus::Queued,
                created_at: message.created_at,
                updated_at: message.created_at,
            }),
            scheduler_claim_work_item: None,
            agent_state: None,
            message_evidence: vec![message],
            transcript_entries: Vec::new(),
            turn_record: None,
            audit_events: Vec::new(),
            notify_scheduler: true,
            fault: None,
            brief_evidence: Vec::new(),
        };
        Ok((dir, db, task, command))
    }

    #[test]
    fn supervised_invocation_sources_match_exact_child_and_task_origin() -> Result<()> {
        let (_dir, db, task, command) = fixture()?;
        let source = &command.message_evidence[0];
        assert!(db
            .messages()
            .supervised_invocation_sources("child", &task.id)?
            .is_empty());
        db.messages().upsert(source)?;

        let mut other_child = source.clone();
        other_child.id = "other-child-source".into();
        other_child.agent_id = "parent".into();
        db.messages().upsert(&other_child)?;

        let mut other_task = source.clone();
        other_task.id = "other-task-source".into();
        other_task.origin = MessageOrigin::Task {
            task_id: "other-task".into(),
        };
        db.messages().upsert(&other_task)?;

        let mut other_origin = source.clone();
        other_origin.id = "other-origin-source".into();
        other_origin.origin = MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        };
        db.messages().upsert(&other_origin)?;

        let matches = db
            .messages()
            .supervised_invocation_sources("child", &task.id)?;
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id, source.id);
        for (child_id, task_id) in [
            ("missing-child", task.id.as_str()),
            ("child", "missing-task"),
            ("child", "' OR 1=1 --"),
        ] {
            assert!(db
                .messages()
                .supervised_invocation_sources(child_id, task_id)?
                .is_empty());
        }
        Ok(())
    }

    #[test]
    fn supervised_invocation_sources_preserve_ambiguity_with_bounded_results() -> Result<()> {
        let (_dir, db, task, command) = fixture()?;
        for index in 0..4 {
            let mut source = command.message_evidence[0].clone();
            source.id = format!("duplicate-source-{index}");
            db.messages().upsert(&source)?;
        }
        let matches = db
            .messages()
            .supervised_invocation_sources("child", &task.id)?;
        assert_eq!(matches.len(), 2);
        assert_ne!(matches[0].id, matches[1].id);
        Ok(())
    }

    #[test]
    fn supervised_invocation_admission_is_atomic_and_idempotent() -> Result<()> {
        let (_dir, db, task, command) = fixture()?;
        let source = &command.message_evidence[0];
        assert!(
            db.transitions()
                .commit_supervised_invocation_admission(&command, &task, 1)?
                .applied
        );
        let bound = db.tasks().latest(&task.id)?.unwrap();
        let detail = bound.detail.as_ref().unwrap();
        assert_eq!(detail["source_message_id"], source.id);
        assert_eq!(detail["source_turn_id"], "source-turn");
        assert_eq!(detail["child_agent_id"], "child");
        assert_eq!(detail["target"]["incarnation"], 1);
        assert_eq!(detail["sentinel"], "preserved");
        assert!(db.messages().by_id(Some("child"), &source.id)?.is_some());
        assert_eq!(
            db.queue_entries().latest(&source.id)?.unwrap().status,
            QueueEntryStatus::Queued
        );
        for snapshot in [&task, &bound] {
            assert!(
                !db.transitions()
                    .commit_supervised_invocation_admission(&command, snapshot, 1)?
                    .applied
            );
        }
        let mut processed = db.queue_entries().latest(&source.id)?.unwrap();
        processed.status = QueueEntryStatus::Processed;
        processed.updated_at = Utc::now();
        db.queue_entries().upsert(&processed)?;
        assert!(
            !db.transitions()
                .commit_supervised_invocation_admission(&command, &task, 1)?
                .applied
        );
        assert_eq!(
            db.queue_entries().latest(&source.id)?.unwrap().status,
            QueueEntryStatus::Processed
        );
        assert_eq!(db.tasks().latest(&task.id)?.unwrap(), bound);
        Ok(())
    }

    #[test]
    fn supervised_invocation_admission_rolls_back_binding_message_and_queue() -> Result<()> {
        for fault in [
            TransitionFaultPoint::AfterValidation,
            TransitionFaultPoint::AfterCanonicalWrites,
            TransitionFaultPoint::AfterAuditWrites,
            TransitionFaultPoint::BeforeCommit,
        ] {
            let (_dir, db, task, mut command) = fixture()?;
            command.fault = Some(fault);
            let error = db
                .transitions()
                .commit_supervised_invocation_admission(&command, &task, 1)
                .unwrap_err();
            assert!(error.to_string().contains("injected"), "{fault:?}: {error}");
            assert_eq!(db.tasks().latest(&task.id)?, Some(task.clone()));
            let source = &command.message_evidence[0];
            assert!(db.messages().by_id(None, &source.id)?.is_none());
            assert!(db.queue_entries().latest(&source.id)?.is_none());
            command.fault = None;
            assert!(
                db.transitions()
                    .commit_supervised_invocation_admission(&command, &task, 1)?
                    .applied
            );
        }
        Ok(())
    }

    #[test]
    fn supervised_invocation_rejects_binding_conflicts_and_wrong_owner() -> Result<()> {
        let (_dir, db, task, command) = fixture()?;
        let mut wrong_owner = task.clone();
        wrong_owner.agent_id = "other-parent".into();
        assert!(db
            .transitions()
            .commit_supervised_invocation_admission(&command, &wrong_owner, 1)
            .unwrap_err()
            .to_string()
            .contains("task_owner_mismatch"));
        assert!(db
            .transitions()
            .commit_supervised_invocation_admission(&command, &task, 2)
            .unwrap_err()
            .to_string()
            .contains("child_identity_mismatch"));
        let mut changed = task.clone();
        changed.summary = Some("not the admitted snapshot".into());
        assert!(db
            .transitions()
            .commit_supervised_invocation_admission(&command, &changed, 1)
            .unwrap_err()
            .to_string()
            .contains("parent_task_changed"));
        assert!(db
            .messages()
            .by_id(None, &command.message_evidence[0].id)?
            .is_none());
        db.transitions()
            .commit_supervised_invocation_admission(&command, &task, 1)?;
        let mut conflicting = command.clone();
        conflicting.message_evidence[0].id = "replacement-source".into();
        if let QueueMutation::Upsert(queue) = &mut conflicting.mutation {
            queue.message_id = "replacement-source".into();
        }
        assert!(db
            .transitions()
            .commit_supervised_invocation_admission(&conflicting, &task, 1)
            .unwrap_err()
            .to_string()
            .contains("source_binding_conflict"));
        assert!(db.messages().by_id(None, "replacement-source")?.is_none());
        let mut tampered = command.clone();
        tampered.message_evidence[0].body = MessageBody::Text {
            text: "same id, different instructions".into(),
        };
        assert!(db
            .transitions()
            .commit_supervised_invocation_admission(&tampered, &task, 1)
            .unwrap_err()
            .to_string()
            .contains("message_conflict"));
        Ok(())
    }

    #[test]
    fn supervised_invocation_rejects_forged_metadata_and_nonchild_targets() -> Result<()> {
        let (_dir, db, task, command) = fixture()?;
        for key in [
            "parent_agent_id",
            "child_agent_id",
            "delegated_task_id",
            "supervision_task_id",
            "parent_supervised",
        ] {
            let mut forged = command.clone();
            forged.message_evidence[0].metadata.as_mut().unwrap()[key] = json!("forged");
            assert!(
                db.transitions()
                    .commit_supervised_invocation_admission(&forged, &task, 1)
                    .unwrap_err()
                    .to_string()
                    .contains("source_metadata_mismatch"),
                "{key}"
            );
        }
        let mut wrong_origin = command.clone();
        wrong_origin.message_evidence[0].origin = MessageOrigin::Task {
            task_id: "other-task".into(),
        };
        assert!(db
            .transitions()
            .commit_supervised_invocation_admission(&wrong_origin, &task, 1)
            .unwrap_err()
            .to_string()
            .contains("source_metadata_mismatch"));
        let mut nonchild = command.clone();
        nonchild.agent_id = "parent".into();
        nonchild.message_evidence[0].agent_id = "parent".into();
        if let QueueMutation::Upsert(queue) = &mut nonchild.mutation {
            queue.agent_id = "parent".into();
        }
        assert!(db
            .transitions()
            .commit_supervised_invocation_admission(&nonchild, &task, 1)
            .unwrap_err()
            .to_string()
            .contains("child_identity_mismatch"));
        assert_eq!(db.tasks().latest(&task.id)?, Some(task));
        Ok(())
    }

    #[test]
    fn supervised_invocation_rejects_changed_supervision() -> Result<()> {
        let (_dir, db, task, command) = fixture()?;
        let mut supervision = db
            .agent_canonical_relations()
            .latest("child")?
            .unwrap()
            .supervision
            .unwrap();
        supervision.state = AgentSupervisionState::Closed;
        supervision.revision += 1;
        supervision.updated_at = Utc::now();
        db.agent_canonical_relations()
            .upsert_supervision(&supervision)?;
        assert!(db
            .transitions()
            .commit_supervised_invocation_admission(&command, &task, 1)
            .unwrap_err()
            .to_string()
            .contains("child_supervision_mismatch"));
        assert_eq!(db.tasks().latest(&task.id)?, Some(task));
        assert!(db
            .messages()
            .by_id(None, &command.message_evidence[0].id)?
            .is_none());
        Ok(())
    }

    #[test]
    fn supervised_invocation_binding_survives_task_updates_and_rejects_terminal_replay(
    ) -> Result<()> {
        let (_dir, db, task, command) = fixture()?;
        db.transitions()
            .commit_supervised_invocation_admission(&command, &task, 1)?;
        let bound = db.tasks().latest(&task.id)?.unwrap();
        for key in [
            "source_message_id",
            "source_turn_id",
            "child_agent_id",
            "target",
        ] {
            let mut changed = bound.clone();
            changed
                .detail
                .as_mut()
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(key);
            assert!(
                db.tasks()
                    .upsert(&changed)
                    .unwrap_err()
                    .to_string()
                    .contains("source_binding_conflict"),
                "{key}"
            );
        }
        let mut terminal = bound;
        terminal.status = TaskStatus::Completed;
        terminal.updated_at = Utc::now();
        db.tasks().upsert(&terminal)?;
        assert!(db
            .transitions()
            .commit_supervised_invocation_admission(&command, &task, 1)
            .unwrap_err()
            .to_string()
            .contains("parent_task_not_active_child"));
        assert_eq!(db.tasks().latest(&task.id)?, Some(terminal));
        Ok(())
    }
}
