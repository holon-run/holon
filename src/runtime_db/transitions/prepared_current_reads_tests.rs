use super::*;
use crate::{
    config::StateMemoryLimits,
    runtime::{current_read_facts::*, input_evidence_budget::InputEvidenceBudgetLedger},
    runtime_db::{
        current_read_snapshot::*,
        evidence::{upsert_agent_identity_tx, upsert_agent_state_tx},
    },
    types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility,
        AuthorityClass, MessageBody, MessageKind, MessageOrigin, Priority, WaitConditionKind,
        WakeSource,
    },
};
struct Fixture {
    _home: tempfile::TempDir,
    db: RuntimeDb,
    scope: InputEvidenceScope,
    at: chrono::DateTime<Utc>,
    ledger: InputEvidenceBudgetLedger,
}
impl Fixture {
    fn new() -> Result<Self> {
        let home = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            home.path().join("runtime.sqlite"),
            home.path().join("runtime.lock"),
        )?;
        let scope = InputEvidenceScope {
            agent_id: "current-read-core".into(),
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
            _home: home,
            db,
            scope,
            at: "2026-10-14T00:00:00.123456789Z".parse()?,
            ledger: InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?,
        })
    }
    fn state(&self) -> Result<AgentState> {
        self.db
            .agent_states()
            .latest(&self.scope.agent_id)?
            .context("actual State missing")
    }
    fn request(&self) -> CurrentReadRequest {
        CurrentReadRequest::current(CURRENT_READ_MAX_BYTES, CURRENT_READ_MAX_RECORDS)
    }
    fn prepared(&self, nonce: &str) -> Result<PreparedInputTransition> {
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
                text: "actual original CurrentRead source".into(),
            },
        );
        message.id = format!("source:{nonce}");
        message.created_at = self.at;
        let before = self.state()?;
        let mut after = before.clone();
        after.pending += 1;
        let mut event = AuditEvent::legacy(
            "current_read_core_source",
            serde_json::json!({"source":message.id}),
        );
        event.id = format!("audit:{nonce}");
        event.created_at = self.at;
        PreparedInputTransition::queue(
            self.scope.clone(),
            nonce.into(),
            self.at,
            PreparedQueueTransition::new(QueueTransitionCommand {
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
                    expected: Some(Box::new(before)),
                    record: Box::new(after),
                }),
                message_evidence: vec![message],
                transcript_entries: Vec::new(),
                turn_record: None,
                audit_events: vec![event],
                notify_scheduler: true,
                fault: None,
                brief_evidence: Vec::new(),
            }),
        )
    }
    fn execute(
        &self,
        prepared: &PreparedInputTransition,
    ) -> Result<InputMutationExecution<TransitionCommit>> {
        self.db
            .transitions_in_scope(&self.scope)
            .commit_prepared_transition_with_domain_requests(
                prepared,
                &crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
                    capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
                    byte_limit: CURRENT_READ_MAX_BYTES,
                    record_limit: CURRENT_READ_MAX_RECORDS,
                },
                &self.ledger,
                &crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
                    capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
                    query: Default::default(),
                    byte_limit: CURRENT_READ_MAX_BYTES,
                    record_limit: CURRENT_READ_MAX_RECORDS,
                },
                &crate::runtime_db::scheduling_snapshot::SchedulingDomainLoadRequest {
                    capture: crate::runtime::resident_scheduling::SchedulingCaptureRequest::current(
                    ),
                    byte_limit: CURRENT_READ_MAX_BYTES,
                    record_limit: CURRENT_READ_MAX_RECORDS,
                },
            )
    }
    fn counts(&self) -> Result<(u64, u64, u64, u64, u64, u64)> {
        Ok(self.db.connection()?.query_row("SELECT (SELECT COUNT(*) FROM messages),(SELECT COUNT(*) FROM queue_entries),(SELECT COUNT(*) FROM audit_events),(SELECT COUNT(*) FROM current_read_members),(SELECT COUNT(*) FROM input_mutation_receipts),(SELECT COALESCE(SUM(last_value),0) FROM runtime_sequences)", [], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?)
    }
}
#[test]
fn original_prepared_core_keeps_exact_current_facts_receipt_and_no_unused_audit_alias() -> Result<()>
{
    let f = Fixture::new()?;
    let prepared = f.prepared("actual-original")?;
    let (value, receipt) = match f.execute(&prepared)? {
        InputMutationExecution::Committed { value, receipt } => (value, receipt),
        _ => anyhow::bail!("first original ACK missing"),
    };
    let facts = value
        .effects
        .current_read_facts
        .as_ref()
        .context("core omitted CurrentRead facts")?
        .clone();
    assert_eq!(facts.revision, facts.base_revision + 1);
    let captured = facts.snapshot.resident.capture(&f.request())?;
    assert!(captured.has_queued());
    assert_eq!(captured.events.len(), 1);
    let CurrentReadFact::Event(event) = &captured.events[0].fact else {
        unreachable!()
    };
    assert_eq!(event.id, "audit:actual-original");
    assert!(event.event_seq > 0);
    assert_eq!(receipt.outcome.scopes[0].audit_events[0], *event);
    assert_eq!(f.state()?.total_message_count, 1);
    let unused: u64 = f.db.connection()?.query_row(
        "SELECT COUNT(*) FROM current_read_members WHERE record_id LIKE 'audit:wait-trigger%'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(unused, 0);
    let before = f.counts()?;
    match f.execute(&prepared)? {
        InputMutationExecution::Replayed {
            receipt: replay, ..
        } => assert_eq!(replay, receipt),
        _ => anyhow::bail!("exact replay must precede new proof"),
    }
    assert_eq!(f.counts()?, before);
    let body = Arc::downgrade(&captured.events[0]);
    f.db.transaction(|tx| {tx.execute_batch("ALTER TABLE audit_events RENAME TO forbidden_current_read_events; ALTER TABLE queue_entries RENAME TO forbidden_current_read_queue")?;Ok(())})?;
    let pure = facts.snapshot.resident.capture(&f.request())?;
    assert!(Arc::ptr_eq(&pure.events[0], &captured.events[0]));
    drop((value, facts, pure));
    assert!(body.upgrade().is_some());
    drop(captured);
    assert!(body.upgrade().is_none());
    assert_eq!(f.ledger.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn receipt_insert_failure_rolls_back_core_queue_audit_sequence_and_current_revision() -> Result<()>
{
    let f = Fixture::new()?;
    let prepared = f.prepared("receipt-last")?;
    let before = f.counts()?;
    let state = f.state()?;
    f.db.transaction(|tx| {tx.execute_batch("CREATE TRIGGER current_read_receipt_fault BEFORE INSERT ON input_mutation_receipts BEGIN SELECT RAISE(ABORT,'actual receipt-last fault'); END")?;Ok(())})?;
    let error = f
        .execute(&prepared)
        .err()
        .context("real final receipt INSERT must fail")?;
    assert!(format!("{error:#}").contains("actual receipt-last fault"));
    assert_eq!(f.counts()?, before);
    assert_eq!(f.state()?, state);
    assert_eq!(
        f.db.connection()?
            .query_row("SELECT COUNT(*) FROM current_read_current", [], |r| r
                .get::<_, u64>(0))?,
        0
    );
    assert_eq!(f.ledger.report()?.observed_bytes, 0);
    f.db.transaction(|tx| {
        tx.execute_batch("DROP TRIGGER current_read_receipt_fault")?;
        Ok(())
    })?;
    assert!(matches!(
        f.execute(&prepared)?,
        InputMutationExecution::Committed { .. }
    ));
    Ok(())
}
#[test]
fn actual_wait_branch_uses_shared_stable_event_key_and_freezes_only_original_sources() -> Result<()>
{
    let f = Fixture::new()?;
    let condition = WaitConditionRecord {
        id: "wait:actual-operator".into(),
        agent_id: f.scope.agent_id.clone(),
        work_item_id: None,
        status: WaitConditionStatus::Active,
        kind: WaitConditionKind::External,
        source: None,
        subject_ref: None,
        waiting_for: "original operator input".into(),
        wake_sources: vec![WakeSource::OperatorInput],
        continuation: None,
        created_at: f.at,
        updated_at: f.at,
        expires_at: None,
        resolved_at: None,
        cancelled_at: None,
        turn_id: None,
        trigger_message_id: None,
        triggered_at: None,
    };
    let wait = PreparedInputTransition::wait(
        f.scope.clone(),
        "actual-wait-registration".into(),
        f.at,
        WaitTransitionCommand {
            agent_id: f.scope.agent_id.clone(),
            work_items: Vec::new(),
            expected_wait_conditions: Vec::new(),
            wait_conditions: vec![condition],
            timer_wake: None,
            task_result_admission: None,
            agent_state: None,
            audit_events: Vec::new(),
            index_changes: Vec::new(),
            notify_scheduler: true,
            fault: None,
        },
        Default::default(),
        None,
    )?;
    assert!(matches!(
        f.execute(&wait)?,
        InputMutationExecution::Committed { .. }
    ));
    let prepared = f.prepared("wait-source")?;
    let value = match f.execute(&prepared)? {
        InputMutationExecution::Committed { value, .. } => value,
        _ => anyhow::bail!("actual source ACK missing"),
    };
    let facts = value.effects.current_read_facts.as_ref().unwrap();
    let expected = wait_trigger_audit_id("source:wait-source", WaitTriggerAuditKind::Triggered);
    let events = facts.snapshot.resident.capture(&f.request())?.events;
    assert!(events.iter().any(|v| matches!(&v.fact,CurrentReadFact::Event(event) if event.id==expected && event.event_seq>0)));
    assert_eq!(
        f.db.connection()?.query_row(
            "SELECT COUNT(*) FROM current_read_members WHERE family='event' AND record_id=?1",
            [wait_trigger_audit_id(
                "source:wait-source",
                WaitTriggerAuditKind::Stale
            )],
            |r| r.get::<_, u64>(0)
        )?,
        0
    );
    let actual_wait = f.db.transaction(|tx| {
        let mut query = tx.prepare(
            "SELECT wait_condition_id, agent_id, work_item_id, status, kind, source,
                subject_ref, waiting_for, created_at, updated_at, expires_at,
                resolved_at, cancelled_at, last_turn_id, trigger_message_id, triggered_at,
                wake_sources_json, continuation_json FROM wait_conditions
             WHERE wait_condition_id=?1 AND agent_id=?2",
        )?;
        let mut rows = query.query(rusqlite::params!["wait:actual-operator", f.scope.agent_id])?;
        crate::runtime_db::repositories::decode_wait_condition_row(
            rows.next()?.context("actual triggered Wait row missing")?,
        )
    })?;
    assert_eq!(actual_wait.status, WaitConditionStatus::Triggered);
    drop((value, events));
    assert_eq!(f.ledger.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn concrete_writer_retains_final_actual_claim_and_cas_without_duplicate_revision() -> Result<()> {
    let f = Fixture::new()?;
    let initial = f.prepared("claim-source")?;
    drop(f.execute(&initial)?);
    let state = f.state()?;
    f.db.transaction(|tx| {
        let keys = BTreeSet::from([CurrentReadKey::queue("source:claim-source")]);
        let mut current = CurrentReadBusinessTransaction::prepare_tx(
            tx,
            &f.scope,
            &state,
            &keys,
            &f.request(),
            &f.ledger,
            (0, 0),
            Vec::new(),
        )?;
        let queued = crate::runtime_db::repositories::decode_queue_entry_payload(&tx.query_row(
            "SELECT payload_json FROM queue_entries WHERE message_id='source:claim-source'",
            [],
            |r| r.get::<_, String>(0),
        )?)?;
        assert!(current.claim_queue_tx(tx, &queued)?);
        // Use actual canonical readback rather than assuming the claim's clock.
        let claimed = crate::runtime_db::repositories::decode_queue_entry_payload(&tx.query_row(
            "SELECT payload_json FROM queue_entries WHERE message_id=?1",
            [&queued.message_id],
            |r| r.get::<_, String>(0),
        )?)?;
        let mut processed = claimed.clone();
        processed.status = QueueEntryStatus::Processed;
        processed.updated_at += chrono::Duration::seconds(1);
        assert!(current.compare_and_set_queue_tx(tx, &claimed, &processed)?);
        assert!(!current.upsert_queue_tx(tx, &processed)?);
        let facts = current.finish(&state)?;
        assert_eq!(facts.revision, facts.base_revision + 1);
        assert!(!facts.snapshot.resident.capture(&f.request())?.has_queued());
        Ok(())
    })?;
    assert_eq!(f.ledger.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn concrete_writer_rejects_wrong_transaction_unfrozen_address_and_preclone_budget() -> Result<()> {
    let f = Fixture::new()?;
    let other = Fixture::new()?;
    let state = f.state()?;
    f.db.transaction(|tx| {
        let mut event = AuditEvent::legacy("original", serde_json::json!({"original":true}));
        event.id = "frozen-original-event".into();
        let keys = BTreeSet::from([CurrentReadKey::event(&event.id)]);
        let mut current = CurrentReadBusinessTransaction::prepare_tx(
            tx,
            &f.scope,
            &state,
            &keys,
            &f.request(),
            &f.ledger,
            (0, 0),
            Vec::new(),
        )?;
        other.db.transaction(|foreign_tx| {
            assert!(current
                .append_event_tx(foreign_tx, Some(&f.scope.agent_id), &event)
                .is_err());
            Ok(())
        })?;
        event.id = "unfrozen-event".into();
        assert!(current
            .append_event_tx(tx, Some(&f.scope.agent_id), &event)
            .is_err());
        assert_eq!(
            tx.query_row("SELECT COUNT(*) FROM audit_events", [], |r| r
                .get::<_, u64>(0))?,
            0
        );
        Ok(())
    })?;
    let draft = f.prepared("oversized-footprint")?;
    let InputTransitionCommand::Queue(mut command) = *draft.command else {
        unreachable!()
    };
    command.command.audit_events[0].id = "x".repeat(CURRENT_READ_MAX_BYTES / 8);
    let prepared = PreparedInputTransition::queue(
        f.scope.clone(),
        "oversized-footprint".into(),
        f.at,
        command,
    )?;
    let before = f.counts()?;
    let error = f
        .execute(&prepared)
        .err()
        .context("combined footprint must reject before writes")?;
    assert!(
        matches!(
            error.downcast_ref::<CurrentReadError>(),
            Some(CurrentReadError::ResourceExhausted)
        ),
        "{error:#}"
    );
    assert_eq!(f.counts()?, before);
    assert_eq!(f.ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn original_timer_fresh_merge_and_cancel_keep_current_queue_in_the_same_whole_ack() -> Result<()> {
    use crate::{
        runtime_db::{TimerCancel, TimerFire},
        types::{TimerRecord, TimerStatus},
    };
    let f = Fixture::new()?;
    let timer = TimerRecord {
        id: "timer:current-read".into(),
        agent_id: f.scope.agent_id.clone(),
        created_at: f.at,
        duration_ms: 1000,
        interval_ms: Some(1000),
        repeat: true,
        status: TimerStatus::Active,
        summary: Some("actual Timer".into()),
        next_fire_at: Some(f.at + chrono::Duration::seconds(1)),
        last_fired_at: None,
        fire_count: 0,
    };
    let freeze = |nonce: &str, command: PreparedTimerTransition| {
        PreparedInputTransition::timer(f.scope.clone(), nonce.into(), f.at, command)
    };
    drop(f.execute(&freeze(
        "timer-create",
        PreparedTimerTransition::create(timer, None),
    )?)?);
    let fire = |id: &str| -> Result<TimerFire> {
        let expected = f.db.timers().latest("timer:current-read")?.unwrap();
        let mut record = expected.clone();
        record.fire_count += 1;
        record.last_fired_at = Some(f.at + chrono::Duration::seconds(record.fire_count as i64));
        let mut message = MessageEnvelope::new(
            &f.scope.agent_id,
            MessageKind::TimerTick,
            MessageOrigin::Timer {
                timer_id: record.id.clone(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Next,
            MessageBody::Text {
                text: "original timer wake".into(),
            },
        );
        message.id = id.into();
        message.created_at = f.at;
        message.metadata =
            Some(serde_json::json!({"timer_id":record.id,"timer_fire_count":record.fire_count}));
        let before = f.state()?;
        let mut after = before.clone();
        after.pending += 1;
        after.total_message_count += 1;
        let queue = QueueEntryRecord {
            message_id: message.id.clone(),
            agent_id: f.scope.agent_id.clone(),
            priority: message.priority.clone(),
            status: QueueEntryStatus::Queued,
            created_at: f.at,
            updated_at: f.at,
        };
        Ok(TimerFire {
            expected,
            record,
            message,
            queue_entry: queue,
            agent_state: (before, after),
        })
    };
    let first = freeze(
        "timer-fire",
        PreparedTimerTransition::fire(fire("wake:current-read")?, None, None),
    )?;
    let first_value = match f.execute(&first)? {
        InputMutationExecution::Committed { value, .. } => value,
        _ => anyhow::bail!("fresh Timer ACK missing"),
    };
    let first_revision = first_value
        .effects
        .current_read_facts
        .as_ref()
        .unwrap()
        .revision;
    assert!(first_value
        .effects
        .current_read_facts
        .as_ref()
        .unwrap()
        .snapshot
        .resident
        .capture(&f.request())?
        .has_queued());
    let pending = f.db.timers().pending_wake("timer:current-read")?.unwrap();
    let merge = freeze(
        "timer-merge",
        PreparedTimerTransition::fire(fire("unused:merge-wake")?, Some(pending.clone()), None),
    )?;
    let merge_value = match f.execute(&merge)? {
        InputMutationExecution::Committed { value, .. } => value,
        _ => anyhow::bail!("merge ACK missing"),
    };
    assert_eq!(
        merge_value
            .effects
            .current_read_facts
            .as_ref()
            .unwrap()
            .revision,
        first_revision
    );
    assert!(f.db.queue_entries().latest("unused:merge-wake")?.is_none());
    assert!(f
        .db
        .messages()
        .by_id(Some(&f.scope.agent_id), "unused:merge-wake")?
        .is_none());
    let expected = f.db.timers().latest("timer:current-read")?.unwrap();
    let mut record = expected.clone();
    record.status = TimerStatus::Cancelled;
    record.next_fire_at = None;
    let before = f.state()?;
    let mut after = before.clone();
    after.pending = 0;
    let cancel = freeze(
        "timer-cancel",
        PreparedTimerTransition::cancel(
            TimerCancel {
                expected,
                record,
                agent_state: Some((before, after)),
            },
            Some(pending),
            None,
        ),
    )?;
    let (cancel_value, receipt) = match f.execute(&cancel)? {
        InputMutationExecution::Committed { value, receipt } => (value, receipt),
        _ => anyhow::bail!("cancel ACK missing"),
    };
    let current = cancel_value.effects.current_read_facts.as_ref().unwrap();
    assert_eq!(current.revision, first_revision + 1);
    assert!(!current
        .snapshot
        .resident
        .capture(&f.request())?
        .has_queued());
    assert_eq!(
        f.db.queue_entries()
            .latest("wake:current-read")?
            .unwrap()
            .status,
        QueueEntryStatus::Dropped
    );
    let before = f.counts()?;
    match f.execute(&cancel)? {
        InputMutationExecution::Replayed { receipt: again, .. } => assert_eq!(again, receipt),
        _ => anyhow::bail!("cancel exact replay missing"),
    };
    assert_eq!(f.counts()?, before);
    drop((first_value, merge_value, cancel_value));
    assert_eq!(f.ledger.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn original_prepared_input_native_birth_is_receipted_once_and_rolls_back_with_business(
) -> Result<()> {
    let f = Fixture::new()?;
    let mut prepared = f.prepared("input-native-original")?;
    let InputTransitionCommand::Queue(queue) = prepared.command.as_mut() else {
        unreachable!()
    };
    queue.command.fault = Some(TransitionFaultPoint::BeforeCommit);
    assert!(f.execute(&prepared).is_err());
    assert_eq!(
        f.db.connection()?.query_row(
            "SELECT COUNT(*) FROM input_query_birth_cursors",
            [],
            |r| r.get::<_, u64>(0)
        )?,
        0
    );
    assert_eq!(
        f.db.connection()?.query_row(
            "SELECT COUNT(*) FROM input_query_birth_members",
            [],
            |r| r.get::<_, u64>(0)
        )?,
        0
    );
    assert_eq!(f.ledger.report()?.observed_bytes, 0);
    let InputTransitionCommand::Queue(queue) = prepared.command.as_mut() else {
        unreachable!()
    };
    queue.command.fault = None;
    let (value, receipt) = match f.execute(&prepared)? {
        InputMutationExecution::Committed { value, receipt } => (value, receipt),
        _ => anyhow::bail!("first actual Input ACK missing"),
    };
    let actual = value
        .effects
        .input_evidence_delta
        .records
        .iter()
        .find(|row| {
            row.locator.key.family
                == crate::runtime::resident_input_evidence::InputEvidenceFamily::Message
                && row.locator.key.id == "source:input-native-original"
        })
        .context("original Message readback missing")?;
    assert!(actual.locator.revision > 0);
    assert_eq!(f.db.connection()?.query_row("SELECT high_water FROM input_query_birth_cursors WHERE agent_id=?1 AND identity_incarnation=?2",params![f.scope.agent_id,f.scope.identity_incarnation],|r|r.get::<_,u64>(0))?,1);
    assert_eq!(f.db.connection()?.query_row("SELECT birth_seq FROM input_query_birth_members WHERE agent_id=?1 AND identity_incarnation=?2 AND family='message' AND record_id='source:input-native-original'",params![f.scope.agent_id,f.scope.identity_incarnation],|r|r.get::<_,u64>(0))?,1);
    let before = f.counts()?;
    let replay = match f.execute(&prepared)? {
        InputMutationExecution::Replayed { receipt, .. } => receipt,
        _ => anyhow::bail!("original whole receipt did not replay"),
    };
    assert_eq!(replay, receipt);
    assert_eq!(f.counts()?, before);
    assert_eq!(
        f.db.connection()?.query_row(
            "SELECT high_water FROM input_query_birth_cursors",
            [],
            |r| r.get::<_, u64>(0)
        )?,
        1
    );
    drop(value);
    assert_eq!(f.ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn actual_prepared_input_never_adopts_a_late_raw_row_as_native_birth() -> Result<()> {
    let f = Fixture::new()?;
    let prepared = f.prepared("late-raw-input-native")?;
    let InputTransitionCommand::Queue(queue) = prepared.command.as_ref() else {
        unreachable!()
    };
    f.db.transaction(|tx| {
        crate::runtime_db::evidence::append_message_tx(tx, &queue.command.message_evidence[0])?;
        Ok(())
    })?;
    let before = f.counts()?;
    let error = f
        .execute(&prepared)
        .err()
        .context("late raw Message unexpectedly qualified")?;
    assert!(matches!(error.downcast_ref::<crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError>(),
        Some(crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError::NeedsPreparation(_))),"{error:#}");
    assert_eq!(f.counts()?, before);
    assert_eq!(
        f.db.connection()?.query_row(
            "SELECT COUNT(*) FROM input_query_birth_members",
            [],
            |r| r.get::<_, u64>(0)
        )?,
        0
    );
    assert_eq!(
        f.db.connection()?.query_row(
            "SELECT COUNT(*) FROM input_evidence_current WHERE family='message' AND record_id=?1",
            [&queue.command.message_evidence[0].id],
            |r| r.get::<_, u64>(0)
        )?,
        0
    );
    assert_eq!(f.ledger.report()?.observed_bytes, 0);
    Ok(())
}
