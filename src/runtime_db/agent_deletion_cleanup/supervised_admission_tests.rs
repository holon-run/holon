use super::*;
use crate::{
    runtime::{
        resident_input_evidence::InputEvidenceScope,
        resident_task::{TaskCaptureRequest, TaskFamily, TaskKey},
    },
    runtime_db::{
        agent_relations::supervised_creation_records,
        evidence::append_message_tx,
        input_mutation_receipts::{
            canonical_command_digest, InputMutationReceipt, InputMutationResult,
            InputMutationScopeOutcome, PreparedInputMutation,
        },
        repositories::upsert_task_tx,
        task_snapshot,
        transitions::{
            InputMutationExecution, PreparedInputTransition, PreparedQueueTransition,
            PreparedSupervisedInvocationAdmission, QueueMutation, QueueOperation,
            QueueTransitionCommand,
        },
    },
    types::{
        AdmissionContext, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility,
        AuthorityClass, MessageBody, MessageDeliverySurface, MessageEnvelope, MessageKind,
        MessageOrigin, Priority, TaskKind, TaskRecord, TaskStatus,
    },
};
use serde_json::json;
use std::collections::BTreeSet;

struct Fixture {
    _dir: tempfile::TempDir,
    db: RuntimeDb,
    receipt: InputMutationReceipt,
    task: TaskRecord,
    result: MessageEnvelope,
}

fn write_parent_task(db: &RuntimeDb, task: &TaskRecord) -> Result<()> {
    let scope = InputEvidenceScope {
        agent_id: task.agent_id.clone(),
        identity_incarnation: 1,
    };
    db.transaction(|tx| {
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &task.id)]);
        let proof = task_snapshot::prepare_task_write_tx(tx, &scope, &keys)?;
        upsert_task_tx(tx, task)?;
        task_snapshot::register_task_writes_tx(
            tx,
            &proof,
            &keys,
            &AgentState::new(&task.agent_id),
            &task_snapshot::TaskDomainLoadRequest {
                recovery_at: None,
                query: Default::default(),
                capture: TaskCaptureRequest::current(),
                byte_limit: 64 * 1024 * 1024,
                record_limit: 65_536,
            },
        )?;
        Ok(())
    })
}

