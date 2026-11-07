use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger, resident_work::WorkCaptureRequest,
    },
    runtime_db::work_snapshot::WorkDomainLoadRequest,
    types::{
        AuthorityClass, MessageBody, MessageKind, MessageOrigin, Priority, WaitConditionKind,
        WakeSource,
    },
};

const BAD: &str = r#"{"not_a_wait":"{,:} \"x\" \\ 路"}"#;

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "wait-pool-agent".into(),
        identity_incarnation: 1,
    }
}
fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}
fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}
fn wire_cost(payload: &str) -> Result<(u64, usize)> {
    // Independent AST formula; no production scanner, gate or sampled usage.
    fn slots(value: &serde_json::Value) -> usize {
        match value {
            serde_json::Value::Object(fields) => {
                2 + fields.len().saturating_sub(1)
                    + fields.values().map(|value| 2 + slots(value)).sum::<usize>()
            }
            serde_json::Value::Array(values) => {
                2 + values.len().saturating_sub(1) + values.iter().map(slots).sum::<usize>()
            }
            serde_json::Value::String(_) => 1,
            _ => 0,
        }
    }
    let slots = slots(&serde_json::from_str(payload)?);
    Ok((
        (payload.len() as u64 * 4 + slots as u64 * 128 + 512) * 8,
        (slots * 4 + 16) * 8,
    ))
}
fn cell_cost() -> (u64, usize) {
    (std::mem::size_of::<WaitConditionRecord>() as u64 * 8, 8)
}
fn at() -> chrono::DateTime<Utc> {
    "2026-10-17T11:22:23Z".parse().unwrap()
}
fn record(id: &str) -> WaitConditionRecord {
    WaitConditionRecord {
        id: id.into(),
        agent_id: scope().agent_id,
        work_item_id: Some(id.into()),
        status: WaitConditionStatus::Active,
        kind: WaitConditionKind::Task,
        source: Some("{,:} \"x\" \\ 路".into()),
        subject_ref: Some("not-this-timer".into()),
        waiting_for: "wait pool".into(),
        wake_sources: vec![WakeSource::TaskResult {
            task_id: "not-this-timer".into(),
        }],
        continuation: None,
        created_at: at(),
        updated_at: at(),
        expires_at: None,
        resolved_at: None,
        cancelled_at: None,
        turn_id: None,
        trigger_message_id: None,
        triggered_at: None,
    }
}
fn insert(tx: &Transaction<'_>, record: &WaitConditionRecord) -> Result<()> {
    // Keep pool fixtures independent of the reducer's separate write boundary.
    tx.execute(
        "INSERT INTO wait_conditions
         (wait_condition_id,agent_id,work_item_id,status,kind,waiting_for,
          created_at,updated_at,payload_json)
         VALUES (?1,?2,?3,?4,'task',?5,?6,?6,?7)",
        params![
            record.id,
            record.agent_id,
            record.work_item_id,
            match record.status {
                WaitConditionStatus::Active => "active",
                WaitConditionStatus::Triggered => "triggered",
                _ => "cancelled",
            },
            record.waiting_for,
            record.updated_at.to_rfc3339(),
            serde_json::to_string(record)?,
        ],
    )?;
    Ok(())
}
fn fixture() -> Result<(tempfile::TempDir, RuntimeDb)> {
    let home = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    db.transaction(|tx| {
        insert(tx, &record("wait-a"))?;
        insert(tx, &record("wait-b"))
    })?;
    Ok((home, db))
}
fn ledger() -> Result<InputEvidenceBudgetLedger> {
    Ok(InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?)
}
fn message_and_queue() -> (MessageEnvelope, QueueEntryRecord) {
    let mut message = MessageEnvelope::new(
        &scope().agent_id,
        MessageKind::TimerTick,
        MessageOrigin::Timer {
            timer_id: "pool-timer".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "timer wake".into(),
        },
    );
    message.id = "pool-message".into();
    message.created_at = at();
    let queue = QueueEntryRecord {
        message_id: message.id.clone(),
        agent_id: message.agent_id.clone(),
        priority: message.priority.clone(),
        status: QueueEntryStatus::Queued,
        created_at: at(),
        updated_at: at(),
    };
    (message, queue)
}
fn request() -> WorkDomainLoadRequest {
    WorkDomainLoadRequest {
        capture: WorkCaptureRequest::current(),
        record_limit: 4096,
        byte_limit: 16 * 1024 * 1024,
    }
}
fn assert_exhausted(error: &anyhow::Error) {
    assert!(
        format!("{error:#}")
            .to_lowercase()
            .contains("resourceexhausted"),
        "{error:#}"
    );
}

