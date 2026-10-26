//! Original-transaction footprint fixtures, without claiming Work qualification.
use super::*;
use crate::types::{
    AdmissionContext, AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset,
    AgentVisibility, AuthorityClass, BriefKind, CompletionReportRequirement, CompletionReportState,
    MessageBody, MessageDeliverySurface, MessageKind, MessageOrigin, Priority, TaskKind,
    TaskStatus, ToolExecutionStatus, WaitConditionKind, WaitConditionStatus, WakeSource,
    WorkItemCompletionIntent,
};
use chrono::{DateTime, Duration};
use tempfile::TempDir;

struct Fixture {
    _directory: TempDir,
    db: RuntimeDb,
    at: DateTime<Utc>,
    state: AgentState,
}
impl Fixture {
    fn new() -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            directory.path().join("state/runtime.sqlite"),
            directory.path().join("state/runtime.lock"),
        )?;
        let at = "2026-10-01T00:00:00Z".parse()?;
        let identity = AgentIdentityRecord::new(
            "agent-wait",
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        db.agent_identities().upsert(&identity)?;
        let state = AgentState::new("agent-wait");
        db.agent_states().upsert_for_fixture(&state)?;
        for (id, index) in [("turn-old", 1), ("turn-new", 2)] {
            let mut turn = TurnRecord::new("agent-wait", id, index);
            turn.created_at = at;
            db.transaction(|tx| upsert_turn_record_tx(tx, &turn).map(|_| ()))?;
        }
        Ok(Self {
            _directory: directory,
            db,
            at,
            state,
        })
    }
    fn work(&self, id: &str) -> Result<WorkItemRecord> {
        let mut work = WorkItemRecord::new("agent-wait", "wait footprint", WorkItemState::Open);
        work.id = id.into();
        work.created_at = self.at;
        work.updated_at = self.at;
        self.db.work_items().insert_new(&work)?;
        Ok(work)
    }
    fn wait(&self, id: &str, owner: &str, turn: &str, seconds: i64) -> WaitConditionRecord {
        WaitConditionRecord {
            id: id.into(),
            agent_id: "agent-wait".into(),
            work_item_id: Some(owner.into()),
            status: WaitConditionStatus::Active,
            kind: WaitConditionKind::Task,
            source: Some("footprint-test".into()),
            subject_ref: Some("task-wait".into()),
            waiting_for: "task result".into(),
            wake_sources: vec![WakeSource::TaskResult {
                task_id: "task-wait".into(),
            }],
            continuation: None,
            created_at: self.at + Duration::seconds(seconds),
            updated_at: self.at + Duration::seconds(seconds),
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
            turn_id: Some(turn.into()),
            trigger_message_id: None,
            triggered_at: None,
        }
    }
    fn wait_command(&self, waits: Vec<WaitConditionRecord>) -> WaitTransitionCommand {
        WaitTransitionCommand {
            agent_id: "agent-wait".into(),
            work_items: Vec::new(),
            expected_wait_conditions: Vec::new(),
            wait_conditions: waits,
            timer_wake: None,
            task_result_admission: None,
            agent_state: None,
            audit_events: Vec::new(),
            index_changes: Vec::new(),
            notify_scheduler: false,
            fault: None,
        }
    }
    fn task_command(&self, waits: Vec<WaitConditionRecord>) -> TaskTransitionCommand {
        TaskTransitionCommand {
            agent_id: "agent-wait".into(),
            expected_owner_incarnation: 1,
            task: TaskRecord {
                id: "task-wait".into(),
                agent_id: "agent-wait".into(),
                kind: TaskKind::CommandTask,
                status: TaskStatus::Completed,
                created_at: self.at,
                updated_at: self.at + Duration::seconds(2),
                parent_message_id: None,
                work_item_id: None,
                summary: Some("done".into()),
                detail: None,
                recovery: None,
            },
            agent_deletion: None,
            task_result_settlement: None,
            queue_entry: None,
            work_items: Vec::new(),
            expected_wait_conditions: Vec::new(),
            wait_conditions: waits,
            agent_state: None,
            message_evidence: Vec::new(),
            audit_events: Vec::new(),
            index_changes: Vec::new(),
            notify_scheduler: false,
            commit_on_idempotent: false,
            fault: None,
        }
    }
    fn queue_command(&self, id: &str) -> QueueTransitionCommand {
        let mut message = MessageEnvelope::new(
            "agent-wait",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "new input".into(),
            },
        );
        message.id = id.into();
        message.created_at = self.at + Duration::seconds(3);
        let mut admitted_state = self.state.clone();
        admitted_state.pending = 1;
        QueueTransitionCommand {
            agent_id: "agent-wait".into(),
            operation: QueueOperation::Admit,
            mutation: QueueMutation::Upsert(QueueEntryRecord {
                message_id: id.into(),
                agent_id: "agent-wait".into(),
                priority: Priority::Normal,
                status: QueueEntryStatus::Queued,
                created_at: message.created_at,
                updated_at: message.created_at,
            }),
            scheduler_claim_work_item: None,
            agent_state: Some(AgentStateMutation {
                expected: Some(Box::new(self.state.clone())),
                record: Box::new(admitted_state),
            }),
            message_evidence: vec![message],
            transcript_entries: Vec::new(),
            turn_record: None,
            audit_events: Vec::new(),
            notify_scheduler: false,
            fault: None,
            brief_evidence: Vec::new(),
        }
    }
    fn row(&self, id: &str) -> Result<WaitConditionRecord> {
        self.db
            .connection()?
            .query_row(
                "SELECT payload_json FROM wait_conditions WHERE wait_condition_id=?1",
                [id],
                |row| row.get::<_, String>(0),
            )
            .map_err(Into::into)
            .and_then(|payload| Ok(serde_json::from_str(&payload)?))
    }
    fn sql_snapshot(&self) -> Result<BTreeMap<&'static str, Vec<String>>> {
        let connection = self.db.connection()?;
        let mut snapshot = BTreeMap::new();
        for table in [
            "wait_conditions",
            "work_items",
            "agent_states",
            "turn_records",
            "tasks",
            "messages",
            "queue_entries",
            "tool_executions",
            "audit_events",
            "runtime_index_outbox",
            "conversation_turn_revisions",
            "conversation_source_revisions",
            "conversation_input_assignments",
            "runtime_sequences",
        ] {
            // Read all columns as SQLite typed values, including normalized revisions.
            let mut statement =
                connection.prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))?;
            let column_count = statement.column_count();
            let rows = statement
                .query_map([], |row| {
                    let values = (0..column_count)
                        .map(|index| row.get::<_, rusqlite::types::Value>(index))
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    Ok(format!("{values:?}"))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            snapshot.insert(table, rows);
        }
        Ok(snapshot)
    }
}

