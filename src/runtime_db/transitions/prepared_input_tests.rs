use super::*;
use crate::{
    runtime_db::{
        evidence::{upsert_agent_identity_tx, upsert_agent_state_tx},
        input_mutation_receipts::ReceiptError,
    },
    types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentState,
        AgentVisibility, AuthorityClass, MessageBody, MessageKind, MessageOrigin, Priority,
    },
};

struct Fixture {
    _directory: tempfile::TempDir,
    db: RuntimeDb,
    scope: InputEvidenceScope,
    at: chrono::DateTime<Utc>,
}
impl Fixture {
    fn new() -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            directory.path().join("runtime.sqlite"),
            directory.path().join("runtime.lock"),
        )?;
        let scope = InputEvidenceScope {
            agent_id: "prepared-input".into(),
            identity_incarnation: 1,
        };
        db.transaction(|tx| {
            upsert_agent_identity_tx(
                tx,
                &AgentIdentityRecord::new(
                    &scope.agent_id,
                    AgentKind::Named,
                    AgentVisibility::Public,
                    AgentOwnership::SelfOwned,
                    AgentProfilePreset::PublicNamed,
                    None,
                    None,
                ),
            )?;
            upsert_agent_state_tx(tx, &AgentState::new(&scope.agent_id))
        })?;
        Ok(Self {
            _directory: directory,
            db,
            scope,
            at: "2026-10-13T00:00:00.123456789Z".parse()?,
        })
    }
    fn state(&self) -> Result<AgentState> {
        self.db
            .agent_states()
            .latest(&self.scope.agent_id)?
            .context("fixture State missing")
    }
    fn posture(
        &self,
        state: AgentState,
        fault: Option<TransitionFaultPoint>,
    ) -> Result<AgentPostureTransitionCommand> {
        Ok(AgentPostureTransitionCommand {
            agent_id: self.scope.agent_id.clone(),
            expected_incarnation: Some(1),
            agent_state: AgentStateMutation {
                expected: Some(Box::new(self.state()?)),
                record: Box::new(state),
            },
            audit_events: Vec::new(),
            fault,
        })
    }
    fn queue(&self) -> Result<PreparedQueueTransition> {
        let mut message = MessageEnvelope::new(
            &self.scope.agent_id,
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "original frozen input".into(),
            },
        );
        message.id = "message:prepared-input".into();
        message.created_at = self.at;
        let mut state = self.state()?;
        state.pending = 1;
        Ok(PreparedQueueTransition::new(QueueTransitionCommand {
            agent_id: self.scope.agent_id.clone(),
            operation: QueueOperation::Admit,
            mutation: QueueMutation::Upsert(QueueEntryRecord {
                message_id: message.id.clone(),
                agent_id: self.scope.agent_id.clone(),
                priority: message.priority.clone(),
                status: QueueEntryStatus::Queued,
                created_at: self.at,
                updated_at: self.at,
            }),
            scheduler_claim_work_item: None,
            agent_state: Some(AgentStateMutation {
                expected: Some(Box::new(self.state()?)),
                record: Box::new(state),
            }),
            message_evidence: vec![message],
            transcript_entries: Vec::new(),
            turn_record: None,
            audit_events: Vec::new(),
            notify_scheduler: true,
            fault: None,
            brief_evidence: Vec::new(),
        }))
    }
    fn receipt_count(&self) -> Result<i64> {
        Ok(self.db.connection()?.query_row(
            "SELECT COUNT(*) FROM input_mutation_receipts",
            [],
            |r| r.get(0),
        )?)
    }
}

#[test]
fn posture_noop_has_committed_receipt_and_replays_before_later_occ() -> Result<()> {
    let f = Fixture::new()?;
    let original = f.state()?;
    let prepared = PreparedInputTransition::posture(
        f.scope.clone(),
        "noop-original".into(),
        f.at,
        f.posture(original.clone(), None)?,
        None,
    )?;
    let receipt = match f
        .db
        .transitions_in_scope(&f.scope)
        .commit_prepared_transition(&prepared)?
    {
        InputMutationExecution::Committed { value, receipt } => {
            assert!(!value.applied);
            assert!(!receipt.outcome.applied);
            receipt
        }
        _ => panic!("first no-op must commit its original receipt"),
    };
    let mut later = original;
    later.pending = 9;
    f.db.agent_states().upsert_for_fixture(&later)?;
    match f
        .db
        .transitions_in_scope(&f.scope)
        .commit_prepared_transition(&prepared)?
    {
        InputMutationExecution::Replayed { receipt: replay } => assert_eq!(replay, receipt),
        _ => panic!("stale original expected State must not run again"),
    }
    assert_eq!(f.state()?, later);
    assert_eq!(f.receipt_count()?, 1);
    Ok(())
}

#[test]
fn queue_receipt_follows_exact_count_and_replay_carries_no_old_installation() -> Result<()> {
    let f = Fixture::new()?;
    let prepared =
        PreparedInputTransition::queue(f.scope.clone(), "admit-original".into(), f.at, f.queue()?)?;
    let original = match f
        .db
        .transitions_in_scope(&f.scope)
        .commit_prepared_transition(&prepared)?
    {
        InputMutationExecution::Committed { value, receipt } => {
            assert!(value.applied);
            assert_eq!(
                value
                    .effects
                    .agent_state
                    .unwrap()
                    .record
                    .total_message_count,
                1
            );
            assert_eq!(value.effects.messages[0].message_seq, Some(1));
            assert_eq!(value.effects.queued_messages[0].message_seq, Some(1));
            assert!(receipt.outcome.scopes[0].notify_memory_index);
            receipt
        }
        _ => panic!("original queue commit missing"),
    };
    let mut later = f.state()?;
    later.pending = 4;
    f.db.agent_states().upsert_for_fixture(&later)?;
    match f
        .db
        .transitions_in_scope(&f.scope)
        .commit_prepared_transition(&prepared)?
    {
        InputMutationExecution::Replayed { receipt } => assert_eq!(receipt, original),
        _ => panic!("replay cannot return a stale TransitionCommit"),
    }
    assert_eq!(f.state()?, later);
    assert_eq!(f.receipt_count()?, 1);
    Ok(())
}

#[test]
fn started_posture_receipt_and_turn_assignment_roll_back_with_canonical_failure() -> Result<()> {
    let f = Fixture::new()?;
    let mut queue = f.queue()?;
    let message = queue.command.message_evidence.remove(0);
    let message =
        f.db.messages()
            .append_with_index_changes_in_scope(&f.scope, &message, &[])?
            .record;
    let before = f.state()?;
    let mut next = before.clone();
    next.current_turn_id = Some("turn:prepared".into());
    next.turn_index = 1;
    let mut turn = TurnRecord::new(&f.scope.agent_id, "turn:prepared", 1);
    turn.input_message_ids.push(message.id.clone());
    let failed = PreparedInputTransition::posture(
        f.scope.clone(),
        "started-failed".into(),
        f.at,
        f.posture(
            next.clone(),
            Some(TransitionFaultPoint::AfterCanonicalWrites),
        )?,
        Some(turn.clone()),
    )?;
    assert!(f
        .db
        .transitions_in_scope(&f.scope)
        .commit_prepared_transition(&failed)
        .is_err());
    assert_eq!(f.state()?, before);
    assert_eq!(f.receipt_count()?, 0);
    assert!(f
        .db
        .turn_records()
        .by_id(Some(&f.scope.agent_id), &turn.turn_id)?
        .is_none());
    let prepared = PreparedInputTransition::posture(
        f.scope.clone(),
        "started-original".into(),
        f.at,
        f.posture(next, None)?,
        Some(turn.clone()),
    )?;
    let InputMutationExecution::Committed { value, .. } =
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&prepared)?
    else {
        panic!("original started commit missing")
    };
    assert_eq!(value.effects.turns, vec![turn]);
    assert_eq!(
        value.effects.input_evidence_delta.message_turn_keys[&message.id],
        Some("turn:prepared".into())
    );
    assert_eq!(f.receipt_count()?, 1);
    Ok(())
}

