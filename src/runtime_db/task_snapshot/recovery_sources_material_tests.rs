use super::*;
use crate::runtime_db::task_snapshot::tests::{at, fresh, scope};
use crate::types::{AuthorityClass, MessageBody, MessageKind, MessageOrigin, Priority};

fn totals(tx: &Transaction<'_>, source: &str) -> Result<(usize, usize)> {
    Ok(tx.query_row(
        &format!("{MATERIAL} SELECT COUNT(*),COALESCE(SUM(length(CAST(payload_json AS BLOB))),0) FROM bodies"),
        params!["task-agent", source, "result", Option::<&str>::None],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?)
}

fn selected(tx: &Transaction<'_>) -> Result<()> {
    // Address-only fixtures: the preflight must not decode or qualify these bodies.
    tx.execute(
        "INSERT INTO task_result_settlements
         (result_identity,task_id,agent_id,message_id,rejoin_generation,parent_turn_id,
          state,created_at,updated_at,payload_json)
         VALUES('selected','task','task-agent','result',1,'parent','persisted_pending',
                '2026-01-01','2026-01-01','{}')",
        [],
    )?;
    Ok(())
}

#[test]
fn recovery_material_includes_claiming_wait_owner_and_trigger_bodies() -> Result<()> {
    let (_dir, db) = fresh()?;
    db.transaction(|tx| {
        selected(tx)?;
        let wait = crate::types::WaitConditionRecord {
            id: "claiming-wait".into(),
            agent_id: "task-agent".into(),
            work_item_id: Some("wait-owner".into()),
            status: crate::types::WaitConditionStatus::Resolved,
            kind: crate::types::WaitConditionKind::Task,
            source: Some("WaitFor".into()),
            subject_ref: Some("task".into()),
            waiting_for: "task result".into(),
            wake_sources: vec![crate::types::WakeSource::TaskResult {
                task_id: "task".into(),
            }],
            continuation: None,
            created_at: at(),
            updated_at: at(),
            expires_at: None,
            resolved_at: Some(at()),
            cancelled_at: None,
            turn_id: None,
            trigger_message_id: Some("wait-trigger".into()),
            triggered_at: Some(at()),
        };
        repositories::upsert_wait_condition_tx(tx, &wait)?;
        let before = totals(tx, "recovery")?;
        tx.execute(
            "INSERT INTO work_items(work_item_id,agent_id,state,objective,revision,
                                    created_at,updated_at,payload_json)
             VALUES('wait-owner','task-agent','open','wait owner',0,
                    '2026-01-01','2026-01-01',?1)",
            [r#"{"body":"owner body"}"#],
        )?;
        tx.execute(
            "INSERT INTO messages(evidence_id,message_id,agent_id,kind,created_at,payload_json)
             VALUES('trigger-evidence','wait-trigger','task-agent','internal_followup',
                    '2026-01-01',?1)",
            [r#"{"body":"trigger body"}"#],
        )?;
        let after = totals(tx, "recovery")?;
        assert_eq!(after.0, before.0 + 2);
        assert_eq!(
            after.1 - before.1,
            r#"{"body":"owner body"}"#.len() + r#"{"body":"trigger body"}"#.len()
        );
        tx.execute(
            "UPDATE messages SET agent_id='another-agent' WHERE message_id='wait-trigger'",
            [],
        )?;
        // The canonical predicate decodes this exact reference before rejecting its scope.
        assert_eq!(totals(tx, "recovery")?, after);
        let request = TaskDomainLoadRequest {
            recovery_at: None,
            query: Default::default(),
            capture: TaskCaptureRequest::current(),
            record_limit: 4096,
            byte_limit: after.1 - 1,
        };
        let original = scope();
        let mut material = read_preparation::TaskReadMaterial::new(&original, &request, None);
        let mut source = MessageEnvelope::new(
            "task-agent",
            MessageKind::InternalFollowup,
            MessageOrigin::System {
                subsystem: "task_result_recovery".into(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Next,
            MessageBody::Text {
                text: "recover".into(),
            },
        );
        source.id = "recovery".into();
        let error = preflight_predicate_tx(
            tx,
            &original,
            &source,
            Some("selected"),
            &request,
            &mut material,
        )
        .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<TaskSnapshotError>(),
            Some(TaskSnapshotError::ResourceExhausted("bytes"))
        ));
        Ok(())
    })
}

#[test]
fn recovery_material_follows_exact_delivery_attempt_and_attached_turn_only() -> Result<()> {
    let (_dir, db) = fresh()?;
    db.transaction(|tx| {
        selected(tx)?;
        let mut task = super::super::tests::task("task", crate::types::TaskStatus::Completed);
        task.recovery = Some(crate::types::TaskRecoverySpec::AgentMessageWait {
            summary: "reply".into(),
            message: "request".into(),
            target_agent_id: "another-agent".into(),
            authority_class: crate::types::AuthorityClass::RuntimeInstruction,
            delivery_id: Some("request".into()),
            after_delivery_rowid: Some(1),
            reply_deadline_at: None,
            requester_incarnation: Some(1),
        });
        task.detail = Some(serde_json::json!({"response_delivery_id":"response"}));
        repositories::upsert_task_tx(tx, &task)?;
        let before = totals(tx, "recovery")?;
        tx.execute(
            "INSERT INTO agent_message_deliveries
             (delivery_id,target_agent_id,idempotency_scope,idempotency_key_digest,
              request_digest,outcome,state,state_version,retryable,created_at,updated_at,payload_json)
             VALUES('response','task-agent','scope','response','digest','accepted','consumed',0,0,
                    '2026-01-01','2026-01-01',?1)",
            [r#"{"activation_id":"reply-attempt","turn_id":"reply-turn"}"#],
        )?;
        tx.execute(
            "INSERT INTO agent_message_deliveries
             (delivery_id,target_agent_id,idempotency_scope,idempotency_key_digest,
              request_digest,outcome,state,state_version,retryable,created_at,updated_at,payload_json)
             VALUES('request','another-agent','scope','request','digest','accepted','queued',0,0,
                    '2026-01-01','2026-01-01','{}')",
            [],
        )?;
        tx.execute(
            "INSERT INTO execution_protocol_attempts
             (attempt_id,agent_id,lifecycle_state,source_identity_json,source_generation,payload_json)
             VALUES('reply-attempt','task-agent','settled','{}',1,
                    '{\"turn_id\":\"reply-turn\"}')",
            [],
        )?;
        tx.execute(
            "INSERT INTO turn_records(turn_id,agent_id,turn_index,created_at,payload_json)
             VALUES('reply-turn','task-agent',1,'2026-01-01','{}')",
            [],
        )?;
        let after = totals(tx, "recovery")?;
        assert_eq!(after.0, before.0 + 4);
        tx.execute(
            "INSERT INTO execution_protocol_attempts
             (attempt_id,agent_id,lifecycle_state,source_identity_json,source_generation,payload_json)
             VALUES('unrelated','task-agent','settled','{}',1,
                    '{\"source_message_id\":\"unrelated\"}')",
            [],
        )?;
        assert_eq!(totals(tx, "recovery")?, after);
        Ok(())
    })
}

#[test]
fn recovery_material_covers_charged_roots_without_admitting_future_epochs() -> Result<()> {
    let (_dir, db) = fresh()?;
    db.transaction(|tx| {
        selected(tx)?;
        tx.execute(
            "UPDATE task_result_settlements SET recovery_epoch=2 WHERE result_identity='selected'",
            [],
        )?;
        let before = totals(tx, "recovery")?;
        for (epoch, root) in [(1, "older-root"), (2, "current-root"), (3, "future-root")] {
            tx.execute(
                "INSERT INTO task_result_recovery_admissions
                 (result_identity,recovery_epoch,root_attempt_id,admitted_at)
                 VALUES('selected',?1,?2,'2026-01-01')",
                params![epoch, root],
            )?;
            tx.execute(
                "INSERT INTO execution_protocol_attempts
                 (attempt_id,agent_id,lifecycle_state,source_identity_json,source_generation,payload_json)
                 VALUES(?1,'task-agent','settled','{}',1,'{}')",
                [root],
            )?;
        }
        let after = totals(tx, "recovery")?;
        assert_eq!(after.0, before.0 + 2);
        assert_eq!(after.1, before.1 + 4);
        Ok(())
    })
}
