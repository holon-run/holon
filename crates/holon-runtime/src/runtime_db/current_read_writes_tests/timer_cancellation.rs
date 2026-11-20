use super::*;
use crate::runtime_db::TimerCancel;
use crate::types::{TimerRecord, WaitConditionRecord};

#[test]
fn current_timer_cancellation_addresses_use_original_unadmitted_wait_selection() -> Result<()> {
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let expected: TimerRecord = serde_json::from_value(serde_json::json!({
        "id": "timer-路", "agent_id": f.scope.agent_id,
        "created_at": f.at, "duration_ms": 1000, "repeat": false,
        "status": "active", "next_fire_at": f.at,
    }))?;
    let mut record = expected.clone();
    record.status = crate::types::TimerStatus::Cancelled;
    record.next_fire_at = None;
    let command = TimerCancel {
        expected,
        record,
        agent_state: None,
        cancelled_at: f.at,
    };
    let wait = |id: &str, status: &str, subject: &str| -> Result<WaitConditionRecord> {
        Ok(serde_json::from_value(serde_json::json!({
            "id": id, "agent_id": f.scope.agent_id,
            "work_item_id": id, "status": status, "kind": "timer",
            "waiting_for": "original cancellation address", "subject_ref": subject,
            "trigger_message_id": id,
            "created_at": f.at, "updated_at": f.at,
        }))?)
    };
    let cases = [
        ("active-路", "active", "timer-路", true),
        ("triggered-路", "triggered", "timer-路", true),
        ("admitted-路", "triggered", "timer-路", false),
        ("closed-路", "cancelled", "timer-路", false),
        ("other-路", "active", "another-timer", false),
        ("late-路", "active", "timer-路", false),
    ];
    for (id, ..) in &cases {
        let mut owner = crate::types::WorkItemRecord::new(
            &f.scope.agent_id,
            "cancellation address owner",
            crate::types::WorkItemState::Open,
        );
        owner.id = (*id).into();
        f.db.work_items().insert_new(&owner)?;
    }
    let timer_event = format!("audit:timer-cancelled:{}", command.record.id);
    let ids: Vec<_> = std::iter::once(timer_event)
        .chain(
            cases
                .iter()
                .map(|(id, ..)| format!("audit:timer-wait-cancelled:{id}")),
        )
        .collect();
    let ack = f.db.transaction(|tx| {
        for (id, status, subject, _) in &cases[..cases.len() - 1] {
            crate::runtime_db::repositories::upsert_wait_condition_tx(
                tx,
                &wait(id, status, subject)?,
            )?;
        }
        crate::runtime_db::repositories::upsert_queue_entry_tx(
            tx,
            &crate::types::QueueEntryRecord {
                message_id: "admitted-路".into(),
                agent_id: f.scope.agent_id.clone(),
                priority: Priority::Normal,
                status: crate::types::QueueEntryStatus::Processed,
                created_at: f.at,
                updated_at: f.at,
            },
        )?;
        let mut native = NativeInputWriteProof::prepare_tx(
            tx,
            &f.scope,
            &ledger,
            (4 * 1024 * 1024, 4096),
            None,
            |_| Ok(()),
        )?
        .context("original Native source")?;
        let current = native.with_input_phase(tx, &ledger, None, |material| {
            let mut current = OriginalCurrentWrites::prepare(tx, &f.scope, material, |_| Ok(()))?
                .context("original Current source")?;
            current.prepare_timer_cancellation_events(&command, material)?;
            Ok(current)
        })?;
        crate::runtime_db::repositories::upsert_wait_condition_tx(
            tx,
            &wait("late-路", "active", "timer-路")?,
        )?;
        for id in &ids {
            let mut event = AuditEvent::legacy("generated", serde_json::json!({}));
            event.id = id.clone();
            append_audit_event_tx(tx, Some(&f.scope.agent_id), &event)?;
        }
        let ack = native.with_input_phase(tx, &ledger, None, |material| {
            current.finish(material, |visit| {
                for id in &ids {
                    visit(CurrentFamily::Event, id)?;
                }
                Ok(())
            })
        })?;
        native.finish_delta_tx(tx, &mut Default::default(), &ledger, None)?;
        Ok(ack)
    })?;
    for (index, id) in ids.iter().enumerate() {
        let qualified = index == 0 || cases[index - 1].3;
        let fact = &ack.facts()[&(CurrentFamily::Event, id.clone())];
        if qualified {
            assert!(
                matches!(fact, CurrentAddressFact::Qualified(_)),
                "{id}: {fact:?}"
            );
        } else {
            assert_eq!(fact, &CurrentAddressFact::Unprepared, "{id}");
        }
    }
    drop(ack);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
