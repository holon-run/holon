use super::*;
use crate::{
    runtime_db::{
        evidence::{
            append_audit_event_tx, append_message_tx, insert_runtime_index_changes_tx,
            upsert_agent_state_tx,
        },
        repositories::upsert_turn_record_tx,
        RuntimeDb, RuntimeIndexChange, RuntimeIndexOperation,
    },
    types::{
        AgentState, AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin,
        Priority, TurnRecord,
    },
};

struct Fixture {
    _directory: tempfile::TempDir,
    db: RuntimeDb,
    state: AgentState,
    time: DateTime<Utc>,
}
impl Fixture {
    fn new() -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            directory.path().join("runtime.sqlite"),
            directory.path().join("runtime.lock"),
        )?;
        let state = AgentState::new("agent-a");
        db.agent_states().upsert_for_fixture(&state)?;
        Ok(Self {
            _directory: directory,
            db,
            state,
            time: "2026-10-13T00:00:00.123456789Z".parse()?,
        })
    }
    fn prepared(&self) -> Result<PreparedInputMutation> {
        PreparedInputMutation::new(
            InputMutationFamily::Queue,
            "original-queue-command".into(),
            vec![scope("agent-a", 1)],
            canonical_command_digest(
                &serde_json::json!({"message":"message-original","expected_pending":0,"time":self.time}),
            )?,
            self.time,
        )
    }
    fn count(&self, table: &str) -> Result<i64> {
        Ok(self
            .db
            .connection()?
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?)
    }
    fn apply_original_tx(
        &self,
        tx: &Transaction<'_>,
        prepared: &PreparedInputMutation,
    ) -> Result<InputMutationOutcome> {
        let mut message = MessageEnvelope::new(
            "agent-a",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "original body".into(),
            },
        );
        message.id = "message-original".into();
        message.created_at = self.time;
        let (message, inserted) = append_message_tx(tx, &message)?;
        ensure!(inserted && message.message_seq.is_some());
        let mut turn = TurnRecord::new("agent-a", "turn-original", 1);
        turn.created_at = self.time;
        turn.input_message_ids.push(message.id.clone());
        upsert_turn_record_tx(tx, &turn)?;
        let mut state = self.state.clone();
        state.current_turn_id = Some(turn.turn_id);
        state.turn_index = 1;
        // Actual same-view count epilogue, not a preSQL estimate.
        state.total_message_count = tx.query_row(
            "SELECT COUNT(*) FROM messages WHERE agent_id='agent-a'",
            [],
            |r| r.get(0),
        )?;
        upsert_agent_state_tx(tx, &state)?;
        let mut event = AuditEvent::legacy(
            "original_receipt_fixture",
            serde_json::json!({"message_id":message.id}),
        );
        event.id = "audit-original".into();
        event.created_at = self.time;
        let (event, inserted) = append_audit_event_tx(tx, Some("agent-a"), &event)?;
        ensure!(inserted);
        insert_runtime_index_changes_tx(
            tx,
            &[RuntimeIndexChange {
                agent_id: "agent-a".into(),
                source_kind: "message".into(),
                source_id: message.id.clone(),
                source_ref: format!("message:{}", message.id),
                operation: RuntimeIndexOperation::Upsert,
                source_updated_at: Some(self.time),
                reason: "receipt_fixture".into(),
            }],
        )?;
        Ok(InputMutationOutcome {
            result: Default::default(),
            applied: true,
            delivery_receipt: None,
            scopes: vec![InputMutationScopeOutcome {
                install_keys: Default::default(),
                scope: prepared.scopes()[0].clone(),
                audit_events: vec![crate::runtime_db::transitions::AuditEventAck::new(event)?],
                notify_memory_index: true,
                notify_scheduler: true,
            }],
        })
    }
}
fn scope(agent: &str, inc: u64) -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: agent.into(),
        identity_incarnation: inc,
    }
}
fn noop(prepared: &PreparedInputMutation) -> InputMutationOutcome {
    InputMutationOutcome {
        result: Default::default(),
        applied: false,
        delivery_receipt: None,
        scopes: prepared
            .scopes()
            .iter()
            .map(|scope| InputMutationScopeOutcome {
                install_keys: Default::default(),
                scope: scope.clone(),
                audit_events: Vec::new(),
                notify_memory_index: false,
                notify_scheduler: false,
            })
            .collect(),
    }
}