#[test]
fn receipt_insert_rejection_rolls_back_entire_original_queue_transaction() -> Result<()> {
    let f = Fixture::new()?;
    let before = f.state()?;
    f.db.connection()?.execute_batch("CREATE TRIGGER reject_original_receipt BEFORE INSERT ON input_mutation_receipts BEGIN SELECT RAISE(IGNORE); END;")?;
    let prepared = PreparedInputTransition::queue(
        f.scope.clone(),
        "rejected-receipt".into(),
        f.at,
        f.queue()?,
    )?;
    assert!(f
        .db
        .transitions_in_scope(&f.scope)
        .commit_prepared_transition(&prepared)
        .is_err());
    assert_eq!(f.state()?, before);
    assert_eq!(f.receipt_count()?, 0);
    for table in [
        "messages",
        "queue_entries",
        "input_evidence_current",
        "runtime_index_outbox",
    ] {
        let count: i64 =
            f.db.connection()?
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?;
        assert_eq!(count, 0, "{table} escaped original rollback");
    }
    Ok(())
}

#[test]
fn prepared_digest_covers_queue_options_and_started_turn_but_excludes_fault() -> Result<()> {
    let f = Fixture::new()?;
    let first =
        PreparedInputTransition::queue(f.scope.clone(), "digest-queue".into(), f.at, f.queue()?)?;
    let mut options = f.queue()?;
    options.timer_wake_claim = Some(TimerWakeClaim {
        timer_id: "timer:original".into(),
        message_id: "message:prepared-input".into(),
        fire_count: 3,
    });
    let second =
        PreparedInputTransition::queue(f.scope.clone(), "digest-queue".into(), f.at, options)?;
    assert_ne!(
        first.descriptor().command_digest(),
        second.descriptor().command_digest()
    );
    let mut command = f.posture(f.state()?, None)?;
    let first = PreparedInputTransition::posture(
        f.scope.clone(),
        "digest-posture".into(),
        f.at,
        command.clone(),
        None,
    )?;
    command.fault = Some(TransitionFaultPoint::BeforeCommit);
    let second = PreparedInputTransition::posture(
        f.scope.clone(),
        "digest-posture".into(),
        f.at,
        command,
        None,
    )?;
    assert_eq!(first.descriptor(), second.descriptor());
    Ok(())
}

#[test]
fn same_nonce_changed_typed_command_is_rejected_instead_of_running_again() -> Result<()> {
    let f = Fixture::new()?;
    let first = PreparedInputTransition::posture(
        f.scope.clone(),
        "same-original".into(),
        f.at,
        f.posture(f.state()?, None)?,
        None,
    )?;
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition(&first)?;
    let mut next = f.state()?;
    next.pending = 2;
    let second = PreparedInputTransition::posture(
        f.scope.clone(),
        "same-original".into(),
        f.at,
        f.posture(next, None)?,
        None,
    )?;
    let error =
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&second)
            .unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<ReceiptError>(),
            Some(ReceiptError::CommandConflict)
        ),
        "{error:#}"
    );
    assert_eq!(f.state()?.pending, 0);
    assert_eq!(f.receipt_count()?, 1);
    Ok(())
}

#[test]
fn terminal_original_result_replays_after_later_canonical_posture() -> Result<()> {
    let f = Fixture::new()?;
    let mut running = f.state()?;
    running.current_turn_id = Some("turn:terminal".into());
    running.turn_index = 1;
    let turn = TurnRecord::new(&f.scope.agent_id, "turn:terminal", 1);
    let started = PreparedInputTransition::posture(
        f.scope.clone(),
        "terminal-start".into(),
        f.at,
        f.posture(running, None)?,
        Some(turn.clone()),
    )?;
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition(&started)?;
    let mut state = f.state()?;
    state.current_turn_id = None;
    let mut terminal = turn;
    terminal.terminal = Some(TurnTerminalSummary {
        kind: TurnTerminalKind::Completed,
        reason: None,
        no_brief_reason: None,
        completed_at: f.at,
        duration_ms: 0,
    });
    let prepared = PreparedInputTransition::terminal(
        f.scope.clone(),
        "terminal-original".into(),
        f.at,
        TurnTerminalTransitionCommand {
            agent_id: f.scope.agent_id.clone(),
            agent_state: AgentStateMutation {
                expected: Some(Box::new(f.state()?)),
                record: Box::new(state),
            },
            turn_record: terminal.clone(),
            terminal_tool_executions: Vec::new(),
            audit_events: Vec::new(),
            fault: None,
        },
    )?;
    assert!(matches!(
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&prepared)?,
        InputMutationExecution::Committed { .. }
    ));
    let mut later = f.state()?;
    later.pending = 8;
    f.db.agent_states().upsert_for_fixture(&later)?;
    assert!(matches!(
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&prepared)?,
        InputMutationExecution::Replayed { .. }
    ));
    assert_eq!(f.state()?, later);
    assert_eq!(
        f.db.turn_records()
            .by_id(Some(&f.scope.agent_id), &terminal.turn_id)?,
        Some(terminal)
    );
    Ok(())
}

#[test]
fn full_scope_group_receipt_is_sorted_and_keeps_primary_effects_only() -> Result<()> {
    let f = Fixture::new()?;
    let secondary = InputEvidenceScope {
        agent_id: "aaa-related-owner".into(),
        identity_incarnation: 1,
    };
    f.db.transaction(|tx| {
        upsert_agent_identity_tx(
            tx,
            &AgentIdentityRecord::new(
                &secondary.agent_id,
                AgentKind::Named,
                AgentVisibility::Public,
                AgentOwnership::SelfOwned,
                AgentProfilePreset::PublicNamed,
                None,
                None,
            ),
        )?;
        upsert_agent_state_tx(tx, &AgentState::new(&secondary.agent_id))
    })?;
    let prepared =
        PreparedInputTransition::queue(f.scope.clone(), "group-original".into(), f.at, f.queue()?)?;
    let digest = prepared.descriptor().command_digest().to_owned();
    let old_scope_digest = prepared.descriptor().scope_digest().to_owned();
    let prepared = prepared.with_scope_group(vec![f.scope.clone(), secondary.clone()])?;
    assert_eq!(prepared.descriptor().command_nonce(), "group-original");
    assert_eq!(prepared.descriptor().prepared_at(), f.at);
    assert_eq!(prepared.descriptor().command_digest(), digest);
    assert_ne!(prepared.descriptor().scope_digest(), old_scope_digest);
    assert_eq!(
        prepared.descriptor().scopes(),
        &[secondary.clone(), f.scope.clone()]
    );
    let receipt = match f
        .db
        .transitions_in_scope(&f.scope)
        .commit_prepared_transition(&prepared)?
    {
        InputMutationExecution::Committed { receipt, .. } => receipt,
        _ => panic!("original group missing"),
    };
    assert_eq!(receipt.outcome.scopes.len(), 2);
    assert_eq!(receipt.outcome.scopes[0].scope, secondary);
    assert!(receipt.outcome.scopes[0].audit_events.is_empty());
    assert!(!receipt.outcome.scopes[0].notify_memory_index);
    assert!(!receipt.outcome.scopes[0].notify_scheduler);
    assert!(receipt.outcome.scopes[1].notify_memory_index);
    assert!(receipt.outcome.scopes[1].notify_scheduler);
    assert!(matches!(
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&prepared)?,
        InputMutationExecution::Replayed { .. }
    ));
    f.db.transaction(|tx| {
        let mut identity = AgentIdentityRecord::new(
            &secondary.agent_id,
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        identity.incarnation = 2;
        upsert_agent_identity_tx(tx, &identity)
    })?;
    assert!(f
        .db
        .transitions_in_scope(&f.scope)
        .commit_prepared_transition(&prepared)
        .is_err());
    assert_eq!(f.receipt_count()?, 1);
    Ok(())
}

