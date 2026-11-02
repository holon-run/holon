use super::*;
#[path = "wait_prediction_budget_tests.rs"]
mod prediction_budget_tests;
#[path = "wait_writes_move_tests.rs"]
mod writes_move_tests;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    types::{AuthorityClass, MessageBody, MessageKind, MessageOrigin, Priority, WakeSource},
};

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "wait-body-agent".into(),
        identity_incarnation: 1,
    }
}
fn at() -> chrono::DateTime<Utc> {
    "2026-10-17T12:00:00Z".parse().unwrap()
}
fn record() -> WaitConditionRecord {
    WaitConditionRecord {
        id: "wait-body".into(),
        agent_id: scope().agent_id,
        work_item_id: None,
        status: WaitConditionStatus::Active,
        kind: crate::types::WaitConditionKind::Timer,
        source: Some("timer".into()),
        subject_ref: Some("body-timer".into()),
        waiting_for: "{,:} \"x\" \\ 路".into(),
        wake_sources: vec![WakeSource::Timer { wake_at: at() }],
        continuation: Some(serde_json::json!({"nested": [null, "{,:} \"x\" \\ 路"]})),
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
fn wake() -> crate::wake_contract::WakeMatch<'static> {
    crate::wake_contract::WakeMatch {
        source: "timer",
        subject_ref: Some("body-timer"),
    }
}
fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}
fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}
fn cost(value: &impl serde::Serialize, decode: bool) -> Result<(u64, usize)> {
    // Independent AST formula, not the production scanner or sampled usage.
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
    let payload = serde_json::to_string(value)?;
    let slots = slots(&serde_json::from_str(&payload)?);
    Ok((
        (payload.len() as u64 * 4 + slots as u64 * 128 + 512) * 8,
        (slots * if decode { 4 } else { 1 } + 16) * 8,
    ))
}
fn gates(record: &WaitConditionRecord, trigger: &str) -> Result<[(u64, usize); 4]> {
    Ok([
        cost(record, false)?,
        cost(&trigger, false)?,
        cost(&record.id, false)?,
        (
            std::mem::size_of::<(WaitConditionRecord, crate::wake_contract::WakeMatch<'_>)>()
                as u64
                * 8,
            8,
        ),
    ])
}
fn ledger() -> Result<InputEvidenceBudgetLedger> {
    Ok(InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?)
}
fn phase(
    budget: &InputEvidenceBudgetLedger,
    limit: (u64, usize),
    bytes_bound: bool,
) -> Result<ResourceReadMaterial> {
    ResourceReadMaterial::with_test_limits(
        budget,
        &scope(),
        if bytes_bound {
            limit.0 - 1
        } else {
            64 * 1024 * 1024
        },
        if bytes_bound { 65536 } else { limit.1 - 1 },
    )
}
fn exhausted(error: &anyhow::Error) {
    assert!(
        format!("{error:#}")
            .to_lowercase()
            .contains("resourceexhausted"),
        "{error:#}"
    );
}

#[test]
fn staged_wait_exact_cost_preserves_original_and_last_shared_holder() -> Result<()> {
    let source = record();
    let trigger = "{,:} \"wake\" \\ 路";
    let exact = gates(&source, trigger)?.into_iter().fold(header(), add);
    let budget = ledger()?;
    let material = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
    let mut staged = Vec::new();
    {
        let _entry = material.enter();
        assert_eq!(
            stage_triggered_wait(&mut staged, &source, wake(), trigger, at())?,
            source.id
        );
    }
    assert_eq!(source, record());
    let mut expected = source.clone();
    expected.mark_triggered(trigger, at());
    assert_eq!(staged, vec![(expected, wake())]);
    assert_eq!(material.finish()?, exact);
    let held = material.guard()?;
    let last = held.clone();
    drop(material);
    assert_eq!(budget.report()?.observed_bytes, exact.0);
    drop((staged, held));
    assert_eq!(budget.report()?.observed_bytes, exact.0);
    drop(last);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn each_staged_wait_gate_refuses_without_partial_publication() -> Result<()> {
    let source = record();
    let mut prefix = header();
    for next in gates(&source, "wake")? {
        for bytes_bound in [true, false] {
            let budget = ledger()?;
            let material = phase(&budget, add(prefix, next), bytes_bound)?;
            let mut staged = Vec::new();
            {
                let _entry = material.enter();
                exhausted(
                    &stage_triggered_wait(&mut staged, &source, wake(), "wake", at()).unwrap_err(),
                );
                assert!(staged.is_empty());
                assert_eq!(budget.report()?.observed_bytes, prefix.0);
                exhausted(
                    &stage_triggered_wait(&mut staged, &source, wake(), "wake", at()).unwrap_err(),
                );
                assert_eq!(budget.report()?.observed_bytes, prefix.0);
                assert_eq!(source, record());
            }
            assert!(material.finish().is_err());
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            drop(material);
            assert_eq!(budget.report()?.observed_bytes, 0);
        }
        prefix = add(prefix, next);
    }
    Ok(())
}

#[test]
fn staged_wait_uses_original_remainder_failure_and_tls_identity() -> Result<()> {
    use std::sync::Arc;
    let source = record();
    let body = gates(&source, "wake")?.into_iter().fold((0, 0), add);
    for bytes_bound in [true, false] {
        let budget = ledger()?;
        let material = phase(&budget, add(add(header(), body), body), bytes_bound)?;
        let outer_budget = ledger()?;
        let outer = ResourceReadMaterial::new(&outer_budget, &scope())?;
        let mut staged = Vec::new();
        {
            let _outside = outer.enter();
            let outside = ResourceReadMaterial::active_guard()?.unwrap();
            {
                let _entry = material.enter();
                let original = ResourceReadMaterial::active_guard()?.unwrap();
                stage_triggered_wait(&mut staged, &source, wake(), "wake", at())?;
                let first = staged.clone();
                assert!(Arc::ptr_eq(
                    &original,
                    &ResourceReadMaterial::active_guard()?.unwrap()
                ));
                exhausted(
                    &stage_triggered_wait(&mut staged, &source, wake(), "wake", at()).unwrap_err(),
                );
                assert_eq!(staged, first);
                exhausted(&ResourceReadMaterial::active_guard().unwrap_err());
                assert!(stage_triggered_wait(&mut staged, &source, wake(), "wake", at()).is_err());
                assert_eq!(staged, first);
            }
            assert!(Arc::ptr_eq(
                &outside,
                &ResourceReadMaterial::active_guard()?.unwrap()
            ));
        }
        assert!(material.finish().is_err());
        drop((staged, material));
        assert_eq!(budget.report()?.observed_bytes, 0);
        assert_eq!(outer.finish()?, header());
    }
    Ok(())
}

fn fixture(record: &WaitConditionRecord) -> Result<(tempfile::TempDir, RuntimeDb)> {
    let home = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    db.transaction(|tx| {
        crate::runtime_db::repositories::upsert_wait_condition_tx(tx, record)?;
        Ok(())
    })?;
    Ok((home, db))
}
fn message() -> (MessageEnvelope, QueueEntryRecord) {
    let mut message = MessageEnvelope::new(
        &scope().agent_id,
        MessageKind::TimerTick,
        MessageOrigin::Timer {
            timer_id: "body-timer".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "timer wake".into(),
        },
    );
    message.id = "body-wake".into();
    message.created_at = at() + chrono::Duration::seconds(1);
    let queue = QueueEntryRecord {
        message_id: message.id.clone(),
        agent_id: message.agent_id.clone(),
        priority: message.priority.clone(),
        status: QueueEntryStatus::Queued,
        created_at: message.created_at,
        updated_at: message.created_at,
    };
    (message, queue)
}
fn pool_cost(record: &WaitConditionRecord) -> Result<(u64, usize)> {
    Ok(add(
        cost(record, true)?,
        (std::mem::size_of::<WaitConditionRecord>() as u64 * 8, 8),
    ))
}
fn assert_unwritten(db: &RuntimeDb, source: &WaitConditionRecord) -> Result<()> {
    db.transaction(|tx| {
        for table in ["messages", "queue_entries", "audit_events"] {
            assert_eq!(
                tx.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                    .get::<_, usize>(0))?,
                0
            );
        }
        let saved: WaitConditionRecord = serde_json::from_str(&tx.query_row(
            "SELECT payload_json FROM wait_conditions WHERE wait_condition_id=?1",
            [&source.id],
            |row| row.get::<_, String>(0),
        )?)?;
        assert_eq!(&saved, source);
        Ok(())
    })
}

#[test]
fn real_reducer_refuses_each_body_gate_and_duplicate_or_generation_id_before_writes() -> Result<()>
{
    let source = record();
    let (_home, db) = fixture(&source)?;
    let (message, queue) = message();
    let mut prefix = add(header(), pool_cost(&source)?);
    for next in gates(&source, &message.id)? {
        for bytes_bound in [true, false] {
            let budget = ledger()?;
            let material = phase(&budget, add(prefix, next), bytes_bound)?;
            {
                let _entry = material.enter();
                exhausted(
                    &db.transaction(|tx| trigger_wait_and_enqueue_tx(tx, &message, &queue))
                        .unwrap_err(),
                );
                assert_eq!(budget.report()?.observed_bytes, prefix.0);
            }
            assert!(material.finish().is_err());
            drop(material);
            assert_eq!(budget.report()?.observed_bytes, 0);
            assert_unwritten(&db, &source)?;
        }
        prefix = add(prefix, next);
    }
    for duplicate in [false, true] {
        let mut source = record();
        let (mut message, queue) = self::message();
        if duplicate {
            source.mark_triggered(&message.id, at());
        } else {
            message
                .source_refs
                .insert("wait_generation".into(), "invalid".into());
        }
        let (_home, db) = fixture(&source)?;
        let prefix = add(header(), pool_cost(&source)?);
        for bytes_bound in [true, false] {
            let budget = ledger()?;
            let material = phase(&budget, add(prefix, cost(&source.id, false)?), bytes_bound)?;
            {
                let _entry = material.enter();
                exhausted(
                    &db.transaction(|tx| trigger_wait_and_enqueue_tx(tx, &message, &queue))
                        .unwrap_err(),
                );
                assert_eq!(budget.report()?.observed_bytes, prefix.0);
            }
            assert!(material.finish().is_err());
            drop(material);
            assert_eq!(budget.report()?.observed_bytes, 0);
            assert_unwritten(&db, &source)?;
        }
    }
    Ok(())
}