fn assert_footprint(commit: &TransitionCommit, waits: &[&str], turns: &[&str]) {
    assert!(commit.applied);
    assert_eq!(
        commit.effects.wait_condition_writes.wait_ids,
        waits.iter().map(|id| (*id).to_owned()).collect()
    );
    assert_eq!(
        commit.effects.wait_condition_writes.turn_ids,
        turns.iter().map(|id| (*id).to_owned()).collect()
    );
}

#[test]
fn focus_wait_and_task_original_transactions_return_implicit_cancelled_wait_ids() -> Result<()> {
    for family in ["focus", "wait", "task"] {
        let f = Fixture::new()?;
        let work = f.work("owner")?;
        let old = f.wait("old", &work.id, "turn-old", 0);
        let new = f.wait("new", &work.id, "turn-new", 1);
        f.db.wait_conditions().upsert(&old)?;
        let turn_payload_before: String = f.db.connection()?.query_row(
            "SELECT payload_json FROM turn_records WHERE turn_id='turn-old'",
            [],
            |r| r.get(0),
        )?;
        let revision_before: u64 = f.db.connection()?.query_row(
            "SELECT detail_revision FROM conversation_turn_revisions WHERE turn_id='turn-old'",
            [],
            |r| r.get(0),
        )?;
        let commit = match family {
            "focus" => {
                f.db.transitions()
                    .commit_work_item_focus(&WorkItemFocusTransitionCommand {
                        agent_id: "agent-wait".into(),
                        work_items: Vec::new(),
                        wait_conditions: vec![new.clone()],
                        active_continuation_expectations: None,
                        continuation_expectations: Vec::new(),
                        continuations: Vec::new(),
                        agent_state: AgentStateMutation {
                            expected: Some(Box::new(f.state.clone())),
                            record: Box::new(f.state.clone()),
                        },
                        brief_evidence: Vec::new(),
                        audit_events: Vec::new(),
                        index_changes: Vec::new(),
                        notify_scheduler: false,
                        fault: None,
                    })?
            }
            "wait" => {
                f.db.transitions()
                    .commit_wait(&f.wait_command(vec![new.clone()]))?
            }
            "task" => {
                f.db.transitions()
                    .commit_task(&f.task_command(vec![new.clone()]))?
            }
            _ => unreachable!(),
        };
        assert_footprint(&commit, &["old", "new"], &["turn-old", "turn-new"]);
        assert!(commit.effects.wait_condition_writes.primary_applied);
        assert_eq!(f.row("old")?.status, WaitConditionStatus::Cancelled);
        assert_eq!(f.row("new")?, new);
        let (turn_payload_after, revision_after): (String,u64) = f.db.connection()?.query_row(
            "SELECT t.payload_json,r.detail_revision FROM turn_records t JOIN conversation_turn_revisions r USING(turn_id)
             WHERE t.turn_id='turn-old'", [], |r| Ok((r.get(0)?,r.get(1)?)))?;
        assert_eq!(
            turn_payload_before, turn_payload_after,
            "Wait bookkeeping does not rewrite canonical Turn body"
        );
        assert!(revision_after > revision_before);
    }
    Ok(())
}

#[test]
fn queue_transaction_merges_transition_registration_extra_and_completion_wait_footprints(
) -> Result<()> {
    let f = Fixture::new()?;
    let mut originals = Vec::new();
    for owner in ["transition", "registration", "extra", "completion"] {
        f.work(owner)?;
        let wait = f.wait(&format!("old-{owner}"), owner, "turn-old", 0);
        f.db.wait_conditions().upsert(&wait)?;
        originals.push(wait);
    }
    let mut resolved = originals[0].clone();
    resolved.status = WaitConditionStatus::Resolved;
    resolved.resolved_at = Some(f.at + Duration::seconds(1));
    resolved.updated_at = f.at + Duration::seconds(1);
    let transition = QueueWaitTransition {
        expected: WaitConditionExpectation {
            id: originals[0].id.clone(),
            agent_id: "agent-wait".into(),
            status: originals[0].status.clone(),
            updated_at: originals[0].updated_at,
        },
        record: resolved,
        work_item: None,
        index_changes: Vec::new(),
    };
    let registration = f.wait_command(vec![f.wait(
        "new-registration",
        "registration",
        "turn-new",
        2,
    )]);
    let extra = f.wait("new-extra", "extra", "turn-new", 2);
    let mut cancelled = originals[3].clone();
    cancelled.status = WaitConditionStatus::Cancelled;
    cancelled.updated_at = f.at + Duration::seconds(2);
    cancelled.cancelled_at = Some(cancelled.updated_at);
    let mut brief = BriefRecord::new("agent-wait", BriefKind::Result, "completed", None, None);
    brief.id = "completion-result".into();
    brief.work_item_id = Some("completion".into());
    brief.created_at = f.at + Duration::seconds(2);
    let mut completed = f.db.work_items().latest("completion")?.unwrap();
    completed.revision += 1;
    completed.state = WorkItemState::Completed;
    completed.updated_at = f.at + Duration::seconds(2);
    completed.result_brief_id = Some("completion-result".into());
    completed.completion_intent = Some(WorkItemCompletionIntent {
        work_item_id: completed.id.clone(),
        source_activation_id: None,
        source_message_id: None,
        source_turn_id: None,
        expected_work_revision: 1,
        report_requirement: CompletionReportRequirement::Required,
        report_state: CompletionReportState::Bound,
        result_brief_id: completed.result_brief_id.clone(),
        created_at: f.at,
        updated_at: completed.updated_at,
    });
    let completion = CompletionTransition {
        requires_execution_continuation: false,
        work_items: vec![WorkItemMutation::Update {
            record: completed,
            expected_revision: 1,
        }],
        wait_conditions: vec![cancelled],
        continuation_expectations: Vec::new(),
        continuations: Vec::new(),
        tool_execution: ToolExecutionRecord {
            id: "complete-tool".into(),
            agent_id: "agent-wait".into(),
            work_item_id: Some("completion".into()),
            turn_index: 0,
            turn_id: None,
            tool_name: crate::tool::names::COMPLETE_WORK_ITEM.into(),
            created_at: f.at,
            completed_at: Some(f.at + Duration::seconds(2)),
            duration_ms: 1,
            authority_class: AuthorityClass::RuntimeInstruction,
            status: ToolExecutionStatus::Success,
            input: serde_json::json!({"work_item_id":"completion"}),
            output: serde_json::json!({"status":"completed"}),
            summary: "completed".into(),
            invocation_surface: None,
        },
        index_changes: Vec::new(),
    };
    let mut command = f.queue_command("nested-queue");
    command.brief_evidence.push(brief);
    let commit = f.db.transitions().commit_queue_transaction_full(
        &command,
        &ExecutionProtocolTransition::default(),
        Some(&transition),
        None,
        Some(&completion),
        Some(&registration),
        &[],
        &[extra],
        None,
        DeliverySynchronization::Required,
        None,
        None,
    )?;
    assert_footprint(
        &commit,
        &[
            "old-transition",
            "old-registration",
            "new-registration",
            "old-extra",
            "new-extra",
            "old-completion",
        ],
        &["turn-old", "turn-new"],
    );
    assert_eq!(
        f.row("old-transition")?.status,
        WaitConditionStatus::Resolved
    );
    assert_eq!(
        f.row("old-registration")?.status,
        WaitConditionStatus::Cancelled
    );
    assert_eq!(f.row("old-extra")?.status, WaitConditionStatus::Cancelled);
    assert_eq!(
        f.row("old-completion")?.status,
        WaitConditionStatus::Cancelled
    );
    assert_eq!(f.db.queue_entries().latest_all()?.len(), 1);
    Ok(())
}

#[test]
fn task_trigger_and_later_cancel_publish_one_final_wait_footprint() -> Result<()> {
    let f = Fixture::new()?;
    f.work("owner")?;
    let wait = f.wait("task-wait", "owner", "turn-old", 0);
    f.db.wait_conditions().upsert(&wait)?;
    let mut message = MessageEnvelope::new(
        "agent-wait",
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: "task-wait".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "done".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::TaskRejoin,
        AdmissionContext::RuntimeOwned,
    );
    message.id = "task-result".into();
    message.task_id = Some("task-wait".into());
    message.work_item_id = Some("owner".into());
    message.created_at = f.at + Duration::seconds(2);
    let queue = QueueEntryRecord {
        message_id: message.id.clone(),
        agent_id: "agent-wait".into(),
        priority: message.priority.clone(),
        status: QueueEntryStatus::Queued,
        created_at: message.created_at,
        updated_at: message.created_at,
    };
    let mut cancelled = wait.clone();
    cancelled.mark_triggered(&message.id, queue.updated_at);
    cancelled.status = WaitConditionStatus::Cancelled;
    cancelled.updated_at += Duration::seconds(1);
    cancelled.cancelled_at = Some(cancelled.updated_at);
    let mut command = f.task_command(vec![cancelled.clone()]);
    command.queue_entry = Some(queue);
    command.message_evidence = vec![message.clone()];
    let mut admitted_state = f.state.clone();
    admitted_state.pending = 1;
    command.agent_state = Some(AgentStateMutation {
        expected: Some(Box::new(f.state.clone())),
        record: Box::new(admitted_state),
    });
    let commit = f.db.transitions().commit_task(&command)?;
    assert_footprint(&commit, &["task-wait"], &["turn-old"]);
    assert_eq!(
        f.row("task-wait")?,
        cancelled,
        "only the final Cancelled fact is eligible for later Work finalization"
    );
    assert_eq!(
        f.db.messages()
            .by_id(Some("agent-wait"), &message.id)?
            .unwrap()
            .message_seq,
        Some(1)
    );
    let events = f.db.audit_events().recent(Some("agent-wait"), 10)?;
    assert!(events
        .iter()
        .any(|event| event.kind == "wait_condition_triggered"));
    Ok(())
}

#[test]
fn idempotent_wait_and_work_without_waits_return_empty_wait_footprints() -> Result<()> {
    let f = Fixture::new()?;
    let work = f.work("owner")?;
    let wait = f.wait("same", "owner", "turn-old", 0);
    f.db.wait_conditions().upsert(&wait)?;
    let before = f.sql_snapshot()?;
    let noop =
        f.db.transitions()
            .commit_wait(&f.wait_command(vec![wait]))?;
    assert!(!noop.applied);
    assert!(noop.effects.wait_condition_writes.wait_ids.is_empty());
    assert!(noop.effects.wait_condition_writes.turn_ids.is_empty());
    assert_eq!(before, f.sql_snapshot()?);
    let mut updated = work;
    updated.revision += 1;
    updated.updated_at += Duration::seconds(1);
    updated.blocked_by = Some("still waiting".into());
    let commit =
        f.db.transitions()
            .commit_work_item(&WorkItemTransitionCommand {
                agent_id: "agent-wait".into(),
                mutation: WorkItemMutation::Update {
                    record: updated,
                    expected_revision: 1,
                },
                agent_state: None,
                brief_evidence: Vec::new(),
                audit_events: Vec::new(),
                index_changes: Vec::new(),
                notify_scheduler: false,
                fault: None,
            })?;
    assert!(commit.applied);
    assert!(commit.effects.wait_condition_writes.wait_ids.is_empty());
    assert!(commit.effects.wait_condition_writes.turn_ids.is_empty());
    Ok(())
}

#[test]
fn implicit_wait_replacement_rolls_back_the_whole_original_transaction_at_each_fault() -> Result<()>
{
    for fault in [
        TransitionFaultPoint::AfterCanonicalWrites,
        TransitionFaultPoint::AfterAuditWrites,
        TransitionFaultPoint::BeforeCommit,
    ] {
        let f = Fixture::new()?;
        let work = f.work("owner")?;
        f.db.wait_conditions()
            .upsert(&f.wait("old", "owner", "turn-old", 0))?;
        let before = f.sql_snapshot()?;
        let mut command = f.wait_command(vec![f.wait("new", "owner", "turn-new", 2)]);
        let mut updated = work;
        updated.revision += 1;
        updated.updated_at += Duration::seconds(2);
        updated.blocked_by = Some("new wait".into());
        command.work_items = vec![WorkItemMutation::Update {
            record: updated,
            expected_revision: 1,
        }];
        command.audit_events = vec![AuditEvent::legacy("wait_replaced", serde_json::json!({}))];
        command.fault = Some(fault);
        let error = f.db.transitions().commit_wait(&command).unwrap_err();
        assert!(format!("{error:#}").contains("injected runtime transition fault"));
        assert_eq!(before, f.sql_snapshot()?, "{fault:?}");
    }
    Ok(())
}

#[test]
fn newer_unresolved_owner_preserves_the_original_transaction_failure() -> Result<()> {
    let f = Fixture::new()?;
    f.work("owner")?;
    f.db.wait_conditions()
        .upsert(&f.wait("newer", "owner", "turn-new", 10))?;
    let before = f.sql_snapshot()?;
    let command = f.wait_command(vec![f.wait("older", "owner", "turn-old", 1)]);
    let error = f.db.transitions().commit_wait(&command).unwrap_err();
    assert!(format!("{error:#}").contains("UNIQUE constraint failed"));
    assert_eq!(before, f.sql_snapshot()?);
    Ok(())
}

#[test]
fn borrowed_funnels_leave_commit_ownership_with_the_whole_outer_transaction() -> Result<()> {
    for family in ["focus", "work", "wait", "task"] {
        let f = Fixture::new()?;
        let work = f.work("owner")?;
        f.db.wait_conditions()
            .upsert(&f.wait("old", "owner", "turn-old", 0))?;
        let before = f.sql_snapshot()?;
        let error =
            f.db.transaction(|tx| -> Result<()> {
                let repository = f.db.transitions();
                let execution = ExecutionProtocolTransition::default();
                let new_wait = f.wait("new", "owner", "turn-new", 2);
                let commit = match family {
                    "focus" => repository.commit_work_item_focus_tx(
                        tx,
                        &WorkItemFocusTransitionCommand {
                            agent_id: "agent-wait".into(),
                            work_items: Vec::new(),
                            wait_conditions: vec![new_wait],
                            active_continuation_expectations: None,
                            continuation_expectations: Vec::new(),
                            continuations: Vec::new(),
                            agent_state: AgentStateMutation {
                                expected: Some(Box::new(f.state.clone())),
                                record: Box::new(f.state.clone()),
                            },
                            brief_evidence: Vec::new(),
                            audit_events: Vec::new(),
                            index_changes: Vec::new(),
                            notify_scheduler: false,
                            fault: None,
                        },
                        &execution,
                        false,
                        None,
                    )?,
                    "work" => {
                        let mut updated = work.clone();
                        updated.revision += 1;
                        updated.updated_at += Duration::seconds(2);
                        updated.blocked_by = Some("outer transaction".into());
                        repository.commit_work_item_tx(
                            tx,
                            &WorkItemTransitionCommand {
                                agent_id: "agent-wait".into(),
                                mutation: WorkItemMutation::Update {
                                    record: updated,
                                    expected_revision: 1,
                                },
                                agent_state: None,
                                brief_evidence: Vec::new(),
                                audit_events: Vec::new(),
                                index_changes: Vec::new(),
                                notify_scheduler: false,
                                fault: None,
                            },
                            &execution,
                            false,
                        )?
                    }
                    "wait" => repository.commit_wait_tx(
                        tx,
                        &f.wait_command(vec![new_wait]),
                        &execution,
                        None,
                    )?,
                    "task" => repository.commit_task_tx(
                        tx,
                        &f.task_command(vec![new_wait]),
                        &execution,
                        false,
                    )?,
                    _ => unreachable!(),
                };
                assert!(commit.applied, "{family}");
                append_audit_event_tx(
                    tx,
                    Some("agent-wait"),
                    &AuditEvent::legacy("outer_receipt_boundary", serde_json::json!({})),
                )?;
                anyhow::bail!("outer transaction intentionally rejected after original funnel")
            })
            .unwrap_err();
        assert!(format!("{error:#}")
            .contains("outer transaction intentionally rejected after original funnel"));
        assert_eq!(before, f.sql_snapshot()?, "{family}");
    }
    Ok(())
}