#[test]
fn scope_group_cannot_omit_primary_or_repeat_a_scope() -> Result<()> {
    let f = Fixture::new()?;
    let prepare = || {
        PreparedInputTransition::queue(f.scope.clone(), "invalid-group".into(), f.at, f.queue()?)
    };
    assert!(prepare()?.with_scope_group(Vec::new()).is_err());
    assert!(prepare()?
        .with_scope_group(vec![InputEvidenceScope {
            agent_id: "other".into(),
            identity_incarnation: 1
        }])
        .is_err());
    assert!(prepare()?
        .with_scope_group(vec![f.scope.clone(), f.scope.clone()])
        .is_err());
    assert_eq!(f.receipt_count()?, 0);
    Ok(())
}

fn pending_wait(agent: &str, message: MessageEnvelope) -> WaitTransitionCommand {
    WaitTransitionCommand {
        agent_id: agent.into(),
        work_items: Vec::new(),
        expected_wait_conditions: Vec::new(),
        wait_conditions: Vec::new(),
        timer_wake: None,
        task_result_admission: Some(WaitTaskResultAdmission {
            record: QueueEntryRecord {
                message_id: message.id.clone(),
                agent_id: message.agent_id.clone(),
                priority: message.priority.clone(),
                status: QueueEntryStatus::Queued,
                created_at: message.created_at,
                updated_at: message.created_at,
            },
            expected: None,
            message,
        }),
        agent_state: None,
        audit_events: Vec::new(),
        index_changes: Vec::new(),
        notify_scheduler: true,
        fault: None,
    }
}

#[test]
fn pending_messages_enumerates_queue_and_wait_frozen_bodies_once() -> Result<()> {
    let f = Fixture::new()?;
    let mut queue = f.queue()?;
    let primary = queue.command.message_evidence[0].clone();
    let mut task_result = primary.clone();
    task_result.id = "message:wait-result".into();
    queue.wait_registration = Some(pending_wait(&f.scope.agent_id, task_result.clone()));
    let prepared =
        PreparedInputTransition::queue(f.scope.clone(), "pending-original".into(), f.at, queue)?;
    let pending = prepared.pending_messages()?;
    assert_eq!(pending.len(), 2);
    assert_eq!(pending[0], &primary);
    assert_eq!(pending[1], &task_result);
    let mut queue = f.queue()?;
    queue.wait_registration = Some(pending_wait(&f.scope.agent_id, primary.clone()));
    let prepared =
        PreparedInputTransition::queue(f.scope.clone(), "pending-dedup".into(), f.at, queue)?;
    assert_eq!(prepared.pending_messages()?, vec![&primary]);
    let mut queue = f.queue()?;
    let QueueMutation::Upsert(record) = &mut queue.command.mutation else {
        unreachable!()
    };
    record.status = QueueEntryStatus::Interrupted;
    let prepared =
        PreparedInputTransition::queue(f.scope.clone(), "pending-interrupted".into(), f.at, queue)?;
    assert_eq!(prepared.pending_messages()?.len(), 1);
    Ok(())
}

#[test]
fn pending_messages_rejects_missing_foreign_and_conflicting_bodies_before_sql() -> Result<()> {
    let f = Fixture::new()?;
    let mut queue = f.queue()?;
    queue.command.message_evidence.clear();
    let prepared =
        PreparedInputTransition::queue(f.scope.clone(), "pending-missing".into(), f.at, queue)?;
    assert!(prepared.pending_messages().is_err());
    let mut queue = f.queue()?;
    let mut foreign = queue.command.message_evidence[0].clone();
    foreign.id = "foreign:wait".into();
    foreign.agent_id = "other-agent".into();
    queue.wait_registration = Some(pending_wait(&f.scope.agent_id, foreign));
    let prepared =
        PreparedInputTransition::queue(f.scope.clone(), "pending-foreign".into(), f.at, queue)?;
    assert!(prepared.pending_messages().is_err());
    let mut queue = f.queue()?;
    let mut conflicting = queue.command.message_evidence[0].clone();
    conflicting.body = MessageBody::Text {
        text: "changed body".into(),
    };
    queue.wait_registration = Some(pending_wait(&f.scope.agent_id, conflicting));
    let prepared =
        PreparedInputTransition::queue(f.scope.clone(), "pending-conflicting".into(), f.at, queue)?;
    assert!(prepared.pending_messages().is_err());
    assert_eq!(f.receipt_count()?, 0);
    Ok(())
}

#[test]
fn consuming_queue_and_posture_have_no_new_pending_body() -> Result<()> {
    let f = Fixture::new()?;
    let mut queue = f.queue()?;
    let QueueMutation::Upsert(mut record) = queue.command.mutation else {
        unreachable!()
    };
    record.status = QueueEntryStatus::Dequeued;
    queue.command.operation = QueueOperation::Claim;
    queue.command.mutation = QueueMutation::Consume(record);
    queue.command.message_evidence.clear();
    let prepared =
        PreparedInputTransition::queue(f.scope.clone(), "pending-consume".into(), f.at, queue)?;
    assert!(prepared.pending_messages()?.is_empty());
    let prepared = PreparedInputTransition::posture(
        f.scope.clone(),
        "pending-posture".into(),
        f.at,
        f.posture(f.state()?, None)?,
        None,
    )?;
    assert!(prepared.pending_messages()?.is_empty());
    Ok(())
}