#[test]
fn selector_refusal_propagates_through_reducer_and_prediction_without_writes() -> Result<()> {
    let (_home, db) = fixture()?;
    let (mut message, queue) = message_and_queue();
    message.kind = MessageKind::TaskResult;
    message.origin = MessageOrigin::Task {
        task_id: "not-this-timer".into(),
    };
    // No task_id permits no deterministic task-owner preference: both match.
    let prefix = ["wait-a", "wait-b"]
        .into_iter()
        .try_fold(header(), |sum, id| {
            Ok::<_, anyhow::Error>(add(
                sum,
                add(
                    wire_cost(&serde_json::to_string(&record(id))?)?,
                    cell_cost(),
                ),
            ))
        })?;
    for prediction in [false, true] {
        // The predictor owns a projection guard before entering the same reducer.
        let prefix = if prediction {
            add(prefix, header())
        } else {
            prefix
        };
        for bytes_bound in [true, false] {
            let budget = ledger()?;
            let phase = ResourceReadMaterial::with_test_limits(
                &budget,
                &scope(),
                if bytes_bound {
                    prefix.0
                } else {
                    64 * 1024 * 1024
                },
                if bytes_bound { 65536 } else { prefix.1 },
            )?;
            {
                let _active = phase.enter();
                let error = db
                    .transaction(|tx| {
                        if prediction {
                            predicted_trigger_wait_tx(
                                tx,
                                &scope(),
                                &message,
                                &queue,
                                &request(),
                                &budget,
                                None,
                            )?;
                        } else {
                            trigger_wait_and_enqueue_tx(tx, &message, &queue)?;
                        }
                        Ok(())
                    })
                    .unwrap_err();
                assert_exhausted(&error);
            }
            assert!(phase.finish().is_err());
            assert_eq!(budget.report()?.observed_bytes, prefix.0);
            drop(phase);
            assert_eq!(budget.report()?.observed_bytes, 0);
            db.transaction(|tx| {
                for table in ["messages", "queue_entries", "audit_events"] {
                    assert_eq!(
                        tx.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                            .get::<_, usize>(0))?,
                        0
                    );
                }
                assert_eq!(
                    trigger_wait_conditions_tx(tx, &scope().agent_id, None)?.len(),
                    2
                );
                Ok(())
            })?;
        }
    }
    Ok(())
}