#[test]
fn descriptor_validates_sha_nonce_unique_nonempty_current_scopes() -> Result<()> {
    let f = Fixture::new()?;
    for (nonce, scopes, digest) in [
        (" ", vec![scope("agent-a", 1)], "a".repeat(64)),
        ("command", Vec::new(), "a".repeat(64)),
        ("command", vec![scope("agent-a", 0)], "a".repeat(64)),
        ("command", vec![scope("", 1)], "a".repeat(64)),
        (
            "command",
            vec![scope("agent-a", 1), scope("agent-a", 1)],
            "a".repeat(64),
        ),
        (
            "command",
            vec![scope("agent-a", 1)],
            "Debug(command)".into(),
        ),
        ("command", vec![scope("agent-a", 1)], "G".repeat(64)),
    ] {
        assert!(PreparedInputMutation::new(
            InputMutationFamily::Queue,
            nonce.into(),
            scopes,
            digest,
            f.time
        )
        .is_err());
    }
    let a = PreparedInputMutation::new(
        InputMutationFamily::Queue,
        "a".into(),
        vec![scope("b", 2), scope("a", 1)],
        "a".repeat(64),
        f.time,
    )?;
    let b = PreparedInputMutation::new(
        InputMutationFamily::Queue,
        "b".into(),
        vec![scope("a", 1), scope("b", 2)],
        "a".repeat(64),
        f.time,
    )?;
    assert_eq!(a.scopes(), &[scope("a", 1), scope("b", 2)]);
    assert_eq!(
        a.scope_digest(),
        b.scope_digest(),
        "nonce is a separate unique address"
    );
    let changed = PreparedInputMutation::new(
        InputMutationFamily::Terminal,
        "a".into(),
        a.scopes().to_vec(),
        "a".repeat(64),
        f.time,
    )?;
    assert_ne!(a.scope_digest(), changed.scope_digest());
    Ok(())
}

#[test]
fn complete_compound_write_and_receipt_roll_back_together() -> Result<()> {
    for fail_after_receipt in [false, true] {
        let f = Fixture::new()?;
        let prepared = f.prepared()?;
        let tables = [
            "messages",
            "turn_records",
            "conversation_input_assignments",
            "audit_events",
            "runtime_index_outbox",
            "input_mutation_receipts",
        ];
        let before = tables
            .iter()
            .map(|table| f.count(table))
            .collect::<Result<Vec<_>>>()?;
        let result: Result<()> = f.db.transaction(|tx| {
            assert!(lookup_input_mutation_receipt_tx(tx, &prepared)?.is_none());
            let outcome = f.apply_original_tx(tx, &prepared)?;
            if fail_after_receipt {
                insert_input_mutation_receipt_tx(tx, &prepared, &outcome, f.time)?;
            }
            anyhow::bail!("original whole-transaction rollback fixture")
        });
        assert!(result.is_err());
        assert_eq!(
            f.db.agent_states().latest("agent-a")?,
            Some(f.state.clone())
        );
        assert_eq!(
            tables
                .iter()
                .map(|table| f.count(table))
                .collect::<Result<Vec<_>>>()?,
            before
        );
        let attempt = OriginalInputMutationAttempt::new(prepared);
        attempt.finish_original_attempt();
        with_fresh_input_mutation_resolution(
            &mut f.db.connection()?,
            &attempt,
            |_, resolution| {
                assert_eq!(resolution, InputMutationResolution::Absent);
                Ok(())
            },
        )?;
    }
    Ok(())
}

#[test]
fn noop_is_a_committed_original_result_and_replay_does_not_apply_again() -> Result<()> {
    let f = Fixture::new()?;
    let prepared = f.prepared()?;
    let outcome = noop(&prepared);
    let receipt =
        f.db.transaction(|tx| insert_input_mutation_receipt_tx(tx, &prepared, &outcome, f.time))?;
    assert!(!receipt.outcome.applied);
    assert_eq!(f.count("input_mutation_receipts")?, 1);
    let returned = f.db.transaction(|tx| {
        if let Some(receipt) = lookup_input_mutation_receipt_tx(tx, &prepared)? {
            return Ok(receipt);
        }
        panic!("replay must precede even a stale business OCC predicate")
    })?;
    assert_eq!(returned, receipt);
    assert_eq!(
        f.db.agent_states().latest("agent-a")?,
        Some(f.state.clone())
    );
    let mut changed = outcome;
    changed.applied = true;
    assert!(f
        .db
        .transaction(|tx| insert_input_mutation_receipt_tx(tx, &prepared, &changed, f.time))
        .is_err());
    Ok(())
}