impl Fixture {
    fn deleting(&self) -> Result<()> {
        let mut identity = self
            .db
            .agent_identities()
            .latest(&self.scope.agent_id)?
            .unwrap();
        identity.status = crate::types::AgentRegistryStatus::Deleting;
        self.db.agent_identities().upsert(&identity)
    }
    fn admitted_cleanup(&self) -> Result<PreparedQueueTransition> {
        let queue = self.queue()?;
        let original = queue.command.mutation.clone();
        let admitted = PreparedInputTransition::queue(
            self.scope.clone(),
            "admitted-before-deletion".into(),
            self.at,
            queue,
        )?;
        self.db
            .transitions_in_scope(&self.scope)
            .commit_prepared_transition(&admitted)?;
        let before = self.state()?;
        let mut after = before.clone();
        after.pending = 0;
        let QueueMutation::Upsert(expected) = original else {
            unreachable!()
        };
        let mut record = expected.clone();
        record.status = QueueEntryStatus::Dropped;
        let mut brief = crate::types::BriefRecord::new(
            &self.scope.agent_id,
            crate::types::BriefKind::Failure,
            "original admitted run interrupted",
            Some(record.message_id.clone()),
            None,
        );
        brief.id = "brief:cleanup-derived".into();
        brief.created_at = self.at;
        let mut trace = crate::types::TranscriptEntry::new(
            &self.scope.agent_id,
            crate::types::TranscriptEntryKind::RuntimeFailure,
            None,
            Some(record.message_id.clone()),
            serde_json::json!({"reason":"daemon_shutdown"}),
        );
        trace.id = "transcript:cleanup-derived".into();
        trace.created_at = self.at;
        Ok(PreparedQueueTransition::new(QueueTransitionCommand {
            agent_id: self.scope.agent_id.clone(),
            operation: QueueOperation::Settle,
            mutation: QueueMutation::CompareAndSet { expected, record },
            scheduler_claim_work_item: None,
            agent_state: Some(AgentStateMutation {
                expected: Some(Box::new(before)),
                record: Box::new(after),
            }),
            message_evidence: Vec::new(),
            transcript_entries: vec![trace],
            turn_record: None,
            audit_events: vec![crate::types::brief_created_event_for(&brief)?],
            notify_scheduler: true,
            fault: None,
            brief_evidence: vec![brief],
        }))
    }
}

#[test]
fn deleting_admitted_cleanup_commits_new_trace_and_brief_with_original_receipt() -> Result<()> {
    let f = Fixture::new()?;
    let queue = f.admitted_cleanup()?;
    f.deleting()?;
    let original =
        PreparedInputTransition::queue(f.scope.clone(), "deleting-cleanup".into(), f.at, queue)?;
    let committed = match f
        .db
        .transitions_in_scope(&f.scope)
        .commit_prepared_transition(&original)?
    {
        InputMutationExecution::Committed { value, receipt } => {
            assert!(value.applied);
            assert_eq!(value.effects.transcript_entries[0].transcript_seq, Some(1));
            assert!(value.effects.briefs[0].created_event_seq.is_some());
            assert!(value
                .effects
                .input_evidence_delta
                .records
                .iter()
                .any(|record| record.locator.key.id == "brief:cleanup-derived"));
            receipt
        }
        _ => panic!("first cleanup must commit"),
    };
    assert_eq!(f.state()?.pending, 0);
    assert!(
        matches!(f.db.transitions_in_scope(&f.scope).commit_prepared_transition(&original)?,
        InputMutationExecution::Replayed { receipt } if receipt == committed)
    );
    Ok(())
}

#[test]
fn deleting_cleanup_rolls_back_all_derived_facts_and_receipt_on_precommit_fault() -> Result<()> {
    let f = Fixture::new()?;
    let mut queue = f.admitted_cleanup()?;
    let state = f.state()?;
    queue.command.fault = Some(TransitionFaultPoint::BeforeCommit);
    f.deleting()?;
    let original = PreparedInputTransition::queue(
        f.scope.clone(),
        "failed-deleting-cleanup".into(),
        f.at,
        queue,
    )?;
    assert!(f
        .db
        .transitions_in_scope(&f.scope)
        .commit_prepared_transition(&original)
        .is_err());
    assert_eq!(f.state()?, state);
    assert_eq!(f.receipt_count()?, 1);
    for table in ["transcript_entries", "briefs"] {
        assert_eq!(
            f.db.connection()?
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                    .get::<_, i64>(0))?,
            0
        );
    }
    Ok(())
}

#[test]
fn deleting_cleanup_requires_original_qualified_source_and_cannot_admit_or_start() -> Result<()> {
    let f = Fixture::new()?;
    let queue = f.admitted_cleanup()?;
    f.deleting()?;
    f.db.connection()?.execute("UPDATE input_evidence_current SET availability='pruned' WHERE agent_id=?1 AND family='message'", [&f.scope.agent_id])?;
    let original =
        PreparedInputTransition::queue(f.scope.clone(), "pruned-cleanup".into(), f.at, queue)?;
    assert!(f
        .db
        .transitions_in_scope(&f.scope)
        .commit_prepared_transition(&original)
        .is_err());
    let new = PreparedInputTransition::queue(
        f.scope.clone(),
        "new-deleting-admit".into(),
        f.at,
        f.queue()?,
    )?;
    assert!(f
        .db
        .transitions_in_scope(&f.scope)
        .commit_prepared_transition(&new)
        .is_err());
    let mut next = f.state()?;
    next.current_run_id = Some("invented-run".into());
    next.status = crate::types::AgentStatus::AwakeRunning;
    let new = PreparedInputTransition::posture(
        f.scope.clone(),
        "new-deleting-posture".into(),
        f.at,
        f.posture(next, None)?,
        None,
    )?;
    assert!(f
        .db
        .transitions_in_scope(&f.scope)
        .commit_prepared_transition(&new)
        .is_err());
    let mut next = f.state()?;
    next.current_turn_id = Some("turn:new-during-deleting".into());
    next.turn_index = 1;
    let new = PreparedInputTransition::posture(
        f.scope.clone(),
        "new-deleting-turn".into(),
        f.at,
        f.posture(next, None)?,
        Some(TurnRecord::new(
            &f.scope.agent_id,
            "turn:new-during-deleting",
            1,
        )),
    )?;
    assert!(f
        .db
        .transitions_in_scope(&f.scope)
        .commit_prepared_transition(&new)
        .is_err());
    let mut brief = crate::types::BriefRecord::new(
        &f.scope.agent_id,
        crate::types::BriefKind::Failure,
        "generic standalone is not cleanup",
        Some("message:prepared-input".into()),
        None,
    );
    brief.id = "brief:standalone-deleting".into();
    assert!(f
        .db
        .evidence()
        .append_brief_in_scope(&f.scope, &brief, &[])
        .is_err());
    assert_eq!(f.receipt_count()?, 1);
    Ok(())
}