fn fixture(source_turn: Option<&str>, terminal: bool) -> Result<Fixture> {
    let dir = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        dir.path().join("runtime.sqlite"),
        dir.path().join("runtime.lock"),
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
    db.agent_states()
        .upsert_for_fixture(&AgentState::new("parent"))?;
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
    write_parent_task(&db, &task)?;
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
    db.agent_identities().create_with_relations(
        &child,
        &supervised_creation_records(&child, &parent.agent_id, &task.id, None),
    )?;
    db.agent_states()
        .upsert_for_fixture(&AgentState::new("child"))?;
    let mut message = MessageEnvelope::new(
        "child",
        MessageKind::InternalFollowup,
        MessageOrigin::Task {
            task_id: task.id.clone(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "original supervised admission".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::RuntimeSystem,
        AdmissionContext::RuntimeOwned,
    );
    message.turn_id = source_turn.map(String::from);
    message.metadata = Some(json!({
        "parent_agent_id": "parent", "child_agent_id": "child",
        "delegated_task_id": task.id, "supervision_task_id": task.id,
        "parent_supervised": true,
    }));
    message.normalize_admission_fields();
    let child_scope = InputEvidenceScope {
        agent_id: child.agent_id,
        identity_incarnation: child.incarnation,
    };
    let parent_scope = InputEvidenceScope {
        agent_id: parent.agent_id,
        identity_incarnation: parent.incarnation,
    };
    let mut queue = PreparedQueueTransition::new(QueueTransitionCommand {
        agent_id: "child".into(),
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
    });
    queue.supervised_invocation = Some(PreparedSupervisedInvocationAdmission {
        parent_scope: parent_scope.clone(),
        parent_task: task.clone(),
        child_incarnation: child_scope.identity_incarnation,
    });
    let prepared = PreparedInputTransition::queue(
        child_scope.clone(),
        "supervised-admission".into(),
        now,
        queue,
    )?
    .with_scope_group(vec![parent_scope, child_scope.clone()])?;
    let InputMutationExecution::Committed { receipt, value } = db
        .transitions_in_scope(&child_scope)
        .commit_prepared_transition(&prepared)?
    else {
        bail!("original admission must commit")
    };
    assert!(value.applied);
    assert_eq!(value.effects.related_task_deltas.len(), 1);
    assert!(matches!(
        receipt.outcome.result,
        InputMutationResult::SupervisedInvocationAdmission { .. }
    ));
    assert!(matches!(
        db.transitions_in_scope(&child_scope)
            .commit_prepared_transition(&prepared)?,
        InputMutationExecution::Replayed { .. }
    ));
    let mut task = db.tasks().latest(&task.id)?.unwrap();
    if terminal {
        task.status = TaskStatus::Completed;
        task.updated_at += chrono::Duration::seconds(1);
        write_parent_task(&db, &task)?;
    }
    let mut result = MessageEnvelope::new(
        "parent",
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: task.id.clone(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "parent-owned result must survive child purge".into(),
        },
    );
    result.task_id = Some(task.id.clone());
    // This execution Turn is deliberately not the original admission source.
    result.turn_id = Some("result-execution-turn".into());
    db.transaction(|tx| {
        append_message_tx(tx, &result)?;
        Ok(())
    })?;
    let result = db.messages().by_id(Some("parent"), &result.id)?.unwrap();
    Ok(Fixture {
        _dir: dir,
        db,
        receipt,
        task,
        result,
    })
}

fn fence_and_settle(db: &RuntimeDb, agent: &str) -> Result<()> {
    let identity = db.agent_identities().latest(agent)?.unwrap();
    db.agent_deletions()
        .begin(agent, identity.revision, "test", true)?;
    settle_execution(db, agent, identity.incarnation, Utc::now())
}

// Tampered fixtures keep every normalized column and checksum coherent unless
// the test explicitly corrupts one. Rejection must not rely on a stale digest.
fn rewrite_receipt(db: &RuntimeDb, receipt: &InputMutationReceipt) -> Result<()> {
    let digest = canonical_command_digest(&json!({
        "prepared": receipt.prepared,
        "outcome": receipt.outcome,
        "committed_at": receipt.committed_at,
    }))?;
    db.transaction(|tx| {
        assert_eq!(
            tx.execute(
                "UPDATE input_mutation_receipts
                 SET prepared_json=?2,scopes_json=?3,scope_digest=?4,
                     outcome_json=?5,result_digest=?6 WHERE command_nonce=?1",
                params![
                    receipt.prepared.command_nonce(),
                    serde_json::to_string(&receipt.prepared)?,
                    serde_json::to_string(receipt.prepared.scopes())?,
                    receipt.prepared.scope_digest(),
                    serde_json::to_string(&receipt.outcome)?,
                    digest,
                ],
            )?,
            1
        );
        Ok(())
    })
}

fn assert_rejected_without_partial_purge(db: &RuntimeDb, agent: &str) -> Result<()> {
    let snapshot = || {
        db.transaction(|tx| {
            let mut rows = Vec::new();
            for (table, predicate) in private_history_selections_tx(tx)? {
                let mut statement = tx.prepare(&format!(
                    "SELECT * FROM {} WHERE {predicate}",
                    quoted(table)
                ))?;
                let columns = statement.column_count();
                let mut values = statement
                    .query_map([agent], |row| {
                        (0..columns)
                            .map(|index| row.get::<_, rusqlite::types::Value>(index))
                            .collect::<rusqlite::Result<Vec<_>>>()
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?
                    .into_iter()
                    .map(|row| format!("{row:?}"))
                    .collect::<Vec<_>>();
                values.sort();
                rows.push((table.to_owned(), values));
            }
            Ok(rows)
        })
    };
    let before = snapshot()?;
    assert!(
        format!("{:#}", export_private_history(db, agent, 1).unwrap_err())
            .contains("shared command history")
    );
    assert!(format!(
        "{:#}",
        purge_private_history(db, agent, 1, Utc::now()).unwrap_err()
    )
    .contains("shared command history"));
    assert_eq!(snapshot()?, before);
    db.transaction(|tx| {
        assert!(!proof_tx(tx, agent)?.unwrap().purged);
        Ok(())
    })
}

#[test]
fn supervised_admission_child_purge_preserves_parent_task_and_result() -> Result<()> {
    for source_turn in [None, Some("parent-task-source-turn")] {
        let fixture = fixture(source_turn, true)?;
        assert_eq!(fixture.task.kind, TaskKind::ActorInvocation);
        assert!(!fixture.task.is_child_agent_task());
        fence_and_settle(&fixture.db, "child")?;
        let export = export_private_history(&fixture.db, "child", 1)?;
        assert!(serde_json::to_string(&export)?.contains("supervised-admission"));
        purge_private_history(&fixture.db, "child", 1, Utc::now())?;
        assert_private_history_cleared(&fixture.db, "child")?;
        assert_eq!(
            fixture.db.tasks().latest(&fixture.task.id)?,
            Some(fixture.task)
        );
        assert_eq!(
            fixture
                .db
                .messages()
                .by_id(Some("parent"), &fixture.result.id)?,
            Some(fixture.result)
        );
        fixture.db.transaction(|tx| {
            assert!(!exists(
                tx,
                "SELECT EXISTS(SELECT 1 FROM input_mutation_receipts WHERE command_nonce=?1)",
                fixture.receipt.prepared.command_nonce(),
            )?);
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn supervised_admission_parent_first_purge_fails_closed() -> Result<()> {
    let fixture = fixture(Some("parent-task-source-turn"), true)?;
    fence_and_settle(&fixture.db, "parent")?;
    assert_rejected_without_partial_purge(&fixture.db, "parent")?;
    assert!(fixture.db.tasks().latest(&fixture.task.id)?.is_some());
    assert!(fixture
        .db
        .messages()
        .by_id(Some("parent"), &fixture.result.id)?
        .is_some());
    Ok(())
}

#[test]
fn supervised_admission_invalid_shared_ownership_fails_closed() -> Result<()> {
    for case in [
        "checksum",
        "source_message",
        "source_turn",
        "child_incarnation",
        "parent_incarnation",
        "third_scope",
        "unknown_queue",
        "wrong_task",
        "wrong_task_kind",
        "nonterminal_task",
    ] {
        let mut fixture = fixture(Some("parent-task-source-turn"), case != "nonterminal_task")?;
        let receipt = &mut fixture.receipt;
        let InputMutationResult::SupervisedInvocationAdmission { ack } =
            &mut receipt.outcome.result
        else {
            unreachable!()
        };
        match case {
            "source_message" => ack.source_message_id = "another-source".into(),
            "source_turn" => ack.source_turn_id = Some("result-execution-turn".into()),
            "wrong_task" => ack.parent_task_id = "another-task".into(),
            "wrong_task_kind" => {
                let mut task = fixture.task.clone();
                task.kind = TaskKind::CommandTask;
                fixture.db.transaction(|tx| {
                    assert_eq!(
                        tx.execute(
                            "UPDATE tasks SET kind=?1,payload_json=?2 WHERE task_id=?3",
                            params![task.kind.as_str(), serde_json::to_string(&task)?, task.id],
                        )?,
                        1
                    );
                    Ok(())
                })?;
            }
            "child_incarnation" | "parent_incarnation" | "third_scope" => {
                let mut scopes = receipt.prepared.scopes().to_vec();
                if case == "third_scope" {
                    let scope = InputEvidenceScope {
                        agent_id: "third".into(),
                        identity_incarnation: 1,
                    };
                    scopes.push(scope.clone());
                    receipt.outcome.scopes.push(InputMutationScopeOutcome {
                        scope,
                        install_keys: Default::default(),
                        audit_events: Vec::new(),
                        notify_memory_index: false,
                        notify_scheduler: false,
                    });
                } else {
                    let scope = if case == "child_incarnation" {
                        &mut ack.child_scope
                    } else {
                        &mut ack.parent_scope
                    };
                    scope.identity_incarnation += 1;
                    for item in &mut scopes {
                        if item.agent_id == scope.agent_id {
                            item.identity_incarnation = scope.identity_incarnation;
                        }
                    }
                    for item in &mut receipt.outcome.scopes {
                        if item.scope.agent_id == scope.agent_id {
                            item.scope = scope.clone();
                        }
                    }
                }
                receipt.prepared = PreparedInputMutation::new(
                    receipt.prepared.family(),
                    receipt.prepared.command_nonce().into(),
                    scopes,
                    receipt.prepared.command_digest().into(),
                    receipt.prepared.prepared_at(),
                )?;
                receipt.outcome.scopes.sort_by(|left, right| {
                    (&left.scope.agent_id, left.scope.identity_incarnation)
                        .cmp(&(&right.scope.agent_id, right.scope.identity_incarnation))
                });
            }
            "unknown_queue" => receipt.outcome.result = InputMutationResult::Transition,
            _ => {}
        }
        rewrite_receipt(&fixture.db, receipt)?;
        if case == "checksum" {
            fixture.db.transaction(|tx| {
                tx.execute(
                    "UPDATE input_mutation_receipts SET result_digest=?1",
                    ["0".repeat(64)],
                )?;
                Ok(())
            })?;
        }
        fence_and_settle(&fixture.db, "child")?;
        assert_rejected_without_partial_purge(&fixture.db, "child")
            .with_context(|| format!("tampered supervised admission: {case}"))?;
    }
    Ok(())
}