#[test]
fn same_nonce_rejects_digest_family_and_complete_group_changes() -> Result<()> {
    let f = Fixture::new()?;
    let prepared = f.prepared()?;
    f.db.transaction(|tx| {
        insert_input_mutation_receipt_tx(tx, &prepared, &noop(&prepared), f.time)
    })?;
    for (family, scopes, digest) in [
        (
            prepared.family(),
            prepared.scopes().to_vec(),
            "f".repeat(64),
        ),
        (
            InputMutationFamily::Terminal,
            prepared.scopes().to_vec(),
            prepared.command_digest().into(),
        ),
        (
            prepared.family(),
            vec![scope("agent-a", 2)],
            prepared.command_digest().into(),
        ),
        (
            prepared.family(),
            vec![scope("agent-a", 1), scope("agent-b", 1)],
            prepared.command_digest().into(),
        ),
    ] {
        let changed = PreparedInputMutation::new(
            family,
            prepared.command_nonce().into(),
            scopes,
            digest,
            f.time,
        )?;
        assert!(f
            .db
            .transaction(|tx| lookup_input_mutation_receipt_tx(tx, &changed))
            .is_err());
    }
    Ok(())
}

#[test]
fn unfinished_or_same_key_new_attempt_cannot_borrow_original_completion() -> Result<()> {
    let f = Fixture::new()?;
    let original = OriginalInputMutationAttempt::new(f.prepared()?);
    let saved = original.clone();
    let other = OriginalInputMutationAttempt::new(f.prepared()?);
    let mut connection = f.db.connection()?;
    let error = with_fresh_input_mutation_resolution(&mut connection, &original, |_, _| Ok(()))
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<ReceiptError>(),
        Some(ReceiptError::OriginalAttemptNotFinished)
    ));
    original.finish_original_attempt();
    assert!(saved.original_attempt_finished() && saved.same_attempt(&original));
    assert!(!other.original_attempt_finished() && !other.same_attempt(&original));
    assert!(with_fresh_input_mutation_resolution(&mut connection, &other, |_, _| Ok(())).is_err());
    with_fresh_input_mutation_resolution(&mut connection, &saved, |_, resolution| {
        assert_eq!(resolution, InputMutationResolution::Absent);
        Ok(())
    })?;
    Ok(())
}

#[test]
fn fresh_preparation_reserves_writer_before_receipt_and_releases_on_error() -> Result<()> {
    let f = Fixture::new()?;
    let prepared = f.prepared()?;
    let attempt = OriginalInputMutationAttempt::new(prepared.clone());
    f.db.transaction(|tx| {
        insert_input_mutation_receipt_tx(tx, &prepared, &noop(&prepared), f.time)
    })?;
    let mut connection = f.db.connection()?;
    let other = f.db.connection()?;
    other.busy_timeout(std::time::Duration::ZERO)?;
    let preflight_called = std::cell::Cell::new(false);
    let error = with_fresh_input_mutation_preparation_and_preflight(
        &mut connection,
        &attempt,
        |_| {
            preflight_called.set(true);
            Ok(())
        },
        |_, _| Ok(()),
    )
    .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<ReceiptError>(),
        Some(ReceiptError::OriginalAttemptNotFinished)
    ));
    assert!(!preflight_called.get() && connection.is_autocommit());
    attempt.finish_original_attempt();
    let mut later = f.state.clone();
    later.turn_index = 9;
    for fail in [true, false] {
        let result = with_fresh_input_mutation_preparation_and_preflight(
            &mut connection,
            &attempt,
            |tx| {
                let count: usize =
                    tx.query_row("SELECT COUNT(*) FROM input_mutation_receipts", [], |r| {
                        r.get(0)
                    })?;
                assert_eq!(count, 1);
                // The receipt snapshot already exists. A competing commit here
                // would make subsequent qualification writes BUSY_SNAPSHOT.
                let error = other
                    .execute(
                        "UPDATE agent_states SET payload_json=payload_json WHERE agent_id='agent-a'",
                        [],
                    )
                    .unwrap_err();
                assert_eq!(
                    error.sqlite_error_code(),
                    Some(rusqlite::ErrorCode::DatabaseBusy)
                );
                Ok(())
            },
            |tx, resolution| {
                let InputMutationResolution::Present(receipt) = resolution else {
                    panic!("original receipt disappeared")
                };
                assert!(!receipt.outcome.applied);
                upsert_agent_state_tx(tx, &later)?;
                if fail {
                    anyhow::bail!("qualification preparation fault");
                }
                Ok(())
            },
        );
        if fail {
            assert!(result
                .unwrap_err()
                .to_string()
                .contains("qualification preparation fault"));
        } else {
            result?;
        }
        assert!(connection.is_autocommit());
        assert_eq!(
            f.db.agent_states().latest("agent-a")?.unwrap(),
            if fail { f.state.clone() } else { later.clone() }
        );
        assert_eq!(
            other.execute(
                "UPDATE agent_states SET payload_json=payload_json WHERE agent_id='agent-a'",
                [],
            )?,
            1
        );
        assert_eq!(f.count("input_mutation_receipts")?, 1);
        assert_eq!(f.count("messages")?, 0);
        assert_eq!(f.count("audit_events")?, 0);
    }
    Ok(())
}