#[test]
fn cleanup_cannot_recreate_deleted_derived_body_with_pruned_alias() -> Result<()> {
    use crate::{
        runtime::resident_input_evidence::{InputEvidenceFamily, InputEvidenceKey},
        runtime_db::input_evidence_snapshot::register_pruned_input_evidence_tx,
    };
    for family in [InputEvidenceFamily::Brief, InputEvidenceFamily::Transcript] {
        let f = Fixture::new()?;
        let queue = f.admitted_cleanup()?;
        let (id, table) = match family {
            InputEvidenceFamily::Brief => {
                let brief = &queue.command.brief_evidence[0];
                f.db.evidence()
                    .append_brief_in_scope(&f.scope, brief, &[])?;
                (brief.id.clone(), "briefs")
            }
            InputEvidenceFamily::Transcript => {
                let trace = &queue.command.transcript_entries[0];
                f.db.evidence()
                    .append_transcript_entry_in_scope(&f.scope, trace)?;
                (trace.id.clone(), "transcript_entries")
            }
            _ => unreachable!(),
        };
        let key = InputEvidenceKey::new(family, &id);
        f.db.transaction(|tx| {
            assert_eq!(
                tx.execute(
                    &format!("DELETE FROM {table} WHERE agent_id=?1 AND evidence_id=?2"),
                    rusqlite::params![f.scope.agent_id, id],
                )?,
                1
            );
            let removal = register_pruned_input_evidence_tx(tx, &f.scope, &[key.clone()])?;
            assert_eq!(removal.removals.len(), 1);
            Ok(())
        })?;
        // The common prewrite guard also protects ordinary Active writers.
        let error = match family {
            InputEvidenceFamily::Brief => {
                f.db.evidence()
                    .append_brief_in_scope(&f.scope, &queue.command.brief_evidence[0], &[])
                    .unwrap_err()
            }
            InputEvidenceFamily::Transcript => f
                .db
                .evidence()
                .append_transcript_entry_in_scope(&f.scope, &queue.command.transcript_entries[0])
                .unwrap_err(),
            _ => unreachable!(),
        };
        assert!(format!("{error:#}").contains("pruned evidence ID"));
        let before = f.state()?;
        f.deleting()?;
        let original = PreparedInputTransition::queue(
            f.scope.clone(),
            format!("new-cleanup-nonce-{family:?}"),
            f.at,
            queue,
        )?;
        let error =
            f.db.transitions_in_scope(&f.scope)
                .commit_prepared_transition(&original)
                .unwrap_err();
        assert!(format!("{error:#}").contains("pruned evidence ID"));
        assert_eq!(f.state()?, before);
        assert_eq!(f.receipt_count()?, 1);
        let connection = f.db.connection()?;
        assert_eq!(
            connection.query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE evidence_id=?1"),
                [&id],
                |row| row.get::<_, i64>(0),
            )?,
            0
        );
        assert_eq!(connection.query_row(
            "SELECT availability FROM input_evidence_current WHERE agent_id=?1 AND identity_incarnation=?2 AND family=?3 AND record_id=?4",
            rusqlite::params![f.scope.agent_id, f.scope.identity_incarnation, family.as_str(), id],
            |row| row.get::<_, String>(0),
        )?, "pruned");
    }
    Ok(())
}

impl Fixture {
    fn admit_for_progress(&self) -> Result<()> {
        let admission = PreparedInputTransition::queue(
            self.scope.clone(),
            "progress-admission".into(),
            self.at,
            self.queue()?,
        )?;
        assert!(matches!(
            self.db
                .transitions_in_scope(&self.scope)
                .commit_prepared_transition(&admission)?,
            InputMutationExecution::Committed { .. }
        ));
        Ok(())
    }
    fn progress_command(&self, max_attempts: u32) -> Result<QueueHeadNoProgressCommand> {
        let expected = self
            .db
            .queue_entries()
            .latest("message:prepared-input")?
            .unwrap();
        let mut quarantined = expected.clone();
        quarantined.status = QueueEntryStatus::Quarantined;
        quarantined.updated_at = self.at + chrono::Duration::seconds(1);
        let state = self.state()?;
        let mut next = state.clone();
        next.pending = 0;
        Ok(QueueHeadNoProgressCommand {
            agent_id: self.scope.agent_id.clone(),
            expected,
            quarantined,
            agent_state: AgentStateMutation {
                expected: Some(Box::new(state)),
                record: Box::new(next),
            },
            reason: "canonical_claim_replan_exhausted".into(),
            scenario_class: Some("lifecycle_external_nudge".into()),
            max_attempts,
            fault: None,
        })
    }
    fn progress_rows(&self) -> Result<i64> {
        Ok(self.db.connection()?.query_row(
            "SELECT COUNT(*) FROM queue_head_no_progress",
            [],
            |row| row.get(0),
        )?)
    }
}

fn progress_result(
    receipt: &crate::runtime_db::input_mutation_receipts::InputMutationReceipt,
) -> Option<QueueHeadNoProgressOutcome> {
    match &receipt.outcome.result {
        crate::runtime_db::input_mutation_receipts::InputMutationResult::QueueHeadNoProgress {
            outcome,
        } => *outcome,
        _ => panic!("wrong original result family"),
    }
}

#[test]
fn prepared_queue_progress_freezes_clock_and_replays_original_bounded_disposition() -> Result<()> {
    use crate::runtime_db::input_mutation_receipts::{
        with_fresh_input_mutation_resolution, InputMutationResolution, OriginalInputMutationAttempt,
    };
    let f = Fixture::new()?;
    f.admit_for_progress()?;
    let command = f.progress_command(3)?;
    let first = PreparedInputTransition::queue_head_no_progress(
        f.scope.clone(),
        "progress-first".into(),
        f.at,
        command.clone(),
    )?;
    assert!(first.pending_messages()?.is_empty());
    let attempt = OriginalInputMutationAttempt::new(first.descriptor().clone());
    let original = match f
        .db
        .transitions_in_scope(&f.scope)
        .commit_prepared_transition(&first)?
    {
        InputMutationExecution::Committed { value, receipt } => {
            assert!(value.applied);
            assert!(value.effects.agent_state.is_none());
            let audit = &value.effects.audit_events[0];
            assert_eq!(audit.id, "queue-progress:progress-first");
            assert_eq!(audit.created_at, f.at);
            assert!(audit.event_seq > 0);
            assert_eq!(
                progress_result(&receipt),
                Some(QueueHeadNoProgressOutcome::BoundedDefer {
                    attempt: 1,
                    max_attempts: 3
                })
            );
            receipt
        }
        _ => panic!("first attempt must commit"),
    };
    attempt.finish_original_attempt();
    for n in 2..=3 {
        let next = PreparedInputTransition::queue_head_no_progress(
            f.scope.clone(),
            format!("progress-{n}"),
            f.at + chrono::Duration::seconds(i64::from(n)),
            command.clone(),
        )?;
        let InputMutationExecution::Committed { value, receipt } =
            f.db.transitions_in_scope(&f.scope)
                .commit_prepared_transition(&next)?
        else {
            panic!("each newly prepared attempt commits once")
        };
        let expected = if n == 3 {
            QueueHeadNoProgressOutcome::Quarantined {
                attempt: n,
                max_attempts: 3,
            }
        } else {
            QueueHeadNoProgressOutcome::BoundedDefer {
                attempt: n,
                max_attempts: 3,
            }
        };
        assert_eq!(progress_result(&receipt), Some(expected));
        assert_eq!(
            value.effects.removed_queue_message_ids,
            if n == 3 {
                vec![command.expected.message_id.clone()]
            } else {
                vec![]
            }
        );
        assert_eq!(value.effects.agent_state.is_some(), n == 3);
    }
    assert_eq!(f.state()?.pending, 0);
    assert_eq!(
        f.db.queue_entries()
            .latest(&command.expected.message_id)?
            .unwrap(),
        command.quarantined
    );
    let InputMutationExecution::Replayed { receipt } =
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&first)?
    else {
        panic!("later quarantine must not rerun the first reducer")
    };
    assert_eq!(receipt, original);
    with_fresh_input_mutation_resolution(&mut f.db.connection()?, &attempt, |_, resolution| {
        assert_eq!(resolution, InputMutationResolution::Present(original));
        Ok(())
    })?;
    let (attempts, first_at, updated_at): (u32, String, String) = f.db.connection()?.query_row(
        "SELECT attempts,first_deferred_at,updated_at FROM queue_head_no_progress",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(attempts, 3);
    assert_eq!(first_at, f.at.to_rfc3339());
    assert_eq!(
        updated_at,
        (f.at + chrono::Duration::seconds(3)).to_rfc3339()
    );
    assert_eq!(f.receipt_count()?, 4);
    Ok(())
}

