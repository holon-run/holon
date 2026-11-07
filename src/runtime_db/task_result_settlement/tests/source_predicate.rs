use super::*;
use crate::domain::execution_protocol::TaskRejoinSource;

fn source_loan_capture(
    db: &RuntimeDb,
    scope: &InputEvidenceScope,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<Arc<CapturedInputEvidence>> {
    use crate::runtime_db::{
        evidence::{upsert_agent_identity_tx, upsert_agent_state_tx},
        input_evidence_snapshot::{register_committed_input_evidence_tx, CanonicalInputEvidence},
    };
    use crate::types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentState,
        AgentVisibility,
    };

    db.transaction(|tx| {
        let identity = AgentIdentityRecord::new(
            &scope.agent_id,
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        upsert_agent_identity_tx(tx, &identity)?;
        upsert_agent_state_tx(tx, &AgentState::new(&scope.agent_id))?;
        let mut facts = Vec::new();
        for (sql, message) in [
            ("SELECT payload_json FROM messages WHERE agent_id=?1", true),
            (
                "SELECT payload_json FROM turn_records WHERE agent_id=?1",
                false,
            ),
        ] {
            let mut statement = tx.prepare(sql)?;
            let mut rows = statement.query([&scope.agent_id])?;
            while let Some(row) = rows.next()? {
                let payload: String = row.get(0)?;
                facts.push(if message {
                    CanonicalInputEvidence::Message(serde_json::from_str(&payload)?)
                } else {
                    CanonicalInputEvidence::Turn(serde_json::from_str(&payload)?)
                });
            }
        }
        let delta = register_committed_input_evidence_tx(tx, scope, &facts)?;
        let guard = ledger.register_view(
            scope,
            crate::runtime::input_evidence_budget::InputMemoryCategory::Current,
            &delta.records,
        )?;
        Ok(Arc::new(CapturedInputEvidence {
            query: None,
            query_acquisition: None,
            selected_completion_brief_ids: Default::default(),
            completion_source_observations: Default::default(),
            recovery_source_absences: Default::default(),
            execution: Default::default(),
            scope: scope.clone(),
            budget: Some(ledger.clone()),
            budget_views: vec![guard],
            windows: Vec::new(),
            records: delta.records,
            unavailable: Default::default(),
            turn_transcript_keys: delta.turn_transcript_keys,
            message_turn_keys: Arc::new(delta.message_turn_keys),
        }))
    })
}

fn source_loan(
    capture: &Arc<CapturedInputEvidence>,
    family: InputEvidenceFamily,
    id: &str,
) -> Result<Option<TaskResultSourceLoan>> {
    // Canonical commit readbacks retain their real budget guard in the holder.
    Ok(capture
        .records
        .iter()
        .find(|fact| fact.locator.key.family == family && fact.locator.key.id == id)
        .map(|fact| TaskResultSourceLoan::new(fact.clone(), capture.clone())))
}

fn source_loan_budget() -> Result<(InputEvidenceBudgetLedger, InputEvidenceScope)> {
    Ok((
        InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?,
        InputEvidenceScope {
            agent_id: "agent-a".into(),
            identity_incarnation: 1,
        },
    ))
}

fn consumed_sources(db: &RuntimeDb, status: QueueEntryStatus) -> Vec<TaskResultSettlementRecord> {
    let boundary = reply_source(db);
    let records = (0..4)
        .map(|index| live_reply(db, index, "older-parent", None, boundary))
        .collect();
    let (turn, attempt) = completed_reply_source(db);
    save_recovery_attempt(db, &attempt);
    db.turn_records().upsert(&turn).unwrap();
    let mut response = db
        .agent_message_deliveries()
        .latest("live-response-delivery")
        .unwrap()
        .unwrap();
    response.state = crate::types::AgentMessageDeliveryState::Consumed;
    response.activation_id = Some(attempt.attempt_id);
    response.turn_id = Some(turn.turn_id);
    rewrite_delivery(db, &response);
    queue(db, "live-reply", status);
    records
}

#[test]
fn source_reader_matches_independent_entries_and_reuses_charged_attempts() -> Result<()> {
    use crate::{
        config::StateMemoryLimits,
        diagnostics::resource_io::ResourceReadMaterial,
        runtime::{
            input_evidence_budget::InputEvidenceBudgetLedger,
            resident_input_evidence::InputEvidenceScope,
        },
    };

    for status in [QueueEntryStatus::Processed, QueueEntryStatus::Interjected] {
        let (_dir, db) = runtime_db();
        let records = consumed_sources(&db, status);
        let command = live_result(&db, 10, TaskKind::CommandTask, "command-parent", None);
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let scope = InputEvidenceScope {
            agent_id: "agent-a".into(),
            identity_incarnation: 1,
        };
        let measure = |shared: bool| -> Result<_> {
            let material = ResourceReadMaterial::new(&ledger, &scope)?;
            db.transaction(|tx| {
                material.read(|| {
                    let read = TaskResultSourceRead::new(tx, "agent-a");
                    for record in &records {
                        let predicates = if shared {
                            [
                                read.source_matches(record, true)?,
                                read.reply_source_recoverable(record)?,
                                read.reply_source_consumed(record)?,
                            ]
                        } else {
                            [
                                reply_source_is_consumed_for_owner_tx(tx, record)?,
                                reply_source_is_recoverable_tx(tx, record)?,
                                reply_source_is_consumed_tx(tx, record)?,
                            ]
                        };
                        assert_eq!(predicates, [true; 3]);
                    }
                    assert!(read.source_matches(&command, false)?);
                    assert!(non_reply_source_matches_owner_tx(tx, &command)?);
                    Ok(())
                })
            })?;
            material.finish()
        };
        let independent = measure(false)?;
        let shared = measure(true)?;
        assert!(
            shared.0 < independent.0 && shared.1 < independent.1,
            "shared={shared:?}, independent={independent:?}"
        );
    }
    Ok(())
}

#[test]
fn source_reader_rejects_other_cuts_and_agents() -> Result<()> {
    let (_dir, db) = runtime_db();
    let (_other_dir, other_db) = runtime_db();
    let records = consumed_sources(&db, QueueEntryStatus::Processed);
    db.transaction(|tx| {
        let read = TaskResultSourceRead::new(tx, "agent-a");
        read.validate_cut(tx, "agent-a")?;
        assert!(read.reply_source_recoverable(&records[0])?);
        assert!(read.validate_cut(tx, "agent-b").is_err());
        other_db.transaction(|other_tx| {
            assert!(read.validate_cut(other_tx, "agent-a").is_err());
            Ok(())
        })?;
        let mut foreign = records[0].clone();
        foreign.agent_id = "agent-b".into();
        assert!(read.source_matches(&foreign, true).is_err());
        assert!(read.source_matches(&foreign, false).is_err());
        assert!(read.reply_source_recoverable(&foreign).is_err());
        assert!(read.reply_source_consumed(&foreign).is_err());
        Ok(())
    })
}

#[test]
fn source_reader_shares_attempt_across_exact_wait_and_recovery_scope() -> Result<()> {
    use crate::{
        config::StateMemoryLimits,
        diagnostics::resource_io::ResourceReadMaterial,
        runtime::{
            input_evidence_budget::InputEvidenceBudgetLedger,
            resident_input_evidence::InputEvidenceScope,
        },
    };

    let (_dir, db) = runtime_db();
    let (_other_dir, other_db) = runtime_db();
    let records = consumed_sources(&db, QueueEntryStatus::Processed);
    let activation = db
        .agent_message_deliveries()
        .latest("live-response-delivery")?
        .unwrap()
        .activation_id
        .unwrap();
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let scope = InputEvidenceScope {
        agent_id: "agent-a".into(),
        identity_incarnation: 1,
    };
    let material = ResourceReadMaterial::new(&ledger, &scope)?;
    db.transaction(|tx| {
        material.read(|| {
            let read = TaskResultSourceRead::new(tx, "agent-a");
            assert!(read.reply_source_consumed(&records[0])?);
            let original = read.attempt(&activation)?.unwrap();
            let baseline = material.finish()?;
            // A closed original Attempt cannot grant an exact waiter; the
            // recovery selector still uses that same admitted immutable fact.
            assert!(read.exact_waiter_candidate(None, &activation)?.is_none());
            let snapshot = RecoverySnapshotRead::from_source_read(&read, &activation)?;
            assert!(matches!(
                &snapshot.scope,
                RecoverySnapshotScope::NormalInput
            ));
            assert!(read
                .exact_waiter_candidate(Some("foreign"), &activation)?
                .is_none());
            assert!(Rc::ptr_eq(&original, &read.attempt(&activation)?.unwrap()));
            assert_eq!(material.finish()?, baseline);
            snapshot.validate_cut(tx, "agent-a")?;
            assert!(snapshot.validate_cut(tx, "agent-b").is_err());
            other_db.transaction(|other_tx| {
                assert!(snapshot.validate_cut(other_tx, "agent-a").is_err());
                Ok(())
            })
        })
    })
}

#[test]
fn source_reader_never_memoizes_waiter_or_source_eligibility() -> Result<()> {
    for case in [
        "task-owner",
        "task-generation",
        "task-parent",
        "task-reply",
        "result-agent",
        "result-owner",
        "result-task",
        "reply-route",
        "delivery-route",
        "delivery-state",
        "queue-state",
        "turn-owner",
        "turn-trigger",
        "task-status",
    ] {
        let (_dir, db) = runtime_db();
        let records = consumed_sources(&db, QueueEntryStatus::Interjected);
        db.transaction(|tx| {
            let read = TaskResultSourceRead::new(tx, "agent-a");
            assert!(read.source_matches(&records[0], true)?, "{case}");
            assert!(read.reply_source_recoverable(&records[0])?, "{case}");
            // Warm this waiter too: neither per-source nor per-waiter
            // eligibility may survive a changed SQL witness.
            let record = &records[1];
            assert!(read.source_matches(record, true)?, "{case}");
            assert!(read.reply_source_recoverable(record)?, "{case}");
            match case {
                "task-owner" | "task-generation" | "task-parent" | "task-reply" => {
                    let (path, value) = match case {
                        "task-owner" => ("$.work_item_id", serde_json::json!("foreign-owner")),
                        "task-generation" => ("$.detail.rejoin_generation", serde_json::json!(2)),
                        "task-parent" => ("$.detail.parent_turn_id", serde_json::json!("foreign")),
                        _ => ("$.detail.message_id", serde_json::json!("foreign-reply")),
                    };
                    tx.execute(
                        "UPDATE tasks SET payload_json = json_set(payload_json, ?2, json(?3))
                         WHERE task_id = ?1",
                        params![record.task_id, path, value.to_string()],
                    )?;
                }
                "result-agent" | "result-owner" | "result-task" => {
                    let path = match case {
                        "result-agent" => "$.agent_id",
                        "result-owner" => "$.work_item_id",
                        _ => "$.task_id",
                    };
                    tx.execute(
                        "UPDATE messages SET payload_json = json_set(payload_json, ?2, 'foreign')
                         WHERE message_id = ?1",
                        params![record.message_id, path],
                    )?;
                }
                "reply-route" => {
                    tx.execute(
                        "UPDATE messages SET payload_json = json_set(payload_json,
                           '$.routing_context.recipient_agent_id', 'foreign')
                         WHERE message_id = 'live-reply'",
                        [],
                    )?;
                }
                "delivery-route" | "delivery-state" => {
                    let (path, value) = if case == "delivery-route" {
                        ("$.routing_context.recipient_agent_id", "foreign")
                    } else {
                        ("$.state", "queued")
                    };
                    tx.execute(
                        "UPDATE agent_message_deliveries
                         SET payload_json = json_set(payload_json, ?1, ?2)
                         WHERE delivery_id = 'live-response-delivery'",
                        params![path, value],
                    )?;
                }
                "queue-state" => {
                    tx.execute(
                        "UPDATE queue_entries SET status = 'queued'
                         WHERE message_id = 'live-reply'",
                        [],
                    )?;
                }
                "turn-owner" | "turn-trigger" => {
                    let path = if case == "turn-owner" {
                        "$.current_work_item_id"
                    } else {
                        "$.trigger.message_id"
                    };
                    tx.execute(
                        "UPDATE turn_records SET payload_json = json_set(payload_json, ?1, 'foreign')
                         WHERE turn_id = 'completed-reply-source'",
                        [path],
                    )?;
                }
                "task-status" => {
                    tx.execute(
                        "UPDATE tasks SET payload_json = json_set(payload_json, '$.status', 'running')
                         WHERE task_id = ?1",
                        [&record.task_id],
                    )?;
                }
                _ => unreachable!(),
            }
            assert!(!read.source_matches(record, true)?, "{case}");
            assert!(!read.reply_source_recoverable(record)?, "{case}");
            assert!(!reply_source_is_consumed_for_owner_tx(tx, record)?, "{case}");
            assert!(!reply_source_is_recoverable_tx(tx, record)?, "{case}");
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn source_reader_rechecks_the_closed_admitted_result_row() -> Result<()> {
    for case in ["valid", "state", "identity", "activation"] {
        let (_dir, db) = runtime_db();
        let boundary = reply_source(&db);
        let pending = live_reply(&db, 0, "live-parent", None, boundary);
        prepare_recovery(&db, &[pending.clone()], true);
        let record = db
            .task_result_settlements()
            .latest_for_message(&pending.message_id)?
            .unwrap();
        assert_eq!(record.state, TaskResultSettlementState::CallerAdmitted);
        db.transaction(|tx| {
            let read = TaskResultSourceRead::new(tx, "agent-a");
            assert!(read.reply_source_recoverable(&record)?);
            assert!(reply_source_is_recoverable_tx(tx, &record)?);
            if case != "valid" {
                let (path, value) = match case {
                    "state" => ("$.state", "settled"),
                    "identity" => ("$.result_identity", "foreign-result"),
                    _ => ("$.activation_id", "foreign-activation"),
                };
                tx.execute(
                    "UPDATE task_result_settlements
                     SET payload_json = json_set(payload_json, ?2, ?3)
                     WHERE message_id = ?1",
                    params![record.message_id, path, value],
                )?;
            }
            assert_eq!(read.reply_source_recoverable(&record)?, case == "valid");
            assert_eq!(
                reply_source_is_recoverable_tx(tx, &record)?,
                case == "valid"
            );
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn independent_source_entries_never_reuse_a_readers_attempt() -> Result<()> {
    let (_dir, db) = runtime_db();
    let records = consumed_sources(&db, QueueEntryStatus::Interjected);
    db.transaction(|tx| {
        let read = TaskResultSourceRead::new(tx, "agent-a");
        assert!(read.reply_source_recoverable(&records[0])?);
        drop(read);
        tx.execute(
            "UPDATE execution_protocol_attempts
             SET payload_json = json_set(payload_json,
                   '$.source_message_id', 'foreign-source',
                   '$.source.identity.message_id', 'foreign-source')
             WHERE agent_id = 'agent-a' AND attempt_id = 'consumed-activation'",
            [],
        )?;
        assert!(!reply_source_is_recoverable_tx(tx, &records[0])?);
        assert!(!reply_source_is_consumed_for_owner_tx(tx, &records[0])?);
        assert!(!reply_source_is_consumed_tx(tx, &records[0])?);
        Ok(())
    })
}

#[test]
fn source_loans_recheck_actual_payload_missing_rows_and_ambiguous_selectors() -> Result<()> {
    for status in [QueueEntryStatus::Processed, QueueEntryStatus::Interjected] {
        for case in [
            "valid",
            "reply-payload",
            "result-payload",
            "turn-payload",
            "missing-reply",
            "missing-turn",
            "ambiguous-message",
            "ambiguous-turn",
        ] {
            let (_dir, db) = runtime_db();
            let records = consumed_sources(&db, status.clone());
            let (ledger, scope) = source_loan_budget()?;
            let capture = source_loan_capture(&db, &scope, &ledger)?;
            let mut duplicate = capture
                .records
                .iter()
                .find_map(|fact| match &fact.record {
                    PreparedInputEvidence::Turn(turn)
                        if turn.turn_id == "completed-reply-source" =>
                    {
                        Some(turn.clone())
                    }
                    _ => None,
                })
                .unwrap();
            duplicate.turn_id = "duplicate-completed-source".into();
            let material = ResourceReadMaterial::new(&ledger, &scope)?;
            db.transaction(|tx| material.read(|| {
            let resolve = |family, id: &str| source_loan(&capture, family, id);
            let scope_read = crate::runtime_db::context_inputs::ContextDurableRead::new(
                tx, &scope.agent_id,
            ).scope_read()?;
            let resolver = TaskResultSourceResolver::with_scope_read(
                tx, &scope, &ledger, &scope_read, &resolve,
            )?;
            let original = TaskResultSourceRead::new(tx, &scope.agent_id);
            let read = original.with_resolver(&resolver)?;
            for record in &records {
                assert!(read.source_matches(record, true)?, "{case}");
                assert!(read.reply_source_recoverable(record)?, "{case}");
                assert!(read.reply_source_consumed(record)?, "{case}");
            }
            match case {
                "reply-payload" => {
                    // Deliberately leave content_hash and Input aliases unchanged.
                    assert_eq!(tx.execute(
                        "UPDATE messages SET payload_json=json_set(payload_json,
                           '$.body.text','changed body') WHERE message_id='live-reply'", [],
                    )?, 1);
                }
                "result-payload" => {
                    assert_eq!(tx.execute(
                        "UPDATE messages SET payload_json=json_set(payload_json,
                           '$.body.text','changed result') WHERE message_id=?1",
                        [&records[0].message_id],
                    )?, 1);
                }
                "turn-payload" => {
                    // Even an ignored JSON field changes the actual payload digest.
                    assert_eq!(tx.execute(
                        "UPDATE turn_records SET payload_json=json_set(payload_json,
                           '$.source_loan_probe',1) WHERE turn_id='completed-reply-source'", [],
                    )?, 1);
                }
                "missing-reply" => {
                    assert_eq!(tx.execute("DELETE FROM messages WHERE message_id='live-reply'", [])?, 1);
                    assert!(read.message("live-reply")?.is_none());
                }
                "missing-turn" => {
                    assert_eq!(tx.execute("DELETE FROM turn_records WHERE turn_id='completed-reply-source'", [])?, 1);
                    assert!(read.turn("completed-reply-source")?.is_none());
                }
                "ambiguous-message" => {
                    assert_eq!(tx.execute(
                        "INSERT INTO messages(evidence_id,agent_id,message_id,created_at,kind,payload_json)
                         SELECT 'duplicate-reply',agent_id,message_id,created_at,kind,payload_json
                         FROM messages WHERE evidence_id='live-reply'", [],
                    )?, 1);
                    assert!(read.message("live-reply")?.is_none());
                }
                "ambiguous-turn" => {
                    crate::runtime_db::repositories::upsert_turn_record_tx(tx, &duplicate)?;
                }
                _ => {}
            }
            let result = read.source_matches(&records[0], true);
            match case {
                "valid" => {
                    assert!(result?);
                    assert!(Rc::ptr_eq(
                        &original.attempt("consumed-activation")?.unwrap(),
                        &read.attempt("consumed-activation")?.unwrap(),
                    ));
                    let message = read.message("live-reply")?.unwrap();
                    let SourceBody::Loan(loan) = message else { panic!("body was copied") };
                    let original = capture.lookup_borrowed(InputEvidenceFamily::Message, "live-reply")?.unwrap();
                    assert!(Arc::ptr_eq(&loan.fact, original));
                    assert!(Arc::ptr_eq(&loan.capture, &capture));
                }
                "reply-payload" | "result-payload" | "turn-payload" => {
                    assert!(result.is_err(), "{case}");
                }
                _ => assert!(!result?, "{case}"),
            }
            Ok(())
        }))?;
        }
    }
    Ok(())
}

#[test]
fn source_loans_charge_each_exact_delivery_payload_not_the_latest_reply_delivery() -> Result<()> {
    let (_dir, db) = runtime_db();
    let records = consumed_sources(&db, QueueEntryStatus::Processed);
    let (ledger, scope) = source_loan_budget()?;
    let capture = source_loan_capture(&db, &scope, &ledger)?;
    let measure = |material: &ResourceReadMaterial| -> Result<_> {
        db.transaction(|tx| {
            material.read(|| {
                let resolve = |family, id: &str| source_loan(&capture, family, id);
                let resolver = TaskResultSourceResolver::new(tx, &scope, &ledger, &resolve)?;
                // Independent roots really decode twice; shared wrappers do not.
                for _ in 0..2 {
                    let original = TaskResultSourceRead::new(tx, &scope.agent_id);
                    let read = original.with_resolver(&resolver)?;
                    assert!(read.reply_source_consumed(&records[0])?);
                }
                Ok(())
            })
        })?;
        material.finish()
    };
    let baseline = measure(&ResourceReadMaterial::new(&ledger, &scope)?)?;
    db.transaction(|tx| {
        // Canonical deliveries have a UNIQUE message_id: a later reply is a
        // different message, not another delivery of this waiter's reply.
        let decoy = delivery(
            "later-response-delivery",
            "peer-b",
            "agent-a",
            "later-reply",
        );
        crate::runtime_db::agent_message_delivery::insert_delivery_tx(tx, &decoy)?;
        assert_eq!(
            tx.execute(
                "UPDATE agent_message_deliveries SET payload_json=payload_json || ?1
             WHERE delivery_id='later-response-delivery'",
                [" ".repeat(193)],
            )?,
            1
        );
        Ok(())
    })?;
    assert_eq!(
        measure(&ResourceReadMaterial::new(&ledger, &scope)?)?,
        baseline,
        "a later delivery of another reply is not this waiter's witness"
    );
    let material = ResourceReadMaterial::new(&ledger, &scope)?;
    db.transaction(|tx| {
        material.read(|| {
            let resolve = |family, id: &str| source_loan(&capture, family, id);
            let resolver = TaskResultSourceResolver::new(tx, &scope, &ledger, &resolve)?;
            let original = TaskResultSourceRead::new(tx, &scope.agent_id);
            let read = original.with_resolver(&resolver)?;
            assert!(read.reply_source_consumed(&records[0])?);
            let payload: String = tx.query_row(
                "SELECT payload_json FROM tasks WHERE task_id=?1",
                [&records[0].task_id],
                |row| row.get(0),
            )?;
            assert_eq!(
                tx.execute(
                    "UPDATE tasks SET payload_json=json_set(payload_json,
                 '$.detail.response_delivery_id','later-response-delivery') WHERE task_id=?1",
                    [&records[0].task_id],
                )?,
                1
            );
            assert!(!read.reply_source_consumed(&records[0])?);
            assert_eq!(
                tx.execute(
                    "UPDATE tasks SET payload_json=?1 WHERE task_id=?2",
                    params![payload, records[0].task_id],
                )?,
                1
            );
            assert!(read.reply_source_consumed(&records[0])?);
            Ok(())
        })
    })?;
    material.finish()?;
    // Whitespace changes the actual payload, but not a reserialized record.
    let padding = "\n ".repeat(17);
    db.transaction(|tx| {
        assert_eq!(
            tx.execute(
                "UPDATE agent_message_deliveries SET payload_json=payload_json || ?1
             WHERE delivery_id='live-response-delivery'",
                [&padding],
            )?,
            1
        );
        Ok(())
    })?;
    let padded = measure(&ResourceReadMaterial::new(&ledger, &scope)?)?;
    assert_eq!(
        (padded.0 - baseline.0, padded.1 - baseline.1),
        (padding.len() as u64 * 16 * 8 * 2, 0),
        "both fresh decodes retain the original payload and outer charge factors"
    );
    assert_eq!(
        measure(&ResourceReadMaterial::with_test_limits(
            &ledger, &scope, padded.0, padded.1,
        )?)?,
        padded,
    );
    let short = ResourceReadMaterial::with_test_limits(&ledger, &scope, padded.0 - 1, padded.1)?;
    let error = measure(&short).unwrap_err();
    assert!(error
        .downcast_ref::<crate::runtime_error::RuntimeError>()
        .is_some());
    let descriptor = crate::runtime_error::describe_runtime_error(&error);
    assert_eq!(descriptor.code, "context_resource_exhausted");
    assert!(descriptor.retryable);
    assert!(short.finish().is_err());
    assert!(measure(&short).is_err(), "exhaustion remains latched");
    Ok(())
}

#[test]
fn source_loans_charge_records_for_each_fresh_task_and_exact_delivery_decode() -> Result<()> {
    let (_dir, db) = runtime_db();
    let records = consumed_sources(&db, QueueEntryStatus::Processed);
    let (ledger, scope) = source_loan_budget()?;
    let capture = source_loan_capture(&db, &scope, &ledger)?;
    let (expected_records, task_payload) = db.transaction(|tx| {
        let task: String = tx.query_row(
            "SELECT payload_json FROM tasks WHERE task_id=?1",
            [&records[0].task_id],
            |row| row.get(0),
        )?;
        let delivery: String = tx.query_row(
            "SELECT payload_json FROM agent_message_deliveries
             WHERE delivery_id='live-response-delivery'",
            [],
            |row| row.get(0),
        )?;
        let task_records = resource_io::json_payload_material(&task)?.1;
        let delivery_records = resource_io::json_payload_material(&delivery)?.1;
        // One Task is returned by the matcher, not decoded again for its detail.
        // The phase table, Task key/Rc and exact Delivery pair/Rc cost 4+5+6.
        Ok(((task_records + delivery_records + 4 + 5 + 6) * 8, task))
    })?;
    let measure = |material: &ResourceReadMaterial| -> Result<_> {
        db.transaction(|tx| {
            material.read(|| {
                let resolve = |family, id: &str| source_loan(&capture, family, id);
                let resolver = TaskResultSourceResolver::new(tx, &scope, &ledger, &resolve)?;
                for _ in 0..3 {
                    let original = TaskResultSourceRead::new(tx, &scope.agent_id);
                    let read = original.with_resolver(&resolver)?;
                    let before = material.finish()?;
                    let message = read.message(&records[0].message_id)?.unwrap();
                    let reply_id = crate::wake_contract::agent_message_reply_reference(&message)
                        .context("reply reference missing")?;
                    assert!(read.message(reply_id)?.is_some());
                    assert_eq!(
                        material.finish()?,
                        before,
                        "borrowed Message bodies and SQL rows are not fresh decodes"
                    );
                    assert!(read.reply_source_consumed(&records[0])?);
                    let after = material.finish()?;
                    assert_eq!(
                        after.1 - before.1,
                        expected_records,
                        "every fresh Task and exact Delivery decode admits its structure"
                    );
                }
                Ok(())
            })
        })?;
        material.finish()
    };
    let measured = measure(&ResourceReadMaterial::new(&ledger, &scope)?)?;
    let padding = "\n ".repeat(19);
    db.transaction(|tx| {
        assert_eq!(
            tx.execute(
                "UPDATE tasks SET payload_json=payload_json || ?1 WHERE task_id=?2",
                params![padding, records[0].task_id],
            )?,
            1
        );
        Ok(())
    })?;
    let padded = measure(&ResourceReadMaterial::new(&ledger, &scope)?)?;
    assert_eq!(
        (padded.0 - measured.0, padded.1 - measured.1),
        (padding.len() as u64 * 16 * 8 * 3, 0),
        "all three truly fresh Task decodes retain the actual UTF8 byte envelope"
    );
    db.transaction(|tx| {
        assert_eq!(
            tx.execute(
                "UPDATE tasks SET payload_json=?1 WHERE task_id=?2",
                params![task_payload, records[0].task_id],
            )?,
            1
        );
        Ok(())
    })?;
    assert_eq!(
        measure(&ResourceReadMaterial::with_test_limits(
            &ledger, &scope, measured.0, measured.1,
        )?)?,
        measured,
        "the exact record budget remains sufficient"
    );
    let short =
        ResourceReadMaterial::with_test_limits(&ledger, &scope, measured.0, measured.1 - 1)?;
    let error = measure(&short).unwrap_err();
    assert!(error
        .downcast_ref::<crate::runtime_error::RuntimeError>()
        .is_some());
    let descriptor = crate::runtime_error::describe_runtime_error(&error);
    assert_eq!(descriptor.code, "context_resource_exhausted");
    assert!(descriptor.retryable);
    assert!(short.finish().is_err());
    assert!(
        measure(&short).is_err(),
        "record exhaustion remains latched"
    );
    Ok(())
}

#[test]
fn source_metadata_loans_share_one_admission_across_resolvers_and_wrappers() -> Result<()> {
    let (_dir, db) = runtime_db();
    let records = consumed_sources(&db, QueueEntryStatus::Interjected);
    let (ledger, scope) = source_loan_budget()?;
    let capture = source_loan_capture(&db, &scope, &ledger)?;
    let material = ResourceReadMaterial::new(&ledger, &scope)?;
    db.transaction(|tx| {
        let checked = std::cell::Cell::new(false);
        let error = material
            .read(|| {
                let resolve = |family, id: &str| source_loan(&capture, family, id);
                let scope_read =
                    crate::runtime_db::context_inputs::ContextDurableRead::new(tx, &scope.agent_id)
                        .scope_read()?;
                let original = TaskResultSourceRead::new(tx, &scope.agent_id);
                let (task, delivery) = {
                    let resolver = TaskResultSourceResolver::with_scope_read(
                        tx,
                        &scope,
                        &ledger,
                        &scope_read,
                        &resolve,
                    )?;
                    let read = original.with_resolver(&resolver)?;
                    assert!(read.source_matches(&records[0], true)?);
                    assert!(read.reply_source_recoverable(&records[0])?);
                    assert!(read.reply_source_consumed(&records[0])?);
                    (
                        read.task(&records[0].task_id)?.unwrap(),
                        read.delivery("live-response-delivery", "live-reply")?
                            .unwrap(),
                    )
                };
                let before = material.finish()?;
                let resolver = TaskResultSourceResolver::with_scope_read(
                    tx,
                    &scope,
                    &ledger,
                    &scope_read,
                    &resolve,
                )?;
                let read = original.with_resolver(&resolver)?;
                let nested = read.with_resolver(&resolver)?;
                for current in [&read, &nested] {
                    assert!(current.source_matches(&records[0], true)?);
                    assert!(current.reply_source_recoverable(&records[0])?);
                    assert!(current.reply_source_consumed(&records[0])?);
                    assert!(std::ptr::eq(
                        &*task,
                        &*current.task(&records[0].task_id)?.unwrap()
                    ));
                    assert!(Rc::ptr_eq(
                        &delivery,
                        &current
                            .delivery("live-response-delivery", "live-reply")?
                            .unwrap()
                    ));
                    assert!(current
                        .delivery("live-response-delivery", "foreign-reply")?
                        .is_none());
                    assert!(current
                        .delivery("foreign-delivery", "live-reply")?
                        .is_none());
                    assert_eq!(
                        material.finish()?,
                        before,
                        "SQL rechecks and Rc loans do not decode or allocate another entry"
                    );
                }
                let phase = original.metadata.borrow();
                assert_eq!(phase.as_ref().unwrap().tasks.len(), 1);
                assert_eq!(phase.as_ref().unwrap().deliveries.len(), 1);
                // A successful cached lookup still cannot hide a later failed allowance.
                assert!(resource_io::charge_owned_metadata(u64::MAX, usize::MAX).is_err());
                for error in [
                    nested.task(&records[0].task_id).err().unwrap(),
                    nested
                        .delivery("live-response-delivery", "live-reply")
                        .err()
                        .unwrap(),
                ] {
                    assert_eq!(
                        crate::runtime_error::describe_runtime_error(&error).code,
                        "context_resource_exhausted"
                    );
                }
                checked.set(true);
                Ok(())
            })
            .expect_err("the latched phase cannot complete successfully");
        assert!(
            checked.get(),
            "all sharing and latch assertions must execute"
        );
        assert_eq!(
            crate::runtime_error::describe_runtime_error(&error).code,
            "context_resource_exhausted"
        );
        Ok(())
    })
}

#[test]
fn source_metadata_loans_never_cross_actual_allowances_or_writer_roots() -> Result<()> {
    let (_dir, db) = runtime_db();
    let records = consumed_sources(&db, QueueEntryStatus::Processed);
    let (ledger, scope) = source_loan_budget()?;
    let capture = source_loan_capture(&db, &scope, &ledger)?;
    let first = ResourceReadMaterial::new(&ledger, &scope)?;
    let second = ResourceReadMaterial::new(&ledger, &scope)?;
    assert!(!first.shares_allowance(&second));
    db.transaction(|tx| {
        first.read(|| {
            let resolve = |family, id: &str| source_loan(&capture, family, id);
            let scope_read =
                crate::runtime_db::context_inputs::ContextDurableRead::new(tx, &scope.agent_id)
                    .scope_read()?;
            let original = TaskResultSourceRead::new(tx, &scope.agent_id);
            let resolver = TaskResultSourceResolver::with_scope_read(
                tx,
                &scope,
                &ledger,
                &scope_read,
                &resolve,
            )?;
            let read = original.with_resolver(&resolver)?;
            assert!(read.reply_source_consumed(&records[0])?);
            let task = read.task(&records[0].task_id)?.unwrap();
            let delivery = read
                .delivery("live-response-delivery", "live-reply")?
                .unwrap();
            let before = second.finish()?;
            second.read(|| {
                assert!(read.task(&records[0].task_id).is_err());
                assert!(read
                    .delivery("live-response-delivery", "live-reply")
                    .is_err());
                let resolver = TaskResultSourceResolver::with_scope_read(
                    tx,
                    &scope,
                    &ledger,
                    &scope_read,
                    &resolve,
                )?;
                let current = original.with_resolver(&resolver)?;
                assert!(current.reply_source_consumed(&records[0])?);
                assert!(!std::ptr::eq(
                    &*task,
                    &*current.task(&records[0].task_id)?.unwrap()
                ));
                assert!(!Rc::ptr_eq(
                    &delivery,
                    &current
                        .delivery("live-response-delivery", "live-reply")?
                        .unwrap()
                ));
                Ok(())
            })?;
            let after = second.finish()?;
            assert!(after.0 > before.0 && after.1 > before.1);
            let writer = TaskResultSourceRead::new(tx, &scope.agent_id);
            assert!(!std::ptr::eq(
                &*task,
                &*writer.task(&records[0].task_id)?.unwrap()
            ));
            assert!(!Rc::ptr_eq(
                &delivery,
                &writer
                    .delivery("live-response-delivery", "live-reply")?
                    .unwrap()
            ));
            assert!(writer.metadata.borrow().is_none());
            assert!(reply_source_is_consumed_tx(tx, &records[0])?);
            Ok(())
        })
    })
}

#[test]
fn source_metadata_loans_reject_task_and_exact_delivery_drift() -> Result<()> {
    for case in [
        "task-payload",
        "task-id",
        "task-agent",
        "task-owner",
        "task-source",
        "task-generation",
        "task-status",
        "task-exact-delivery",
        "task-sql-owner",
        "task-sql-status",
        "task-sql-revision",
        "missing-task",
        "delivery-payload",
        "delivery-id",
        "delivery-message",
        "delivery-state",
        "delivery-route",
        "delivery-sql-id",
        "delivery-sql-message",
        "delivery-sql-state",
        "delivery-sql-version",
        "delivery-sql-target",
        "missing-delivery",
        "queue-state",
    ] {
        let (_dir, db) = runtime_db();
        let records = consumed_sources(&db, QueueEntryStatus::Interjected);
        let (ledger, scope) = source_loan_budget()?;
        let capture = source_loan_capture(&db, &scope, &ledger)?;
        let material = ResourceReadMaterial::new(&ledger, &scope)?;
        db.transaction(|tx| {
            material.read(|| {
                let resolve = |family, id: &str| source_loan(&capture, family, id);
                let resolver = TaskResultSourceResolver::new(tx, &scope, &ledger, &resolve)?;
                let original = TaskResultSourceRead::new(tx, &scope.agent_id);
                let read = original.with_resolver(&resolver)?;
                assert!(read.source_matches(&records[0], true)?, "{case}");
                assert!(read.reply_source_recoverable(&records[0])?, "{case}");
                assert!(read.reply_source_consumed(&records[0])?, "{case}");
                let changed = match case {
                    "task-payload" => tx.execute(
                        "UPDATE tasks SET payload_json=payload_json || ' '
                         WHERE task_id=?1",
                        [&records[0].task_id],
                    )?,
                    "task-id"
                    | "task-agent"
                    | "task-owner"
                    | "task-source"
                    | "task-generation"
                    | "task-status"
                    | "task-exact-delivery" => {
                        let (path, value) = match case {
                            "task-id" => ("$.id", serde_json::json!("foreign-task")),
                            "task-agent" => ("$.agent_id", serde_json::json!("foreign-agent")),
                            "task-owner" => ("$.work_item_id", serde_json::json!("foreign-owner")),
                            "task-source" => {
                                ("$.detail.parent_turn_id", serde_json::json!("foreign"))
                            }
                            "task-generation" => {
                                ("$.detail.rejoin_generation", serde_json::json!(2))
                            }
                            "task-status" => ("$.status", serde_json::json!("running")),
                            _ => (
                                "$.detail.response_delivery_id",
                                serde_json::json!("foreign"),
                            ),
                        };
                        tx.execute(
                            "UPDATE tasks SET payload_json=json_set(payload_json,?1,json(?2))
                             WHERE task_id=?3",
                            params![path, value.to_string(), records[0].task_id],
                        )?
                    }
                    "task-sql-owner" => tx.execute(
                        "UPDATE tasks SET owner_agent_id='foreign' WHERE task_id=?1",
                        [&records[0].task_id],
                    )?,
                    "task-sql-status" => tx.execute(
                        "UPDATE tasks SET status='running' WHERE task_id=?1",
                        [&records[0].task_id],
                    )?,
                    "task-sql-revision" => tx.execute(
                        "UPDATE tasks SET revision=revision+1 WHERE task_id=?1",
                        [&records[0].task_id],
                    )?,
                    "missing-task" => {
                        tx.execute("DELETE FROM tasks WHERE task_id=?1", [&records[0].task_id])?
                    }
                    "delivery-payload" => tx.execute(
                        "UPDATE agent_message_deliveries SET payload_json=payload_json || ' '
                         WHERE delivery_id='live-response-delivery'",
                        [],
                    )?,
                    "delivery-id" | "delivery-message" | "delivery-state" | "delivery-route" => {
                        let (path, value) = match case {
                            "delivery-id" => ("$.delivery_id", "foreign-delivery"),
                            "delivery-message" => ("$.message_id", "foreign-reply"),
                            "delivery-state" => ("$.state", "queued"),
                            _ => ("$.routing_context.recipient_agent_id", "foreign"),
                        };
                        tx.execute(
                            "UPDATE agent_message_deliveries
                             SET payload_json=json_set(payload_json,?1,?2)
                             WHERE delivery_id='live-response-delivery'",
                            params![path, value],
                        )?
                    }
                    "delivery-sql-id" => tx.execute(
                        "UPDATE agent_message_deliveries SET delivery_id='foreign'
                         WHERE delivery_id='live-response-delivery'",
                        [],
                    )?,
                    "delivery-sql-message" => tx.execute(
                        "UPDATE agent_message_deliveries SET message_id='foreign'
                         WHERE delivery_id='live-response-delivery'",
                        [],
                    )?,
                    "delivery-sql-state" => tx.execute(
                        "UPDATE agent_message_deliveries SET state='queued'
                         WHERE delivery_id='live-response-delivery'",
                        [],
                    )?,
                    "delivery-sql-version" => tx.execute(
                        "UPDATE agent_message_deliveries SET state_version=state_version+1
                         WHERE delivery_id='live-response-delivery'",
                        [],
                    )?,
                    "delivery-sql-target" => tx.execute(
                        "UPDATE agent_message_deliveries SET target_agent_id='foreign'
                         WHERE delivery_id='live-response-delivery'",
                        [],
                    )?,
                    "missing-delivery" => tx.execute(
                        "DELETE FROM agent_message_deliveries
                         WHERE delivery_id='live-response-delivery'",
                        [],
                    )?,
                    "queue-state" => tx.execute(
                        "UPDATE queue_entries SET status='queued' WHERE message_id='live-reply'",
                        [],
                    )?,
                    _ => unreachable!(),
                };
                assert_eq!(changed, 1, "{case}");
                // A new wrapper on the same root cannot turn stale typed facts into proof.
                let next = original.with_resolver(&resolver)?;
                assert!(!next.source_matches(&records[0], true)?, "{case}");
                assert!(!next.reply_source_recoverable(&records[0])?, "{case}");
                assert!(!next.reply_source_consumed(&records[0])?, "{case}");
                if matches!(case, "task-payload" | "delivery-payload") {
                    assert!(
                        reply_source_is_consumed_for_owner_tx(tx, &records[0])?,
                        "an independent writer must decode the current valid payload: {case}"
                    );
                }
                Ok(())
            })
        })?;
    }
    Ok(())
}

#[test]
fn source_loans_preserve_delivery_decode_errors_without_latching_budget_failure() -> Result<()> {
    let (_dir, db) = runtime_db();
    let records = consumed_sources(&db, QueueEntryStatus::Processed);
    let (ledger, scope) = source_loan_budget()?;
    let capture = source_loan_capture(&db, &scope, &ledger)?;
    let original_payload: String = db.transaction(|tx| {
        tx.query_row(
            "SELECT payload_json FROM agent_message_deliveries
             WHERE delivery_id='live-response-delivery'",
            [],
            |row| row.get(0),
        )
        .map_err(Into::into)
    })?;
    let material = ResourceReadMaterial::new(&ledger, &scope)?;
    let read_source = || {
        db.transaction(|tx| {
            material.read(|| {
                let resolve = |family, id: &str| source_loan(&capture, family, id);
                let resolver = TaskResultSourceResolver::new(tx, &scope, &ledger, &resolve)?;
                let original = TaskResultSourceRead::new(tx, &scope.agent_id);
                original
                    .with_resolver(&resolver)?
                    .reply_source_consumed(&records[0])
            })
        })
    };
    db.transaction(|tx| {
        assert_eq!(
            tx.execute(
                "UPDATE agent_message_deliveries SET payload_json='{}'
                 WHERE delivery_id='live-response-delivery'",
                [],
            )?,
            1
        );
        Ok(())
    })?;
    let error = read_source().unwrap_err();
    assert!(error.downcast_ref::<serde_json::Error>().is_some());
    assert!(error
        .downcast_ref::<crate::runtime_error::RuntimeError>()
        .is_none());
    material.finish()?;
    db.transaction(|tx| {
        assert_eq!(
            tx.execute(
                "UPDATE agent_message_deliveries SET payload_json=?1
                 WHERE delivery_id='live-response-delivery'",
                [&original_payload],
            )?,
            1
        );
        Ok(())
    })?;
    assert!(read_source()?);
    material.finish()?;
    Ok(())
}

#[test]
fn source_loans_keep_closed_admitted_result_and_recovery_checks_fresh() -> Result<()> {
    for change in [
        "none",
        "result",
        "result-payload",
        "source-payload",
        "turn-payload",
    ] {
        let (_dir, db) = runtime_db();
        let boundary = reply_source(&db);
        let pending = live_reply(&db, 0, "live-parent", None, boundary);
        prepare_recovery(&db, &[pending.clone()], true);
        // prepare_recovery attaches a real turn, but only queues its anchor.
        let mut source = MessageEnvelope::new(
            "agent-a",
            MessageKind::InternalFollowup,
            MessageOrigin::System {
                subsystem: "task_result_recovery".into(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Next,
            MessageBody::Text {
                text: "recover interrupted results".into(),
            },
        )
        .with_admission(
            MessageDeliverySurface::RuntimeSystem,
            AdmissionContext::RuntimeOwned,
        );
        source.id = "live-anchor".into();
        db.messages().upsert(&source)?;
        let record = db
            .task_result_settlements()
            .latest_for_message(&pending.message_id)?
            .unwrap();
        let (ledger, scope) = source_loan_budget()?;
        let capture = source_loan_capture(&db, &scope, &ledger)?;
        let material = ResourceReadMaterial::new(&ledger, &scope)?;
        db.transaction(|tx| {
            material.read(|| {
                let resolve = |family, id: &str| source_loan(&capture, family, id);
                let resolver = TaskResultSourceResolver::new(tx, &scope, &ledger, &resolve)?;
                let original = TaskResultSourceRead::new(tx, &scope.agent_id);
                let read = original.with_resolver(&resolver)?;
                assert!(read.reply_source_recoverable(&record)?);
                let activation = record.activation_id.as_deref().unwrap();
                let attempt = read.attempt(activation)?.unwrap();
                assert_eq!(
                    attempt.source_message_id.as_deref(),
                    Some(source.id.as_str())
                );
                assert!(read.turn("live-parent")?.is_some());
                let snapshot = RecoverySnapshotRead::from_source_read(&read, "recovery-a")?;
                assert!(matches!(
                    &snapshot.scope,
                    RecoverySnapshotScope::RecoveryRoot(root) if root == activation
                ));
                let snapshot = RecoverySnapshotRead::from_source_read_with_resolver(
                    &original,
                    &resolver,
                    "recovery-a",
                )?;
                assert!(matches!(
                    &snapshot.scope,
                    RecoverySnapshotScope::RecoveryRoot(root) if root == activation
                ));
                snapshot.validate_cut(tx, &scope.agent_id)?;
                match change {
                    "result" => {
                        assert_eq!(tx.execute(
                            "UPDATE task_result_settlements SET payload_json=json_set(payload_json,
                           '$.state','settled') WHERE message_id=?1",
                            [&record.message_id],
                        )?, 1);
                    }
                    "result-payload" => {
                        assert_eq!(
                            tx.execute(
                                "UPDATE messages SET payload_json=json_set(payload_json,
                           '$.body.text','changed admitted result') WHERE message_id=?1",
                                [&record.message_id],
                            )?,
                            1
                        );
                    }
                    "source-payload" => {
                        assert_eq!(
                            tx.execute(
                                "UPDATE messages SET payload_json=json_set(payload_json,
                           '$.body.text','changed recovery') WHERE message_id=?1",
                                [&source.id],
                            )?,
                            1
                        );
                        assert!(
                            RecoverySnapshotRead::from_source_read(&read, "recovery-a").is_err()
                        );
                        assert!(RecoverySnapshotRead::from_source_read_with_resolver(
                            &original,
                            &resolver,
                            "recovery-a",
                        )
                        .is_err());
                    }
                    "turn-payload" => {
                        // attempt.turn_id is an ingress id, not its attached turn.
                        assert_eq!(
                            tx.execute(
                                "UPDATE turn_records SET payload_json=json_set(payload_json,
                           '$.source_loan_probe',1) WHERE agent_id=?1 AND turn_id='live-parent'",
                                [&scope.agent_id],
                            )?,
                            1
                        );
                    }
                    _ => {}
                }
                let result = read.reply_source_recoverable(&record);
                match change {
                    // The recovery anchor is not the admitted Result body.
                    "none" | "source-payload" => assert!(result?),
                    "result" => assert!(!result?),
                    _ => assert!(result.is_err(), "{change}"),
                }
                Ok(())
            })
        })?;
    }
    Ok(())
}

#[test]
fn source_loans_reject_foreign_cut_agent_scope_capture_and_phase() -> Result<()> {
    let (_dir, db) = runtime_db();
    let (_other_dir, other_db) = runtime_db();
    let records = consumed_sources(&db, QueueEntryStatus::Interjected);
    let (ledger, scope) = source_loan_budget()?;
    let capture = source_loan_capture(&db, &scope, &ledger)?;
    let material = ResourceReadMaterial::new(&ledger, &scope)?;
    db.transaction(|tx| {
        material.read(|| {
            let resolve = |family, id: &str| source_loan(&capture, family, id);
            let resolver = TaskResultSourceResolver::new(tx, &scope, &ledger, &resolve)?;
            let original = TaskResultSourceRead::new(tx, &scope.agent_id);
            let read = original.with_resolver(&resolver)?;
            assert!(read.source_matches(&records[0], true)?);
            other_db.transaction(|other_tx| {
                let foreign = TaskResultSourceRead::new(other_tx, &scope.agent_id);
                assert!(foreign.with_resolver(&resolver).is_err());
                Ok(())
            })?;
            for (agent_id, incarnation) in [("agent-b", 1), ("agent-a", 2)] {
                let foreign_scope = InputEvidenceScope {
                    agent_id: agent_id.into(),
                    identity_incarnation: incarnation,
                };
                let foreign = TaskResultSourceResolver::new(tx, &foreign_scope, &ledger, &resolve)?;
                assert!(original.with_resolver(&foreign).is_err());
            }
            for corrupt_scope in [false, true] {
                let corrupt = |family, id: &str| {
                    let Some(mut loan) = source_loan(&capture, family, id)? else {
                        return Ok(None);
                    };
                    // Equal bytes in another allocation are not capture membership.
                    let mut fact = (*loan.fact).clone();
                    if corrupt_scope {
                        fact.locator.scope.identity_incarnation += 1;
                    }
                    loan.fact = Arc::new(fact);
                    Ok(Some(loan))
                };
                let corrupt = TaskResultSourceResolver::new(tx, &scope, &ledger, &corrupt)?;
                assert!(original
                    .with_resolver(&corrupt)?
                    .source_matches(&records[0], true)
                    .is_err());
            }
            let other_phase = ResourceReadMaterial::new(&ledger, &scope)?;
            other_phase.read(|| {
                assert!(read.source_matches(&records[0], true).is_err());
                assert!(read
                    .exact_waiter_candidate(None, "consumed-activation")
                    .is_err());
                Ok(())
            })?;
            assert!(read.source_matches(&records[0], true)?);
            tx.execute(
                "UPDATE agent_identities SET payload_json=json_set(payload_json,
               '$.incarnation',2) WHERE agent_id='agent-a'",
                [],
            )?;
            assert!(read.source_matches(&records[0], true).is_err());
            Ok(())
        })
    })
}

fn with_non_reply_material(
    db: &RuntimeDb,
    mut action: impl FnMut(&TaskResultSourceRead<'_, '_>, &ResourceReadMaterial) -> Result<()>,
) -> Result<()> {
    let (ledger, scope) = source_loan_budget()?;
    let capture = source_loan_capture(db, &scope, &ledger)?;
    let material = ResourceReadMaterial::new(&ledger, &scope)?;
    db.transaction(|tx| {
        material.read(|| {
            let resolve = |family, id: &str| source_loan(&capture, family, id);
            let resolver = TaskResultSourceResolver::new(tx, &scope, &ledger, &resolve)?;
            let original = TaskResultSourceRead::new(tx, &scope.agent_id);
            let read = original.with_resolver(&resolver)?;
            action(&read, &material)
        })
    })
}

#[test]
fn non_reply_projection_handles_64_independent_tasks_without_full_materialization() -> Result<()> {
    let (_dir, db) = runtime_db();
    let records: Vec<_> = (0..64)
        .map(|index| {
            live_result(
                &db,
                index,
                if index % 2 == 0 {
                    TaskKind::CommandTask
                } else {
                    TaskKind::ChildAgentTask
                },
                "independent-parent",
                None,
            )
        })
        .collect();
    let full_records = db.transaction(|tx| {
        let mut count = 0;
        for record in &records {
            let payload: String = tx.query_row(
                "SELECT payload_json FROM tasks WHERE task_id=?1",
                [&record.task_id],
                |row| row.get(0),
            )?;
            count += resource_io::json_payload_material(&payload)?.1 * 8;
        }
        Ok(count)
    })?;
    assert!(full_records > 65_536);
    with_non_reply_material(&db, |read, material| {
        for record in &records {
            assert!(read.source_matches(record, false)?);
        }
        let metadata = read.non_reply_metadata()?;
        let first =
            Rc::clone(&metadata.borrow().as_ref().unwrap().tasks[&records[0].task_id].record);
        let wrapper = read.with_resolver(read.resolver.unwrap())?;
        for record in &records {
            assert!(wrapper.source_matches(record, false)?);
        }
        assert!(Rc::ptr_eq(
            &first,
            &metadata.borrow().as_ref().unwrap().tasks[&records[0].task_id].record,
        ));
        assert_eq!(metadata.borrow().as_ref().unwrap().tasks.len(), 64);
        assert!(read.shared_metadata.unwrap().borrow().is_none());
        let used = material.finish()?;
        assert!(used.0 > 0 && used.1 > 0 && used.1 < 65_536);
        Ok(())
    })
}

#[test]
fn non_reply_projection_preserves_full_task_and_fence_decode_semantics() -> Result<()> {
    for case in [
        "canonical",
        "session-alias",
        "escaped-identity",
        "escaped-field",
        "nested-detail",
        "duplicate-fence",
        "duplicate-unknown-detail",
        "missing-optionals",
        "failed",
        "cancelled",
        "interrupted",
        "child",
        "recovery",
        "generation-u64-max",
        "missing-id",
        "duplicate-id",
        "alias-conflict",
        "invalid-date",
        "summary-type",
        "recovery-type",
        "detail-number-range",
        "detail-array",
        "obligation-type",
        "obligation-missing",
        "obligation-empty",
        "obligation-foreign",
        "generation-missing",
        "generation-zero",
        "generation-string",
        "generation-float",
        "generation-negative",
        "generation-overflow",
        "parent-null",
        "parent-empty",
        "parent-type",
        "source-missing",
        "source-conflict",
        "source-null-conflict",
        "kind-type",
        "kind-unknown",
        "kind-wait",
        "status-running",
        "status-mismatch",
        "parent-message-null",
        "work-foreign",
        "agent-type",
        "foreign-agent",
    ] {
        let (_dir, db) = runtime_db();
        let mut record = live_result(&db, 0, TaskKind::CommandTask, "parent", None);
        let payload: String = db.transaction(|tx| {
            Ok(tx.query_row(
                "SELECT payload_json FROM tasks WHERE task_id=?1",
                [&record.task_id],
                |row| row.get(0),
            )?)
        })?;
        let mut value: serde_json::Value = serde_json::from_str(&payload)?;
        let mut raw = None;
        match case {
            "session-alias" => {
                let object = value.as_object_mut().unwrap();
                let agent = object.remove("agent_id").unwrap();
                object.insert("session_id".into(), agent);
            }
            "escaped-identity" => {
                raw = Some(payload.replace("\"live-task-0\"", "\"live-task-\\u0030\""));
            }
            "escaped-field" => {
                raw = Some(
                    payload
                        .replace("\"agent_id\":", "\"\\u0061gent_id\":")
                        .replace("\"rejoin_generation\":", "\"rejoin_\\u0067eneration\":"),
                );
            }
            "nested-detail" => {
                value["detail"]["extra"] = serde_json::json!({
                    "text": "escaped\npayload", "nested": [null, true, -1, 1.5, {"x": "y"}],
                })
            }
            "duplicate-fence" => {
                raw = Some(payload.replace(
                    "\"rejoin_generation\":1",
                    "\"rejoin_generation\":0,\"rejoin_generation\":1",
                ))
            }
            "duplicate-unknown-detail" => {
                raw = Some(payload.replace(
                    "\"rejoin_generation\":1",
                    "\"extra\":0,\"extra\":1,\"rejoin_generation\":1",
                ))
            }
            "missing-optionals" => {
                for key in ["summary", "work_item_id", "recovery"] {
                    value.as_object_mut().unwrap().remove(key);
                }
            }
            "failed" | "cancelled" | "interrupted" => {
                value["status"] = serde_json::json!(case);
                record.task_status = case.into();
            }
            "child" => value["kind"] = serde_json::json!("child_agent_task"),
            "recovery" => {
                value["recovery"] = serde_json::json!({
                    "kind": "subagent_task", "summary": "summary", "prompt": "prompt",
                    "authority_class": "runtime_instruction",
                })
            }
            "missing-id" => {
                value.as_object_mut().unwrap().remove("id");
            }
            "duplicate-id" => {
                raw = Some(payload.replacen("\"id\":", "\"id\":\"foreign\",\"id\":", 1))
            }
            "alias-conflict" => value["session_id"] = serde_json::json!("agent-a"),
            "invalid-date" => value["created_at"] = serde_json::json!("not-a-date"),
            "summary-type" => value["summary"] = serde_json::json!(false),
            "recovery-type" => value["recovery"] = serde_json::json!({ "kind": "unknown" }),
            "detail-number-range" => {
                raw = Some(payload.replace(
                    "\"rejoin_generation\":1",
                    "\"ignored\":1e999,\"rejoin_generation\":1",
                ))
            }
            "detail-array" => value["detail"] = serde_json::json!([]),
            "obligation-type" => value["detail"]["rejoin_obligation_id"] = serde_json::json!(1),
            "obligation-missing" => {
                value["detail"]
                    .as_object_mut()
                    .unwrap()
                    .remove("rejoin_obligation_id");
            }
            "obligation-empty" => value["detail"]["rejoin_obligation_id"] = serde_json::json!(""),
            "obligation-foreign" => {
                value["detail"]["rejoin_obligation_id"] = serde_json::json!("foreign")
            }
            "generation-missing" => {
                value["detail"]
                    .as_object_mut()
                    .unwrap()
                    .remove("rejoin_generation");
            }
            "generation-zero" => value["detail"]["rejoin_generation"] = serde_json::json!(0),
            "generation-u64-max" => {
                value["detail"]["rejoin_generation"] = serde_json::json!(u64::MAX);
                record.rejoin_generation = u64::MAX;
            }
            "generation-string" => value["detail"]["rejoin_generation"] = serde_json::json!("1"),
            "generation-float" => value["detail"]["rejoin_generation"] = serde_json::json!(1.0),
            "generation-negative" => value["detail"]["rejoin_generation"] = serde_json::json!(-1),
            "generation-overflow" => {
                raw = Some(payload.replace(
                    "\"rejoin_generation\":1",
                    "\"rejoin_generation\":18446744073709551616",
                ))
            }
            "parent-null" => value["detail"]["parent_turn_id"] = serde_json::Value::Null,
            "parent-empty" => value["detail"]["parent_turn_id"] = serde_json::json!(""),
            "parent-type" => value["detail"]["parent_turn_id"] = serde_json::json!(true),
            "source-missing" => {
                value["detail"]
                    .as_object_mut()
                    .unwrap()
                    .remove("parent_turn_id");
            }
            "source-conflict" => {
                value["detail"]["command_creation_id"] = serde_json::json!(record.task_id)
            }
            "source-null-conflict" => {
                value["detail"]["command_creation_id"] = serde_json::Value::Null
            }
            "kind-type" => value["kind"] = serde_json::json!(1),
            "kind-unknown" => value["kind"] = serde_json::json!("unknown"),
            "kind-wait" => value["kind"] = serde_json::json!("agent_message_wait"),
            "status-running" => value["status"] = serde_json::json!("running"),
            "status-mismatch" => value["status"] = serde_json::json!("failed"),
            "parent-message-null" => value["parent_message_id"] = serde_json::Value::Null,
            "work-foreign" => value["work_item_id"] = serde_json::json!("foreign"),
            "agent-type" => value["agent_id"] = serde_json::json!(1),
            "foreign-agent" => value["agent_id"] = serde_json::json!("foreign"),
            _ => {}
        }
        let changed = raw.unwrap_or(serde_json::to_string(&value)?);
        db.transaction(|tx| {
            tx.execute(
                "UPDATE tasks SET payload_json=?1 WHERE task_id=?2",
                params![changed, record.task_id],
            )?;
            Ok(())
        })?;
        let expected = db.transaction(|tx| non_reply_source_matches_owner_tx(tx, &record));
        if matches!(
            case,
            "canonical"
                | "session-alias"
                | "escaped-identity"
                | "escaped-field"
                | "nested-detail"
                | "duplicate-fence"
                | "duplicate-unknown-detail"
                | "missing-optionals"
                | "failed"
                | "cancelled"
                | "interrupted"
                | "child"
                | "recovery"
                | "generation-u64-max"
        ) {
            assert!(matches!(expected, Ok(true)), "{case}");
        } else {
            assert!(!matches!(expected, Ok(true)), "{case}");
        }
        let mut observed = None;
        with_non_reply_material(&db, |read, _| {
            observed = Some(read.source_matches(&record, false));
            Ok(())
        })?;
        let observed = observed.unwrap();
        assert_eq!(observed.is_err(), expected.is_err(), "{case}");
        assert_eq!(observed.ok(), expected.ok(), "{case}");
    }
    Ok(())
}

#[test]
fn non_reply_projection_rechecks_complete_payload_and_sql_witnesses() -> Result<()> {
    for case in [
        "payload-whitespace",
        "ignored-detail",
        "sql-kind",
        "sql-status",
        "sql-revision",
        "sql-owner",
        "missing",
        "id",
        "agent",
        "kind",
        "status",
        "work",
        "parent-message",
        "obligation",
        "generation",
        "parent-turn",
        "source-conflict",
        "source-null",
        "record-generation",
        "record-source",
        "record-status",
        "record-message",
        "record-work",
    ] {
        let (_dir, db) = runtime_db();
        let record = live_result(&db, 0, TaskKind::CommandTask, "parent", None);
        with_non_reply_material(&db, |read, _| {
            assert!(read.source_matches(&record, false)?, "{case}");
            let mut changed_record = record.clone();
            match case {
                "record-generation" => changed_record.rejoin_generation += 1,
                "record-source" => {
                    changed_record.source = TaskRejoinSource::ParentTurn {
                        parent_turn_id: "foreign".into(),
                    }
                }
                "record-status" => changed_record.task_status = "failed".into(),
                "record-message" => changed_record.message_id = "foreign".into(),
                "record-work" => changed_record.work_item_id = Some("foreign".into()),
                "payload-whitespace" => {
                    read.tx
                        .execute("UPDATE tasks SET payload_json=payload_json || ' '", [])?;
                }
                "sql-kind" => {
                    read.tx
                        .execute("UPDATE tasks SET kind='child_agent_task'", [])?;
                }
                "sql-status" => {
                    read.tx.execute("UPDATE tasks SET status='running'", [])?;
                }
                "sql-revision" => {
                    read.tx
                        .execute("UPDATE tasks SET revision=revision+1", [])?;
                }
                "sql-owner" => {
                    read.tx
                        .execute("UPDATE tasks SET owner_agent_id='foreign'", [])?;
                }
                "missing" => {
                    read.tx.execute("DELETE FROM tasks", [])?;
                }
                _ => {
                    let (path, value) = match case {
                        "ignored-detail" => ("$.detail.ignored", serde_json::json!("changed")),
                        "id" => ("$.id", serde_json::json!("foreign")),
                        "agent" => ("$.agent_id", serde_json::json!("foreign")),
                        "kind" => ("$.kind", serde_json::json!("child_agent_task")),
                        "status" => ("$.status", serde_json::json!("failed")),
                        "work" => ("$.work_item_id", serde_json::json!("foreign")),
                        "parent-message" => ("$.parent_message_id", serde_json::json!("foreign")),
                        "obligation" => (
                            "$.detail.rejoin_obligation_id",
                            serde_json::json!("foreign"),
                        ),
                        "generation" => ("$.detail.rejoin_generation", serde_json::json!(2)),
                        "parent-turn" => ("$.detail.parent_turn_id", serde_json::json!("foreign")),
                        "source-conflict" => (
                            "$.detail.command_creation_id",
                            serde_json::json!(record.task_id),
                        ),
                        _ => ("$.detail.command_creation_id", serde_json::Value::Null),
                    };
                    assert_eq!(
                        read.tx.execute(
                            "UPDATE tasks SET payload_json=json_set(payload_json,?1,json(?2))",
                            params![path, value.to_string()],
                        )?,
                        1
                    );
                }
            }
            assert!(!read.source_matches(&changed_record, false)?, "{case}");
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn non_reply_projection_preserves_message_and_current_creation_gates() -> Result<()> {
    for case in [
        "kind",
        "agent",
        "work",
        "task",
        "origin",
        "authority",
        "surface",
        "admission",
        "reply-reference",
        "missing",
    ] {
        let (_dir, db) = runtime_db();
        let record = live_result(&db, 0, TaskKind::ChildAgentTask, "parent", None);
        with_non_reply_material(&db, |read, _| {
            assert!(read.source_matches(&record, false)?, "{case}");
            if case == "missing" {
                read.tx.execute("DELETE FROM messages", [])?;
            } else {
                let (path, value) = match case {
                    "kind" => ("$.kind", serde_json::json!("operator_prompt")),
                    "agent" => ("$.agent_id", serde_json::json!("foreign")),
                    "work" => ("$.work_item_id", serde_json::json!("foreign")),
                    "task" => ("$.task_id", serde_json::json!("foreign")),
                    "origin" => ("$.origin.task_id", serde_json::json!("foreign")),
                    "authority" => ("$.authority_class", serde_json::json!("external_evidence")),
                    "surface" => ("$.delivery_surface", serde_json::json!("runtime_system")),
                    "admission" => ("$.admission_context", serde_json::json!("local_process")),
                    _ => (
                        "$.metadata",
                        serde_json::json!({
                            "task_id": record.task_id, "task_kind": "agent_message_wait",
                            "task_status": "completed",
                            "task_detail": { "reply_content_source": "original_message", "message_id": "foreign" },
                        }),
                    ),
                };
                read.tx.execute(
                    "UPDATE messages SET payload_json=json_set(payload_json,?1,json(?2))",
                    params![path, value.to_string()],
                )?;
            }
            assert!(
                !read.source_matches(&record, false).unwrap_or(false),
                "{case}"
            );
            Ok(())
        })?;
    }
    for case in [
        "current-source",
        "child-creation",
        "wrong-creation",
        "both-sources",
    ] {
        let (_dir, db) = runtime_db();
        let record = command_creation_result(&db, 0);
        with_non_reply_material(&db, |read, _| {
            assert!(read.source_matches(&record, false)?, "{case}");
            if case == "current-source" {
                read.tx.execute(
                    "UPDATE command_task_creation_sources
                     SET original_task_json=json_set(original_task_json,'$.detail.rejoin_generation',2)",
                    [],
                )?;
                assert!(!read.source_matches(&record, false)?);
                return Ok(());
            }
            let (path, value) = match case {
                "child-creation" => ("$.kind", serde_json::json!("child_agent_task")),
                "wrong-creation" => ("$.detail.command_creation_id", serde_json::json!("foreign")),
                _ => ("$.detail.parent_turn_id", serde_json::json!("parent")),
            };
            read.tx.execute(
                "UPDATE tasks SET payload_json=json_set(payload_json,?1,json(?2))",
                params![path, value.to_string()],
            )?;
            // A fresh reader must reject too, not merely the warmed raw witness.
            let fresh = TaskResultSourceRead::new(read.tx, read.agent_id);
            let fresh = fresh.with_resolver(read.resolver.unwrap())?;
            assert!(!fresh.source_matches(&record, false)?, "{case}");
            assert!(
                !non_reply_source_matches_owner_tx(read.tx, &record)?,
                "{case}"
            );
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn non_reply_projection_never_crosses_phases_or_lends_to_reply_and_writer() -> Result<()> {
    let (_dir, db) = runtime_db();
    let replies = consumed_sources(&db, QueueEntryStatus::Processed);
    let record = live_result(&db, 80, TaskKind::CommandTask, "parent", None);
    let (ledger, scope) = source_loan_budget()?;
    let capture = source_loan_capture(&db, &scope, &ledger)?;
    let first = ResourceReadMaterial::new(&ledger, &scope)?;
    let second = ResourceReadMaterial::new(&ledger, &scope)?;
    db.transaction(|tx| {
        let original = TaskResultSourceRead::new(tx, &scope.agent_id);
        first.read(|| {
            let resolve = |family, id: &str| source_loan(&capture, family, id);
            let resolver = TaskResultSourceResolver::new(tx, &scope, &ledger, &resolve)?;
            let read = original.with_resolver(&resolver)?;
            assert!(read.source_matches(&record, false)?);
            let identity = Rc::clone(
                &original.non_reply_metadata.borrow().as_ref().unwrap().tasks[&record.task_id]
                    .record,
            );
            assert!(original.metadata.borrow().is_none());
            assert!(read.source_matches(&replies[0], true)?);
            assert!(original
                .metadata
                .borrow()
                .as_ref()
                .unwrap()
                .tasks
                .contains_key(&replies[0].task_id));
            assert!(!original
                .non_reply_metadata
                .borrow()
                .as_ref()
                .unwrap()
                .tasks
                .contains_key(&replies[0].task_id));
            // A projected identity cannot supply task(), even for this exact id.
            let full = read.task(&record.task_id)?.unwrap();
            let writer = TaskResultSourceRead::new(tx, &scope.agent_id);
            assert!(writer.source_matches(&record, false)?);
            let writer_first = writer.task(&record.task_id)?.unwrap();
            let writer_second = writer.task(&record.task_id)?.unwrap();
            assert!(!std::ptr::eq(&*full, &*writer_first));
            assert!(!std::ptr::eq(&*writer_first, &*writer_second));
            assert!(writer.metadata.borrow().is_none());
            assert!(writer.non_reply_metadata.borrow().is_none());
            second.read(|| {
                assert!(read.source_matches(&record, false).is_err());
                let resolver = TaskResultSourceResolver::new(tx, &scope, &ledger, &resolve)?;
                let current = original.with_resolver(&resolver)?;
                assert!(current.source_matches(&record, false)?);
                let current_metadata = original.non_reply_metadata.borrow();
                let current_identity =
                    &current_metadata.as_ref().unwrap().tasks[&record.task_id].record;
                assert!(!Rc::ptr_eq(&identity, current_identity));
                Ok(())
            })?;
            Ok(())
        })
    })
}

#[test]
fn non_reply_projection_admits_exact_material_and_keeps_failure_latched() -> Result<()> {
    let (_dir, db) = runtime_db();
    let record = live_result(&db, 0, TaskKind::CommandTask, "parent", None);
    let (ledger, scope) = source_loan_budget()?;
    let capture = source_loan_capture(&db, &scope, &ledger)?;
    let measure = |material: &ResourceReadMaterial| -> Result<_> {
        db.transaction(|tx| {
            material.read(|| {
                let resolve = |family, id: &str| source_loan(&capture, family, id);
                let resolver = TaskResultSourceResolver::new(tx, &scope, &ledger, &resolve)?;
                let original = TaskResultSourceRead::new(tx, &scope.agent_id);
                let read = original.with_resolver(&resolver)?;
                assert!(read.source_matches(&record, false)?);
                assert!(read.source_matches(&record, false)?);
                assert!(original.metadata.borrow().is_none());
                Ok(())
            })
        })?;
        material.finish()
    };
    let measured = measure(&ResourceReadMaterial::new(&ledger, &scope)?)?;
    assert_eq!(
        measure(&ResourceReadMaterial::with_test_limits(
            &ledger, &scope, measured.0, measured.1
        )?)?,
        measured,
    );
    for short in [
        ResourceReadMaterial::with_test_limits(&ledger, &scope, measured.0 - 1, measured.1)?,
        ResourceReadMaterial::with_test_limits(&ledger, &scope, measured.0, measured.1 - 1)?,
    ] {
        let error = measure(&short).unwrap_err();
        let descriptor = crate::runtime_error::describe_runtime_error(&error);
        assert_eq!(descriptor.code, "context_resource_exhausted");
        assert!(descriptor.retryable);
        assert!(short.finish().is_err());
        assert!(measure(&short).is_err());
    }
    for warm in [false, true] {
        let error = with_non_reply_material(&db, |read, material| {
            if warm {
                assert!(read.source_matches(&record, false)?);
            }
            assert!(resource_io::charge_owned_metadata(u64::MAX, usize::MAX).is_err());
            assert!(material.finish().is_err());
            assert!(read.source_matches(&record, false).is_err());
            Ok(())
        })
        .unwrap_err();
        assert_eq!(
            crate::runtime_error::describe_runtime_error(&error).code,
            "context_resource_exhausted",
        );
    }
    Ok(())
}

#[test]
fn source_loans_cannot_hide_latched_resource_failure_or_lend_to_writer() -> Result<()> {
    for failure in ["before", "some", "none"] {
        let (_dir, db) = runtime_db();
        let records = consumed_sources(&db, QueueEntryStatus::Interjected);
        let (ledger, scope) = source_loan_budget()?;
        let capture = source_loan_capture(&db, &scope, &ledger)?;
        let material = ResourceReadMaterial::new(&ledger, &scope)?;
        let failed = db.transaction(|tx| {
            material.read(|| -> Result<()> {
                let resolve = |family, id: &str| {
                    if failure != "before" {
                        assert!(resource_io::charge_owned_metadata(u64::MAX, usize::MAX).is_err());
                    }
                    if failure == "none" {
                        return Ok(None);
                    }
                    source_loan(&capture, family, id)
                };
                let resolver = TaskResultSourceResolver::new(tx, &scope, &ledger, &resolve)?;
                let original = TaskResultSourceRead::new(tx, &scope.agent_id);
                let read = original.with_resolver(&resolver)?;
                if failure == "before" {
                    assert!(read.source_matches(&records[0], true)?);
                    // Writer qualification is independent even after a loan warmed.
                    tx.execute(
                        "UPDATE turn_records SET payload_json=json_set(payload_json,
                       '$.trigger.message_id','foreign-source')",
                        [],
                    )?;
                    assert!(!reply_source_is_consumed_for_owner_tx(tx, &records[0])?);
                    assert!(resource_io::charge_owned_metadata(u64::MAX, usize::MAX).is_err());
                }
                Err(read.source_matches(&records[0], true).unwrap_err())
            })
        });
        let error = failed.unwrap_err();
        let descriptor = crate::runtime_error::describe_runtime_error(&error);
        assert_eq!(descriptor.code, "context_resource_exhausted");
        assert!(descriptor.retryable);
    }
    Ok(())
}
