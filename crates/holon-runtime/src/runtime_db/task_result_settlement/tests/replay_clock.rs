use super::*;
use crate::runtime_db::task_result_settlement::live_turn::observe_interrupted_result_anchor_tx;

fn replay_fixture(
    db: &RuntimeDb,
    reply: bool,
    initial: bool,
) -> (TaskResultSettlementRecord, TurnRecord) {
    let record = if reply {
        let boundary = reply_source(db);
        live_reply(db, 0, "live-parent", None, boundary)
    } else {
        live_result(db, 0, TaskKind::CommandTask, "live-parent", None)
    };
    prepare_recovery(db, &[record.clone()], reply);
    let mut turn = interrupted_turn("live-parent", &[record.clone()], reply);
    let idle = initial_only_turn(turn.clone(), "live-activation");
    turn.idle_input_batch = idle.idle_input_batch;
    if initial {
        turn.live_input_batches.clear();
    }
    queue(db, "live-anchor", QueueEntryStatus::Interrupted);
    rewrite_turn(db, &turn);
    let admitted = db
        .task_result_settlements()
        .latest_for_message(&record.message_id)
        .unwrap()
        .unwrap();
    (admitted, turn)
}

#[test]
fn stable_replay_observes_every_dynamic_time_gate_without_authorizing_early() -> Result<()> {
    for initial in [false, true] {
        for reply in [false, true] {
            let gates = if reply { 8 } else { 5 };
            for gate in 0..gates {
                let (_dir, db) = runtime_db();
                let (mut record, mut turn) = replay_fixture(&db, reply, initial);
                assert_eq!(
                    db.task_result_settlements()
                        .interrupted_admitted_result_anchor(
                            "agent-a",
                            &record.message_id,
                            Utc::now()
                        )?,
                    Some("live-anchor".into()),
                    "fixture initial={initial} reply={reply}"
                );
                let future = DateTime::from_timestamp_millis(
                    (Utc::now() + Duration::days(1)).timestamp_millis(),
                )
                .unwrap();
                // The immutable admission cutoff is independently valid here.
                turn.idle_input_batch.as_mut().unwrap().cutoff = future;
                rewrite_turn(&db, &turn);
                let mut task = db.tasks().latest(&record.task_id)?.unwrap();
                let mut message = db.messages().by_id(None, &record.message_id)?.unwrap();
                match gate {
                    0 => record.created_at = future,
                    1 => record.admitted_at = Some(future),
                    2 => task.created_at = future,
                    3 => task.updated_at = future,
                    4 => message.created_at = future,
                    5 => {
                        let mut reply = db.messages().by_id(None, "live-reply")?.unwrap();
                        reply.created_at = future;
                        db.messages().upsert(&reply)?;
                    }
                    6 | 7 => {
                        let id = if gate == 6 {
                            "live-request-delivery"
                        } else {
                            "live-response-delivery"
                        };
                        let mut delivery = db.agent_message_deliveries().latest(id)?.unwrap();
                        delivery.accepted_at = Some(future);
                        rewrite_delivery(&db, &delivery);
                    }
                    _ => unreachable!(),
                }
                db.transaction(|tx| update_tx(tx, &record))?;
                rewrite_task(&db, &task);
                db.messages().upsert(&message)?;
                let observed = db.transaction(|tx| {
                    observe_interrupted_result_anchor_tx(tx, "agent-a", &record.message_id)
                })?;
                assert_eq!(
                    observed,
                    Some(("live-anchor".into(), future)),
                    "initial={initial} reply={reply} gate={gate}"
                );
                for now in [
                    future - Duration::nanoseconds(1),
                    future,
                    future + Duration::days(1),
                    future - Duration::days(1),
                ] {
                    let original = db
                        .task_result_settlements()
                        .interrupted_admitted_result_anchor("agent-a", &record.message_id, now)?;
                    assert_eq!(
                        original,
                        (now >= future).then(|| "live-anchor".into()),
                        "initial={initial} reply={reply} gate={gate} now={now}"
                    );
                }
                // Observe must continue after future evidence and reject a later
                // non-time failure, rather than cache an eventual grant.
                message.delivery_surface = None;
                db.messages().upsert(&message)?;
                assert!(db
                    .transaction(|tx| observe_interrupted_result_anchor_tx(
                        tx,
                        "agent-a",
                        &record.message_id
                    ))?
                    .is_none());
            }
        }
    }
    Ok(())
}

#[test]
fn stable_replay_never_defers_missing_timestamps_or_fixed_admission_failures() -> Result<()> {
    for invalid in [
        "admitted",
        "request",
        "response",
        "fixed-cutoff",
        "queue",
        "generation",
    ] {
        let (_dir, db) = runtime_db();
        let (mut record, mut turn) = replay_fixture(&db, true, true);
        let future = Utc::now() + Duration::days(1);
        let mut task = db.tasks().latest(&record.task_id)?.unwrap();
        task.updated_at = future;
        match invalid {
            "admitted" => {
                record.admitted_at = None;
                // Corrupt only the source payload; keep the SQL state invariant.
                db.transaction(|tx| {
                    tx.execute(
                        "UPDATE task_result_settlements SET payload_json = ?2
                         WHERE result_identity = ?1",
                        params![record.result_identity, serde_json::to_string(&record)?],
                    )?;
                    Ok(())
                })?;
            }
            "request" | "response" => {
                let id = if invalid == "request" {
                    "live-request-delivery"
                } else {
                    "live-response-delivery"
                };
                let mut delivery = db.agent_message_deliveries().latest(id)?.unwrap();
                delivery.accepted_at = None;
                rewrite_delivery(&db, &delivery);
            }
            "fixed-cutoff" => {
                turn.idle_input_batch.as_mut().unwrap().cutoff =
                    record.created_at - Duration::nanoseconds(1);
                rewrite_turn(&db, &turn);
            }
            "queue" => queue(&db, "live-anchor", QueueEntryStatus::Processed),
            "generation" => {
                task.detail.as_mut().unwrap()["rejoin_generation"] = serde_json::json!(2)
            }
            _ => unreachable!(),
        }
        rewrite_task(&db, &task);
        assert!(
            db.transaction(|tx| observe_interrupted_result_anchor_tx(
                tx,
                "agent-a",
                &record.message_id
            ))?
            .is_none(),
            "{invalid}"
        );
        assert!(
            db.task_result_settlements()
                .interrupted_admitted_result_anchor(
                    "agent-a",
                    &record.message_id,
                    future + Duration::days(1)
                )?
                .is_none(),
            "{invalid}"
        );
    }
    Ok(())
}