#[test]
fn prepared_queue_progress_successful_none_has_receipt_and_cannot_later_apply() -> Result<()> {
    let f = Fixture::new()?;
    f.admit_for_progress()?;
    let mut command = f.progress_command(1)?;
    command.expected.updated_at += chrono::Duration::seconds(1);
    let first = PreparedInputTransition::queue_head_no_progress(
        f.scope.clone(),
        "stale-progress".into(),
        f.at,
        command.clone(),
    )?;
    let original = match f
        .db
        .transitions_in_scope(&f.scope)
        .commit_prepared_transition(&first)?
    {
        InputMutationExecution::Committed { value, receipt } => {
            assert!(!value.applied);
            assert!(value.effects.audit_events.is_empty());
            assert_eq!(progress_result(&receipt), None);
            receipt
        }
        _ => panic!("successful stale disposition must commit a no-op receipt"),
    };
    assert_eq!(f.progress_rows()?, 0);
    f.db.queue_entries().upsert(&command.expected)?;
    assert!(
        matches!(f.db.transitions_in_scope(&f.scope).commit_prepared_transition(&first)?,
        InputMutationExecution::Replayed { receipt } if receipt == original)
    );
    assert_eq!(f.progress_rows()?, 0);
    assert_eq!(f.state()?.pending, 1);
    assert_eq!(f.receipt_count()?, 2);
    Ok(())
}

#[test]
fn prepared_queue_progress_rolls_back_counter_state_queue_audit_and_receipt() -> Result<()> {
    use crate::runtime_db::input_mutation_receipts::{
        with_fresh_input_mutation_resolution, InputMutationResolution, OriginalInputMutationAttempt,
    };
    for fault in [
        TransitionFaultPoint::AfterValidation,
        TransitionFaultPoint::AfterCanonicalWrites,
        TransitionFaultPoint::AfterAuditWrites,
        TransitionFaultPoint::BeforeCommit,
    ] {
        let f = Fixture::new()?;
        f.admit_for_progress()?;
        let before = f.state()?;
        let mut command = f.progress_command(1)?;
        command.fault = Some(fault);
        let expected = command.expected.clone();
        let prepared = PreparedInputTransition::queue_head_no_progress(
            f.scope.clone(),
            format!("failed-progress-{fault:?}"),
            f.at,
            command,
        )?;
        let attempt = OriginalInputMutationAttempt::new(prepared.descriptor().clone());
        assert!(f
            .db
            .transitions_in_scope(&f.scope)
            .commit_prepared_transition(&prepared)
            .is_err());
        attempt.finish_original_attempt();
        with_fresh_input_mutation_resolution(
            &mut f.db.connection()?,
            &attempt,
            |_, resolution| {
                assert_eq!(resolution, InputMutationResolution::Absent);
                Ok(())
            },
        )?;
        assert_eq!(f.state()?, before);
        assert_eq!(
            f.db.queue_entries().latest(&expected.message_id)?,
            Some(expected)
        );
        assert_eq!(f.progress_rows()?, 0);
        assert_eq!(f.receipt_count()?, 1);
        assert_eq!(
            f.db.connection()?
                .query_row("SELECT COUNT(*) FROM audit_events", [], |row| row
                    .get::<_, i64>(0))?,
            0
        );
    }
    // Even a business no-op must roll back its own receipt on precommit failure.
    let f = Fixture::new()?;
    f.admit_for_progress()?;
    let mut command = f.progress_command(1)?;
    command.expected.updated_at += chrono::Duration::seconds(1);
    command.fault = Some(TransitionFaultPoint::BeforeCommit);
    let prepared = PreparedInputTransition::queue_head_no_progress(
        f.scope.clone(),
        "failed-noop-progress".into(),
        f.at,
        command,
    )?;
    assert!(f
        .db
        .transitions_in_scope(&f.scope)
        .commit_prepared_transition(&prepared)
        .is_err());
    assert_eq!(f.receipt_count()?, 1);
    Ok(())
}

#[test]
fn prepared_queue_progress_binds_group_payload_and_clock_before_replay() -> Result<()> {
    let f = Fixture::new()?;
    f.admit_for_progress()?;
    let secondary = InputEvidenceScope {
        agent_id: "progress-related".into(),
        identity_incarnation: 1,
    };
    f.db.transaction(|tx| {
        upsert_agent_identity_tx(
            tx,
            &AgentIdentityRecord::new(
                &secondary.agent_id,
                AgentKind::Named,
                AgentVisibility::Public,
                AgentOwnership::SelfOwned,
                AgentProfilePreset::PublicNamed,
                None,
                None,
            ),
        )
    })?;
    let command = f.progress_command(3)?;
    let grouped = PreparedInputTransition::queue_head_no_progress(
        f.scope.clone(),
        "grouped-progress".into(),
        f.at,
        command.clone(),
    )?
    .with_scope_group(vec![secondary.clone(), f.scope.clone()])?;
    let InputMutationExecution::Committed { receipt, .. } =
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&grouped)?
    else {
        panic!()
    };
    assert_eq!(receipt.outcome.scopes.len(), 2);
    let empty = receipt
        .outcome
        .scopes
        .iter()
        .find(|outcome| outcome.scope == secondary)
        .unwrap();
    assert!(empty.audit_events.is_empty());
    assert!(!empty.notify_scheduler);
    for changed_clock in [false, true] {
        let mut changed = command.clone();
        if !changed_clock {
            changed.reason.push_str("-changed");
        }
        let conflict = PreparedInputTransition::queue_head_no_progress(
            f.scope.clone(),
            "grouped-progress".into(),
            f.at + chrono::Duration::seconds(i64::from(changed_clock)),
            changed,
        )?
        .with_scope_group(vec![f.scope.clone(), secondary.clone()])?;
        assert!(matches!(
            f.db.transitions_in_scope(&f.scope)
                .commit_prepared_transition(&conflict)
                .unwrap_err()
                .downcast_ref::<ReceiptError>(),
            Some(ReceiptError::CommandConflict)
        ));
    }
    let missing_group = PreparedInputTransition::queue_head_no_progress(
        f.scope.clone(),
        "grouped-progress".into(),
        f.at,
        command,
    )?;
    assert!(matches!(
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&missing_group)
            .unwrap_err()
            .downcast_ref::<ReceiptError>(),
        Some(ReceiptError::GroupConflict)
    ));
    assert_eq!(f.progress_rows()?, 1);
    assert_eq!(f.receipt_count()?, 2);
    Ok(())
}

fn namespace_rows(fixture: &Fixture) -> Result<BTreeMap<String, Vec<String>>> {
    let connection = fixture.db.connection()?;
    let mut snapshot = BTreeMap::new();
    for table in [
        "agent_identities",
        "agent_states",
        "messages",
        "queue_entries",
        "work_items",
        "wait_conditions",
        "work_item_continuations",
        "turn_records",
        "runtime_index_outbox",
        "audit_events",
        "input_mutation_receipts",
        "input_evidence_current",
        "runtime_sequences",
    ] {
        let mut statement = connection.prepare(&format!("SELECT * FROM {table}"))?;
        let columns = statement.column_count();
        let mut rows = statement
            .query_map([], |row| {
                (0..columns)
                    .map(|column| row.get::<_, rusqlite::types::Value>(column))
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map(|values| format!("{values:?}"))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.sort();
        snapshot.insert(table.into(), rows);
    }
    Ok(snapshot)
}

fn absent_secondary() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "cold-child".into(),
        identity_incarnation: 1,
    }
}