#[test]
fn both_pool_reads_share_exact_original_allowance_and_last_holder() -> Result<()> {
    let (_home, db) = fixture()?;
    let one = add(
        wire_cost(&serde_json::to_string(&record("wait-a"))?)?,
        cell_cost(),
    );
    let body = (one.0 * 4, one.1 * 4);
    let exact = add(add(header(), header()), body);
    let budget = ledger()?;
    let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
    let transaction = phase.transaction()?;
    let (first, second) = {
        let _active = transaction.enter();
        db.transaction(|tx| {
            Ok((
                trigger_wait_conditions_tx(tx, &scope().agent_id, Some(2))?,
                trigger_wait_conditions_tx(tx, &scope().agent_id, None)?,
            ))
        })?
    };
    assert_eq!(first, vec![record("wait-b"), record("wait-a")]);
    assert_eq!(second, first);
    assert_eq!(phase.finish()?, exact);
    assert_eq!(transaction.finish()?, add(header(), body));
    let first_holder = transaction.guard()?;
    let last_holder = first_holder.clone();
    assert!(std::sync::Arc::ptr_eq(&first_holder, &last_holder));
    drop((phase, transaction, first, second));
    assert_eq!(budget.report()?.observed_bytes, add(header(), body).0);
    drop(first_holder);
    assert_eq!(budget.report()?.observed_bytes, add(header(), body).0);
    drop(last_holder);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn each_row_decode_and_cell_gate_precedes_malformed_body_and_latches_failure() -> Result<()> {
    let good = add(
        wire_cost(&serde_json::to_string(&record("wait-b"))?)?,
        cell_cost(),
    );
    for row_index in 0..2 {
        let (_home, db) = fixture()?;
        db.transaction(|tx| {
            tx.execute(
                "UPDATE wait_conditions SET payload_json=?1 WHERE wait_condition_id=?2",
                params![BAD, if row_index == 0 { "wait-b" } else { "wait-a" }],
            )?;
            Ok(())
        })?;
        let mut prefix = if row_index == 0 {
            header()
        } else {
            add(header(), good)
        };
        for next in [wire_cost(BAD)?, cell_cost()] {
            for bytes_bound in [true, false] {
                let budget = ledger()?;
                let phase = ResourceReadMaterial::with_test_limits(
                    &budget,
                    &scope(),
                    if bytes_bound {
                        prefix.0 + next.0 - 1
                    } else {
                        64 * 1024 * 1024
                    },
                    if bytes_bound {
                        65536
                    } else {
                        prefix.1 + next.1 - 1
                    },
                )?;
                {
                    let _active = phase.enter();
                    assert_exhausted(
                        &db.transaction(|tx| {
                            trigger_wait_conditions_tx(tx, &scope().agent_id, None)
                        })
                        .unwrap_err(),
                    );
                    assert_eq!(budget.report()?.observed_bytes, prefix.0);
                    assert_exhausted(
                        &db.transaction(|tx| {
                            trigger_wait_conditions_tx(tx, &scope().agent_id, None)
                        })
                        .unwrap_err(),
                    );
                    assert_eq!(budget.report()?.observed_bytes, prefix.0);
                }
                assert!(phase.finish().is_err());
                assert!(ResourceReadMaterial::active_guard()?.is_none());
                drop(phase);
                assert_eq!(budget.report()?.observed_bytes, 0);
            }
            prefix = add(prefix, next);
        }
    }
    Ok(())
}

#[test]
fn real_prediction_and_reducer_reject_pool_before_decode_or_sql_writes() -> Result<()> {
    let (_home, db) = fixture()?;
    db.transaction(|tx| {
        tx.execute("UPDATE wait_conditions SET payload_json=?1", [BAD])?;
        Ok(())
    })?;
    let (message, queue) = message_and_queue();
    for prediction in [true, false] {
        let mut prefix = header();
        for next in [wire_cost(BAD)?, cell_cost()] {
            for bytes_bound in [true, false] {
                let budget = ledger()?;
                let phase = ResourceReadMaterial::with_test_limits(
                    &budget,
                    &scope(),
                    if bytes_bound {
                        prefix.0 + next.0 - 1
                    } else {
                        64 * 1024 * 1024
                    },
                    if bytes_bound {
                        65536
                    } else {
                        prefix.1 + next.1 - 1
                    },
                )?;
                {
                    let _active = phase.enter();
                    let result = db.transaction(|tx| {
                        if prediction {
                            predicted_trigger_wait_tx(
                                tx,
                                &scope(),
                                &message,
                                &queue,
                                &request(),
                                &budget,
                                None,
                            )?;
                        } else {
                            trigger_wait_and_enqueue_tx(tx, &message, &queue)?;
                        }
                        Ok(())
                    });
                    assert_exhausted(&result.unwrap_err());
                }
                assert!(phase.finish().is_err());
                drop(phase);
                assert_eq!(budget.report()?.observed_bytes, 0);
                db.transaction(|tx| {
                    for table in ["messages", "queue_entries", "audit_events"] {
                        assert_eq!(
                            tx.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                                row.get::<_, usize>(0)
                            })?,
                            0
                        );
                    }
                    Ok(())
                })?;
            }
            prefix = add(prefix, next);
        }
    }
    Ok(())
}

#[test]
fn malformed_decode_releases_original_material_without_hiding_sql_error() -> Result<()> {
    let (_home, db) = fixture()?;
    db.transaction(|tx| {
        tx.execute("UPDATE wait_conditions SET payload_json=?1", [BAD])?;
        Ok(())
    })?;
    let budget = ledger()?;
    let phase = ResourceReadMaterial::new(&budget, &scope())?;
    {
        let _active = phase.enter();
        let error = db
            .transaction(|tx| trigger_wait_conditions_tx(tx, &scope().agent_id, None))
            .unwrap_err();
        assert!(format!("{error:#}").contains("missing field"));
        assert!(error.downcast_ref::<rusqlite::Error>().is_some());
    }
    assert_eq!(
        phase.finish()?,
        add(header(), add(wire_cost(BAD)?, cell_cost()))
    );
    drop(phase);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn pool_preserves_filter_order_limits_and_plain_inspection() -> Result<()> {
    let (_home, db) = fixture()?;
    let mut triggered = record("wait-c");
    triggered.status = WaitConditionStatus::Triggered;
    let mut cancelled = record("wait-d");
    cancelled.status = WaitConditionStatus::Cancelled;
    let mut foreign = record("wait-e");
    foreign.agent_id = "foreign-agent".into();
    db.transaction(|tx| {
        insert(tx, &triggered)?;
        insert(tx, &cancelled)?;
        insert(tx, &foreign)?;
        assert!(trigger_wait_conditions_tx(tx, &scope().agent_id, Some(0))?.is_empty());
        assert_eq!(
            trigger_wait_conditions_tx(tx, &scope().agent_id, Some(1))?,
            vec![triggered.clone()]
        );
        assert_eq!(
            trigger_wait_conditions_tx(tx, &scope().agent_id, None)?,
            vec![triggered.clone(), record("wait-b"), record("wait-a")]
        );
        Ok(())
    })?;
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}