#[test]
fn fresh_receipt_observation_keeps_independent_deferred_snapshot() -> Result<()> {
    let f = Fixture::new()?;
    let attempt = OriginalInputMutationAttempt::new(f.prepared()?);
    attempt.finish_original_attempt();
    let other = f.db.connection()?;
    other.busy_timeout(std::time::Duration::ZERO)?;
    let mut later = f.state.clone();
    later.turn_index = 9;
    let mut connection = f.db.connection()?;
    with_fresh_input_mutation_resolution_and_preflight(
        &mut connection,
        &attempt,
        |tx| {
            let payload: String = tx.query_row(
                "SELECT payload_json FROM agent_states WHERE agent_id='agent-a'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(serde_json::from_str::<AgentState>(&payload)?, f.state);
            // A pure observer must not reserve the writer slot.
            other.execute(
                "UPDATE agent_states SET payload_json=?1 WHERE agent_id='agent-a'",
                [serde_json::to_string(&later)?],
            )?;
            Ok(())
        },
        |tx, resolution| {
            assert_eq!(resolution, InputMutationResolution::Absent);
            let payload: String = tx.query_row(
                "SELECT payload_json FROM agent_states WHERE agent_id='agent-a'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(serde_json::from_str::<AgentState>(&payload)?, f.state);
            Ok(())
        },
    )?;
    assert!(connection.is_autocommit());
    assert_eq!(f.db.agent_states().latest("agent-a")?.unwrap(), later);
    Ok(())
}

#[test]
fn lost_ack_proves_original_outcome_and_reads_later_state_without_rewinding() -> Result<()> {
    let f = Fixture::new()?;
    let prepared = f.prepared()?;
    let attempt = OriginalInputMutationAttempt::new(prepared.clone());
    // The SQL really commits; its return value is deliberately discarded.
    f.db.transaction(|tx| {
        let outcome = f.apply_original_tx(tx, &prepared)?;
        insert_input_mutation_receipt_tx(tx, &prepared, &outcome, f.time)
    })?;
    attempt.finish_original_attempt();
    let mut later = f.db.agent_states().latest("agent-a")?.unwrap();
    later.turn_index = 9;
    f.db.transaction(|tx| upsert_agent_state_tx(tx, &later))?;
    // Retention may remove the original canonical audit row; the receipt keeps
    // the exact payload for notification reissue, without re-appending audit SQL.
    f.db.transaction(|tx| {
        tx.execute(
            "DELETE FROM audit_events WHERE audit_event_id='audit-original'",
            [],
        )?;
        Ok(())
    })?;
    with_fresh_input_mutation_resolution(&mut f.db.connection()?, &attempt, |tx, resolution| {
        let InputMutationResolution::Present(receipt) = resolution else {
            panic!("committed original receipt")
        };
        assert!(receipt.outcome.applied);
        let event = &receipt.outcome.scopes[0].audit_events[0];
        assert_eq!(event.id, "audit-original");
        assert!(event.event_seq > 0 && !event.event_log_epoch.is_empty());
        let payload: String = tx.query_row(
            "SELECT payload_json FROM agent_states WHERE agent_id='agent-a'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(serde_json::from_str::<AgentState>(&payload)?, later);
        assert_eq!(later.total_message_count, 1);
        Ok(())
    })?;
    assert_eq!(f.count("messages")?, 1);
    assert_eq!(f.count("input_mutation_receipts")?, 1);
    assert_eq!(f.count("audit_events")?, 0);
    Ok(())
}

#[test]
fn fresh_resolution_does_not_reuse_an_older_sqlite_read_snapshot() -> Result<()> {
    let f = Fixture::new()?;
    let prepared = f.prepared()?;
    let mut connection = f.db.connection()?;
    {
        let old = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        assert!(lookup_input_mutation_receipt_tx(&old, &prepared)?.is_none());
        f.db.transaction(|tx| {
            insert_input_mutation_receipt_tx(tx, &prepared, &noop(&prepared), f.time)
        })?;
        assert!(
            lookup_input_mutation_receipt_tx(&old, &prepared)?.is_none(),
            "old readview remains stale"
        );
        old.commit()?;
    }
    let attempt = OriginalInputMutationAttempt::new(prepared);
    attempt.finish_original_attempt();
    with_fresh_input_mutation_resolution(&mut connection, &attempt, |_, resolution| {
        assert!(matches!(resolution, InputMutationResolution::Present(_)));
        Ok(())
    })
}

#[test]
fn original_group_outcome_requires_every_member_and_keeps_each_effects() -> Result<()> {
    let f = Fixture::new()?;
    let prepared = PreparedInputMutation::new(
        InputMutationFamily::Queue,
        "multi-owner-original".into(),
        vec![scope("agent-b", 2), scope("agent-a", 1)],
        "a".repeat(64),
        f.time,
    )?;
    let mut outcome = noop(&prepared);
    outcome.scopes[0].notify_memory_index = true;
    outcome.scopes[1].notify_scheduler = true;
    let receipt =
        f.db.transaction(|tx| insert_input_mutation_receipt_tx(tx, &prepared, &outcome, f.time))?;
    assert!(
        receipt.outcome.scopes[0].notify_memory_index && receipt.outcome.scopes[1].notify_scheduler
    );
    let mut incomplete = outcome;
    incomplete.scopes.pop();
    assert!(f
        .db
        .transaction(|tx| insert_input_mutation_receipt_tx(tx, &prepared, &incomplete, f.time))
        .is_err());
    Ok(())
}

#[test]
fn normalized_projection_corruption_and_non_actual_audit_are_rejected() -> Result<()> {
    let f = Fixture::new()?;
    let prepared = f.prepared()?;
    let mut fake = noop(&prepared);
    fake.scopes[0]
        .audit_events
        .push(crate::runtime_db::transitions::AuditEventAck::new(
            AuditEvent::legacy("unallocated", serde_json::json!({})),
        )?);
    assert!(f
        .db
        .transaction(|tx| insert_input_mutation_receipt_tx(tx, &prepared, &fake, f.time))
        .is_err());
    f.db.transaction(|tx| {
        insert_input_mutation_receipt_tx(tx, &prepared, &noop(&prepared), f.time)
    })?;
    f.db.transaction(|tx| {
        tx.execute(
            "UPDATE input_mutation_receipts SET scope_digest=?1 WHERE command_nonce=?2",
            params!["f".repeat(64), prepared.command_nonce()],
        )?;
        Ok(())
    })?;
    assert!(f
        .db
        .transaction(|tx| lookup_input_mutation_receipt_tx(tx, &prepared))
        .is_err());
    Ok(())
}

#[test]
fn ignored_receipt_insert_rolls_back_its_original_compound_writes() -> Result<()> {
    let f = Fixture::new()?;
    let prepared = f.prepared()?;
    f.db.connection()?.execute_batch("CREATE TRIGGER ignore_original_receipt BEFORE INSERT ON input_mutation_receipts BEGIN SELECT RAISE(IGNORE); END;")?;
    assert!(f
        .db
        .transaction(|tx| {
            let outcome = f.apply_original_tx(tx, &prepared)?;
            insert_input_mutation_receipt_tx(tx, &prepared, &outcome, f.time)
        })
        .is_err());
    assert_eq!(
        f.db.agent_states().latest("agent-a")?,
        Some(f.state.clone())
    );
    for table in [
        "messages",
        "turn_records",
        "conversation_input_assignments",
        "audit_events",
        "runtime_index_outbox",
        "input_mutation_receipts",
    ] {
        assert_eq!(f.count(table)?, 0, "{table}");
    }
    Ok(())
}

#[test]
fn actual_delivery_outcome_is_bound_once_and_preserved_after_native_state_advances() -> Result<()> {
    use crate::runtime_db::agent_message_delivery::insert_delivery_tx;
    let f = Fixture::new()?;
    let prepared = f.prepared()?;
    let record: crate::types::AgentMessageDeliveryRecord = serde_json::from_value(
        serde_json::json!({
            "delivery_id":"delivery-original","target_agent_id":"agent-a",
            "idempotency_scope":"caller-to-agent-a","idempotency_key_digest":"a".repeat(64),"request_digest":"b".repeat(64),
            "caller":{"caller_principal":"runtime:receipt-fixture","principal_kind":"runtime_capability","route":"agent_invocation",
                "origin":{"kind":"system","subsystem":"receipt-fixture"},"authority_class":"runtime_instruction",
                "delivery_surface":"runtime_system","admission_context":"runtime_owned"},
            "outcome":"rejected","state":"rejected","state_version":1,
            "admission_evidence":{"principal_kind":"runtime_capability","route":"agent_invocation"},
            "rejection_code":"agent_deleted","retryable":false,"terminal_at":f.time,"created_at":f.time,"updated_at":f.time,
        }),
    )?;
    let mut outcome = noop(&prepared);
    outcome.delivery_receipt = Some(record.receipt(false));
    let receipt = f.db.transaction(|tx| {
        insert_delivery_tx(tx, &record)?;
        insert_input_mutation_receipt_tx(tx, &prepared, &outcome, f.time)
    })?;
    let mut wrong = outcome.clone();
    wrong.delivery_receipt.as_mut().unwrap().retryable = true;
    let other = PreparedInputMutation::new(
        prepared.family(),
        "another-command".into(),
        prepared.scopes().to_vec(),
        prepared.command_digest().into(),
        f.time,
    )?;
    assert!(f
        .db
        .transaction(|tx| insert_input_mutation_receipt_tx(tx, &other, &wrong, f.time))
        .is_err());
    // Later native delivery data is not used to rewrite the original outcome.
    f.db.transaction(|tx| {
        tx.execute(
            "DELETE FROM agent_message_deliveries WHERE delivery_id=?1",
            [&record.delivery_id],
        )?;
        Ok(())
    })?;
    let attempt = OriginalInputMutationAttempt::new(prepared);
    attempt.finish_original_attempt();
    with_fresh_input_mutation_resolution(&mut f.db.connection()?, &attempt, |_, resolution| {
        assert_eq!(resolution, InputMutationResolution::Present(receipt));
        Ok(())
    })
}

#[test]
fn result_checksum_detects_later_timestamp_or_payload_corruption() -> Result<()> {
    for column in ["committed_at", "outcome_json"] {
        let f = Fixture::new()?;
        let prepared = f.prepared()?;
        let outcome = noop(&prepared);
        f.db.transaction(|tx| insert_input_mutation_receipt_tx(tx, &prepared, &outcome, f.time))?;
        let replacement = if column == "committed_at" {
            "2030-01-01T00:00:00Z".into()
        } else {
            let mut corrupted = outcome.clone();
            corrupted.applied = true;
            serde_json::to_string(&corrupted)?
        };
        f.db.transaction(|tx| {
            tx.execute(
                &format!("UPDATE input_mutation_receipts SET {column}=?1 WHERE command_nonce=?2"),
                params![replacement, prepared.command_nonce()],
            )?;
            Ok(())
        })?;
        assert!(f
            .db
            .transaction(|tx| lookup_input_mutation_receipt_tx(tx, &prepared))
            .is_err());
    }
    Ok(())
}

#[test]
fn task_result_successful_empty_outcomes_replay_original_after_later_state() -> Result<()> {
    use crate::runtime_db::task_result_settlement::TaskResultWriteOutcome;
    let f = Fixture::new()?;
    let values = [
        TaskResultWriteOutcome::Record(None),
        TaskResultWriteOutcome::Records(Vec::new()),
        TaskResultWriteOutcome::Changed(false),
        TaskResultWriteOutcome::Count(0),
        TaskResultWriteOutcome::Unit,
    ];
    let mut originals = Vec::new();
    for (index, value) in values.into_iter().enumerate() {
        let prepared = PreparedInputMutation::new(
            InputMutationFamily::TaskResult,
            format!("original-result-{index}"),
            vec![scope("agent-a", 1)],
            canonical_command_digest(&serde_json::json!({"operation":index,"time":f.time}))?,
            f.time,
        )?;
        let mut outcome = noop(&prepared);
        outcome.result = InputMutationResult::TaskResult {
            outcome: Box::new(value),
        };
        f.db.transaction(|tx| {
            assert!(lookup_input_mutation_receipt_tx(tx, &prepared)?.is_none());
            insert_input_mutation_receipt_tx(tx, &prepared, &outcome, f.time)?;
            Ok(())
        })?;
        originals.push((prepared, outcome));
    }
    let mut later = f.state.clone();
    later.pending = 7;
    f.db.agent_states().upsert_for_fixture(&later)?;
    for (prepared, outcome) in originals {
        let attempt = OriginalInputMutationAttempt::new(prepared);
        attempt.finish_original_attempt();
        with_fresh_input_mutation_resolution(
            &mut f.db.connection()?,
            &attempt,
            |_, resolution| {
                let InputMutationResolution::Present(receipt) = resolution else {
                    anyhow::bail!("successful empty TaskResult operation lost its receipt")
                };
                assert_eq!(receipt.outcome, outcome);
                assert!(!receipt.outcome.applied);
                assert_eq!(receipt.committed_at, f.time);
                Ok(())
            },
        )?;
    }
    assert_eq!(f.db.agent_states().latest("agent-a")?, Some(later));
    assert_eq!(f.count("input_mutation_receipts")?, 5);
    Ok(())
}

#[test]
fn task_result_receipt_rejects_wrong_family_and_foreign_original_result() -> Result<()> {
    use crate::runtime_db::task_result_settlement::{
        TaskResultSettlementRecord, TaskResultSettlementState, TaskResultWriteOutcome,
    };
    let f = Fixture::new()?;
    let prepared = PreparedInputMutation::new(
        InputMutationFamily::TaskResult,
        "original-result-invalid".into(),
        vec![scope("agent-a", 1)],
        canonical_command_digest(&serde_json::json!({"operation":"admit","time":f.time}))?,
        f.time,
    )?;
    let mut outcome = noop(&prepared);
    assert!(f
        .db
        .transaction(|tx| {
            insert_input_mutation_receipt_tx(tx, &prepared, &outcome, f.time)?;
            Ok(())
        })
        .is_err());
    outcome.result = InputMutationResult::TaskResult {
        outcome: Box::new(TaskResultWriteOutcome::Record(Some(
            TaskResultSettlementRecord {
                result_identity: "foreign-result".into(),
                agent_id: "agent-b".into(),
                task_id: "task-b".into(),
                message_id: "message-b".into(),
                work_item_id: None,
                rejoin_generation: 1,
                parent_turn_id: "turn-b".into(),
                task_status: "completed".into(),
                state: TaskResultSettlementState::PersistedPending,
                activation_id: None,
                disposition: None,
                created_at: f.time,
                updated_at: f.time,
                admitted_at: None,
                settled_at: None,
                deferred_reason: None,
                deferred_at: None,
                next_recheck_at: None,
                recovery_epoch: 0,
                recovery_attempts: 0,
                last_recovery_root_attempt_id: None,
                last_recovery_admitted_at: None,
                recovery_paused_at: None,
                recovery_diagnostic_at: None,
                source_projection: None,
                reply_projection: None,
            },
        ))),
    };
    assert!(f
        .db
        .transaction(|tx| {
            insert_input_mutation_receipt_tx(tx, &prepared, &outcome, f.time)?;
            Ok(())
        })
        .is_err());
    let queue = f.prepared()?;
    outcome = noop(&queue);
    outcome.result = InputMutationResult::TaskResult {
        outcome: Box::new(TaskResultWriteOutcome::Record(None)),
    };
    assert!(f
        .db
        .transaction(|tx| {
            insert_input_mutation_receipt_tx(tx, &queue, &outcome, f.time)?;
            Ok(())
        })
        .is_err());
    assert_eq!(f.count("input_mutation_receipts")?, 0);
    Ok(())
}