#[test]
fn readonly_secondary_cannot_receive_queue_or_state_writes() -> Result<()> {
    for target in [
        "queue-upsert",
        "queue-consume",
        "queue-expected",
        "queue-record",
        "state-record",
        "state-expected",
    ] {
        let f = Fixture::new()?;
        let secondary = absent_secondary();
        let before = namespace_rows(&f)?;
        let mut queue = f.queue()?;
        let mut record = queue_mutation_record(&queue.command.mutation).clone();
        match target {
            "queue-upsert" | "queue-consume" => {
                record.agent_id = secondary.agent_id.clone();
                queue.command.agent_state = None;
                queue.command.message_evidence.clear();
                if target == "queue-upsert" {
                    // The original regression: Settle has no pending body, so
                    // pending_messages() cannot certify its write namespace.
                    record.status = QueueEntryStatus::Processed;
                    queue.command.operation = QueueOperation::Settle;
                    queue.command.mutation = QueueMutation::Upsert(record);
                } else {
                    record.status = QueueEntryStatus::Dequeued;
                    queue.command.operation = QueueOperation::Claim;
                    queue.command.mutation = QueueMutation::Consume(record);
                }
            }
            "queue-expected" | "queue-record" => {
                queue.command.operation = QueueOperation::Settle;
                queue.command.agent_state = None;
                queue.command.message_evidence.clear();
                let mut expected = record.clone();
                record.status = QueueEntryStatus::Dropped;
                if target == "queue-expected" {
                    expected.agent_id = secondary.agent_id.clone();
                } else {
                    record.agent_id = secondary.agent_id.clone();
                }
                queue.command.mutation = QueueMutation::CompareAndSet { expected, record };
            }
            "state-record" => {
                queue.command.agent_state = Some(AgentStateMutation {
                    expected: None,
                    record: Box::new(AgentState::new(&secondary.agent_id)),
                });
            }
            "state-expected" => {
                queue.command.agent_state.as_mut().unwrap().expected =
                    Some(Box::new(AgentState::new(&secondary.agent_id)));
            }
            _ => unreachable!(),
        }
        let prepared = PreparedInputTransition::queue(
            f.scope.clone(),
            format!("foreign-{target}"),
            f.at,
            queue,
        )?
        .with_scope_group(vec![secondary, f.scope.clone()])?;
        if target == "queue-upsert" {
            assert!(prepared.pending_messages()?.is_empty());
        }
        let error =
            f.db.transitions_in_scope(&f.scope)
                .commit_prepared_transition(&prepared)
                .unwrap_err();
        assert!(
            format!("{error:#}").contains("primary namespace"),
            "{target}: {error:#}"
        );
        assert_eq!(namespace_rows(&f)?, before, "{target}");
    }
    Ok(())
}

fn namespace_wait(agent: &str, at: chrono::DateTime<Utc>) -> WaitConditionRecord {
    WaitConditionRecord {
        id: "wait:namespace".into(),
        agent_id: agent.into(),
        work_item_id: None,
        status: WaitConditionStatus::Active,
        kind: crate::types::WaitConditionKind::External,
        source: None,
        subject_ref: None,
        waiting_for: "namespace fixture".into(),
        wake_sources: Vec::new(),
        continuation: None,
        created_at: at,
        updated_at: at,
        expires_at: None,
        resolved_at: None,
        cancelled_at: None,
        turn_id: None,
        trigger_message_id: None,
        triggered_at: None,
    }
}

#[test]
fn readonly_secondary_cannot_receive_nested_wait_work_or_index_writes() -> Result<()> {
    for target in [
        "extra-wait",
        "registered-wait",
        "registration-agent",
        "wait-expected",
        "wait-record",
        "wait-result-expected",
        "wait-result-record",
        "wait-result-message",
        "wait-state",
        "wait-work",
        "wait-index",
    ] {
        let f = Fixture::new()?;
        let secondary = absent_secondary();
        let before = namespace_rows(&f)?;
        let mut queue = f.queue()?;
        let message = queue.command.message_evidence[0].clone();
        let mut registration = pending_wait(&f.scope.agent_id, message.clone());
        match target {
            "extra-wait" => {
                queue
                    .extra_wait_conditions
                    .push(namespace_wait(&secondary.agent_id, f.at));
            }
            "registered-wait" => {
                registration
                    .wait_conditions
                    .push(namespace_wait(&secondary.agent_id, f.at));
            }
            "registration-agent" => registration.agent_id = secondary.agent_id.clone(),
            "wait-expected" | "wait-record" => {
                let mut wait = namespace_wait(&f.scope.agent_id, f.at);
                let mut expected = WaitConditionExpectation {
                    id: wait.id.clone(),
                    agent_id: wait.agent_id.clone(),
                    status: wait.status.clone(),
                    updated_at: wait.updated_at,
                };
                if target == "wait-expected" {
                    expected.agent_id = secondary.agent_id.clone();
                } else {
                    wait.agent_id = secondary.agent_id.clone();
                }
                queue.wait_transition = Some(QueueWaitTransition {
                    expected,
                    record: wait,
                    work_item: None,
                    index_changes: Vec::new(),
                });
            }
            "wait-result-expected" => {
                let admission = registration.task_result_admission.as_mut().unwrap();
                let mut expected = admission.record.clone();
                expected.agent_id = secondary.agent_id.clone();
                admission.expected = Some(expected);
            }
            "wait-result-record" => {
                registration
                    .task_result_admission
                    .as_mut()
                    .unwrap()
                    .record
                    .agent_id = secondary.agent_id.clone();
            }
            "wait-result-message" => {
                registration
                    .task_result_admission
                    .as_mut()
                    .unwrap()
                    .message
                    .agent_id = secondary.agent_id.clone();
            }
            "wait-state" => {
                registration.agent_state = Some(AgentStateMutation {
                    expected: None,
                    record: Box::new(AgentState::new(&secondary.agent_id)),
                });
            }
            "wait-work" => {
                registration.work_items.push(WorkItemMutation::Insert {
                    record: WorkItemRecord::new(
                        &secondary.agent_id,
                        "foreign work",
                        WorkItemState::Open,
                    ),
                });
            }
            "wait-index" => {
                let mut change = RuntimeIndexChange::for_message(&message);
                change.agent_id = secondary.agent_id.clone();
                registration.index_changes.push(change);
            }
            _ => unreachable!(),
        }
        queue.wait_registration = Some(registration);
        let prepared = PreparedInputTransition::queue(
            f.scope.clone(),
            format!("foreign-{target}"),
            f.at,
            queue,
        )?
        .with_scope_group(vec![f.scope.clone(), secondary])?;
        let error =
            f.db.transitions_in_scope(&f.scope)
                .commit_prepared_transition(&prepared)
                .unwrap_err();
        assert!(
            format!("{error:#}").contains("primary namespace"),
            "{target}: {error:#}"
        );
        assert_eq!(namespace_rows(&f)?, before, "{target}");
    }
    Ok(())
}

#[test]
fn primary_queue_commit_preserves_truly_readonly_absent_secondary() -> Result<()> {
    let f = Fixture::new()?;
    let secondary = absent_secondary();
    let prepared = PreparedInputTransition::queue(
        f.scope.clone(),
        "primary-with-absent-secondary".into(),
        f.at,
        f.queue()?,
    )?
    .with_scope_group(vec![secondary.clone(), f.scope.clone()])?;
    let InputMutationExecution::Committed { value, receipt } =
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&prepared)?
    else {
        panic!("primary queue should commit with a truly readonly absent owner");
    };
    assert!(value.applied);
    assert_eq!(f.state()?.pending, 1);
    assert_eq!(f.db.queue_entries().latest_all()?.len(), 1);
    let outcome = receipt
        .outcome
        .scopes
        .iter()
        .find(|outcome| outcome.scope == secondary)
        .unwrap();
    assert!(outcome.audit_events.is_empty());
    assert_eq!(outcome.install_keys, Default::default());
    assert!(!outcome.notify_scheduler);
    assert!(!outcome.notify_memory_index);
    let child_rows: i64 = f.db.connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM agent_identities WHERE agent_id=?1)
              + (SELECT COUNT(*) FROM agent_states WHERE agent_id=?1)
              + (SELECT COUNT(*) FROM queue_entries WHERE agent_id=?1)
              + (SELECT COUNT(*) FROM input_evidence_current WHERE agent_id=?1)",
        [&secondary.agent_id],
        |row| row.get(0),
    )?;
    assert_eq!(child_rows, 0);
    assert!(matches!(
        f.db.transitions_in_scope(&f.scope).commit_prepared_transition(&prepared)?,
        InputMutationExecution::Replayed { receipt: original } if original == receipt
    ));
    assert_eq!(f.db.queue_entries().latest_all()?.len(), 1);
    Ok(())
}

#[test]
fn primary_payload_cannot_reassign_existing_foreign_queue_work_or_wait() -> Result<()> {
    for target in ["queue", "work", "wait"] {
        let f = Fixture::new()?;
        let foreign = InputEvidenceScope {
            agent_id: "existing-foreign".into(),
            identity_incarnation: 1,
        };
        let mut command = f.queue()?;
        let mut foreign_queue = queue_mutation_record(&command.command.mutation).clone();
        foreign_queue.agent_id = foreign.agent_id.clone();
        foreign_queue.message_id = "message:existing-foreign".into();
        let foreign_work =
            WorkItemRecord::new(&foreign.agent_id, "foreign work", WorkItemState::Open);
        let foreign_wait = namespace_wait(&foreign.agent_id, f.at);
        f.db.transaction(|tx| {
            upsert_agent_identity_tx(
                tx,
                &AgentIdentityRecord::new(
                    &foreign.agent_id,
                    AgentKind::Named,
                    AgentVisibility::Public,
                    AgentOwnership::SelfOwned,
                    AgentProfilePreset::PublicNamed,
                    None,
                    None,
                ),
            )?;
            upsert_agent_state_tx(tx, &AgentState::new(&foreign.agent_id))?;
            match target {
                "queue" => {
                    let mut message = command.command.message_evidence[0].clone();
                    message.id = foreign_queue.message_id.clone();
                    message.agent_id = foreign.agent_id.clone();
                    append_message_tx(tx, &message)?;
                    upsert_queue_entry_tx(tx, &foreign_queue)?;
                }
                "work" => {
                    insert_new_work_item_tx(tx, &foreign_work)?;
                }
                "wait" => {
                    upsert_wait_condition_tx(tx, &foreign_wait)?;
                }
                _ => unreachable!(),
            }
            Ok(())
        })?;
        let expected_table = match target {
            "queue" => {
                let mut record = foreign_queue;
                record.agent_id = f.scope.agent_id.clone();
                record.status = QueueEntryStatus::Processed;
                record.updated_at = f.at + chrono::Duration::milliseconds(1);
                command.command.operation = QueueOperation::Settle;
                command.command.mutation = QueueMutation::Upsert(record);
                command.command.agent_state = None;
                command.command.message_evidence.clear();
                "queue_entries"
            }
            "work" => {
                let mut record = foreign_work;
                record.agent_id = f.scope.agent_id.clone();
                record.revision = 2;
                let mut wait = pending_wait(
                    &f.scope.agent_id,
                    command.command.message_evidence[0].clone(),
                );
                wait.task_result_admission = None;
                wait.work_items.push(WorkItemMutation::Update {
                    record,
                    expected_revision: 1,
                });
                command.wait_registration = Some(wait);
                "work_items"
            }
            "wait" => {
                let mut record = foreign_wait;
                record.agent_id = f.scope.agent_id.clone();
                record.status = WaitConditionStatus::Resolved;
                record.updated_at = f.at + chrono::Duration::milliseconds(1);
                record.resolved_at = Some(record.updated_at);
                command.extra_wait_conditions.push(record);
                "wait_conditions"
            }
            _ => unreachable!(),
        };
        let prepared = PreparedInputTransition::queue(
            f.scope.clone(),
            format!("existing-foreign-{target}"),
            f.at,
            command,
        )?
        .with_scope_group(vec![foreign, f.scope.clone()])?;
        // All frozen payload namespaces are primary. The collision is only
        // visible from the actual canonical row selected by its global ID.
        prepared.validate_primary_namespace()?;
        let before = namespace_rows(&f)?;
        let error =
            f.db.transitions_in_scope(&f.scope)
                .commit_prepared_transition(&prepared)
                .unwrap_err();
        assert!(
            format!("{error:#}").contains(&format!(
                "stored {expected_table} target differs from primary namespace"
            )),
            "{target}: {error:#}"
        );
        assert_eq!(namespace_rows(&f)?, before, "{target}");
        assert_eq!(f.receipt_count()?, 0, "{target}");
    }
    Ok(())
}

#[test]
fn exact_receipt_replay_precedes_later_existing_target_namespace_check() -> Result<()> {
    let f = Fixture::new()?;
    let prepared = PreparedInputTransition::queue(
        f.scope.clone(),
        "before-target-owner-changed".into(),
        f.at,
        f.queue()?,
    )?;
    let InputMutationExecution::Committed { receipt, .. } =
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&prepared)?
    else {
        panic!("first original command must commit");
    };
    // Simulate a restored conflicting row directly; ordinary queue writes
    // preserve conversation ownership and reject this restoration. An exact
    // ACK replay must not run its old command or overwrite the later row.
    let mut later_record = queue_mutation_record(&f.queue()?.command.mutation).clone();
    later_record.agent_id = "later-foreign".into();
    later_record.updated_at = f.at + chrono::Duration::milliseconds(1);
    f.db.transaction(|tx| {
        assert_eq!(tx.execute(
            "UPDATE queue_entries SET agent_id=?1, updated_at=?2, payload_json=?3 WHERE message_id=?4",
            rusqlite::params![
                later_record.agent_id,
                later_record.updated_at.timestamp_millis(),
                serde_json::to_string(&later_record)?,
                later_record.message_id,
            ],
        )?, 1);
        Ok(())
    })?;
    let before = namespace_rows(&f)?;
    let InputMutationExecution::Replayed { receipt: replay } =
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&prepared)?
    else {
        panic!("exact receipt must replay before reading changed business targets");
    };
    assert_eq!(replay, receipt);
    assert_eq!(namespace_rows(&f)?, before);
    Ok(())
}
