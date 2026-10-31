use super::*;
use crate::types::{
    AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility,
};

async fn retain_source_then_clear_revision_drift(
    runtime: &RuntimeHandle,
    source: &MessageEnvelope,
) {
    // The standalone batch-target fixture has no host registration.
    if runtime
        .runtime_db()
        .agent_identities()
        .latest(&source.agent_id)
        .unwrap()
        .is_none()
    {
        runtime
            .runtime_db()
            .agent_identities()
            .upsert(&AgentIdentityRecord::new(
                &source.agent_id,
                AgentKind::Named,
                AgentVisibility::Public,
                AgentOwnership::SelfOwned,
                AgentProfilePreset::PublicNamed,
                None,
                None,
            ))
            .unwrap();
    }
    let work = runtime
        .create_work_item(
            "retained input revision drift".into(),
            Some(WorkItemPlanStatus::Draft),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let original = runtime
        .runtime_db()
        .work_items()
        .latest(&work.id)
        .unwrap()
        .unwrap();
    let mut changed = original.clone();
    changed.revision += 1;
    runtime
        .runtime_db()
        .transaction(|tx| {
            tx.execute(
                "UPDATE work_items SET revision = ?1, payload_json = ?2
                 WHERE work_item_id = ?3",
                rusqlite::params![
                    changed.revision,
                    serde_json::to_string(&changed)?,
                    changed.id
                ],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(runtime
        .runtime_db()
        .source_revision_repair_blocks_source(source, Utc::now())
        .unwrap());
    // Repair registration only: preserve its owner state and monotonic revision.
    let mut registration = runtime
        .runtime_db()
        .transitions()
        .load_execution_protocol_state_if_initialized(&source.agent_id)
        .unwrap()
        .unwrap()
        .work_items[&work.id]
        .clone();
    registration.source_revision = changed.revision;
    runtime
        .runtime_db()
        .transaction(|tx| {
            tx.execute(
                "UPDATE execution_protocol_work_items SET payload_json = ?1
                 WHERE agent_id = ?2 AND work_item_id = ?3",
                rusqlite::params![
                    serde_json::to_string(&registration)?,
                    source.agent_id,
                    work.id
                ],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(runtime
        .runtime_db()
        .source_revision_drifts(&source.agent_id)
        .unwrap()
        .is_empty());
    assert!(runtime
        .runtime_db()
        .source_revision_repair_blocks_source(source, Utc::now())
        .unwrap());
    let assessments = runtime
        .runtime_db()
        .source_revision_repair_assessments(&source.agent_id)
        .unwrap();
    assert_eq!(assessments.len(), 1);
    assert!(!assessments[0].drift_present);
}

fn operator_input(priority: Priority, text: &str) -> MessageEnvelope {
    MessageEnvelope::new(
        "batch-target",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("test".into()),
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        priority,
        MessageBody::Text { text: text.into() },
    )
    .with_admission(
        MessageDeliverySurface::CliPrompt,
        crate::types::AdmissionContext::LocalProcess,
    )
}

fn request_text(request: &ProviderTurnRequest) -> String {
    request
        .prompt_frame
        .context_blocks
        .iter()
        .map(|block| block.text.as_str())
        .chain(
            request
                .conversation
                .iter()
                .filter_map(|message| match message {
                    ConversationMessage::UserText(text) => Some(text.as_str()),
                    _ => None,
                }),
        )
        .collect::<Vec<_>>()
        .join("\n")
}

async fn assert_source_still_queued(runtime: &RuntimeHandle, source: &MessageEnvelope) {
    assert_eq!(
        runtime
            .runtime_db()
            .queue_entries()
            .latest(&source.id)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Queued
    );
    assert!(runtime
        .inner
        .agent
        .lock()
        .await
        .queue
        .peek_next_matching(|message| message.id == source.id)
        .is_some());
}

#[tokio::test]
async fn p1_repair_retained_interject_is_not_replayed_by_new_prompt_after_drift_clears() {
    let (_home, _host, _sender, target, provider) = fixture().await;
    let old = target
        .enqueue(operator_input(
            Priority::Interject,
            "retained-old-interject-body",
        ))
        .await
        .unwrap();
    retain_source_then_clear_revision_drift(&target, &old).await;
    let fresh = target
        .enqueue(operator_input(
            Priority::Normal,
            "fresh-legal-operator-body",
        ))
        .await
        .unwrap();

    process_one(&target).await;

    let requests = provider.requests.lock().await;
    assert_eq!(
        requests.len(),
        1,
        "retained input must not trigger a continuation"
    );
    assert!(request_text(&requests[0]).contains("fresh-legal-operator-body"));
    assert!(!request_text(&requests[0]).contains("retained-old-interject-body"));
    let turn = target.storage().read_recent_turns(1).unwrap().remove(0);
    assert_eq!(turn.input_message_ids, [fresh.id]);
    assert_source_still_queued(&target, &old).await;
}

#[tokio::test]
async fn p1_repair_retained_follower_is_not_batched_by_new_message_after_drift_clears() {
    let (_home, _host, sender, target, provider) = fixture().await;
    let old_id = send(&sender, &target, "retained", "retained-old-follower-body").await;
    let old = target
        .storage()
        .read_message_by_id(&old_id)
        .unwrap()
        .unwrap();
    retain_source_then_clear_revision_drift(&target, &old).await;
    let fresh = send(&sender, &target, "fresh-anchor", "fresh-legal-message-body").await;
    let follower = send(
        &sender,
        &target,
        "fresh-follower",
        "fresh-legal-follower-body",
    )
    .await;

    process_one(&target).await;

    let turn = target.storage().read_recent_turns(1).unwrap().remove(0);
    assert_eq!(turn.input_message_ids, [fresh, follower]);
    let requests = provider.requests.lock().await;
    assert_eq!(requests.len(), 1);
    let text = request_text(&requests[0]);
    assert!(text.contains("fresh-legal-message-body"));
    assert!(text.contains("fresh-legal-follower-body"));
    assert!(!text.contains("retained-old-follower-body"));
    assert_source_still_queued(&target, &old).await;
}

#[tokio::test]
async fn p1_repair_retained_source_rejects_raw_claim_and_interject_after_drift_clears() {
    let (_home, _host, sender, target, _) = fixture().await;
    let id = send(&sender, &target, "retained", "retained-raw-consumption").await;
    let old = target.storage().read_message_by_id(&id).unwrap().unwrap();
    retain_source_then_clear_revision_drift(&target, &old).await;
    let record = target
        .runtime_db()
        .queue_entries()
        .latest(&id)
        .unwrap()
        .unwrap();

    assert!(!target
        .runtime_db()
        .queue_entries()
        .try_claim_queued_message(&record)
        .unwrap());
    assert!(!target
        .runtime_db()
        .transaction(|tx| {
            crate::runtime_db::repositories::try_interject_queued_message_tx(tx, &record)
        })
        .unwrap());
    assert_source_still_queued(&target, &old).await;
}

#[tokio::test]
async fn p1_repair_retained_source_rejects_commit_queue_atomically_after_drift_clears() {
    use crate::runtime_db::transitions::{
        AgentStateMutation, QueueMutation, QueueOperation, QueueTransitionCommand,
    };
    use crate::types::AuditEvent;

    let (_home, _host, sender, target, _) = fixture().await;
    let old_id = send(&sender, &target, "retained", "retained-direct-consumption").await;
    let old = target
        .storage()
        .read_message_by_id(&old_id)
        .unwrap()
        .unwrap();
    retain_source_then_clear_revision_drift(&target, &old).await;
    let fresh_id = send(&sender, &target, "fresh", "fresh-direct-consumption").await;
    let before = target
        .runtime_db()
        .agent_states()
        .latest(&old.agent_id)
        .unwrap()
        .unwrap();
    let mut changed = before.clone();
    changed.last_wake_reason = Some("direct-consumption-probe".into());
    let event = AuditEvent::legacy(
        "retained_source_consumption_probe",
        serde_json::json!({ "agent_id": old.agent_id }),
    );
    let command = |message_id: &str| {
        let mut record = target
            .runtime_db()
            .queue_entries()
            .latest(message_id)
            .unwrap()
            .unwrap();
        record.status = QueueEntryStatus::Interjected;
        record.updated_at = Utc::now();
        QueueTransitionCommand {
            agent_id: old.agent_id.clone(),
            operation: QueueOperation::Interject,
            mutation: QueueMutation::Consume(record),
            scheduler_claim_work_item: None,
            agent_state: Some(AgentStateMutation {
                expected: Some(Box::new(before.clone())),
                record: Box::new(changed.clone()),
            }),
            message_evidence: Vec::new(),
            transcript_entries: Vec::new(),
            turn_record: None,
            audit_events: vec![event.clone()],
            notify_scheduler: true,
            fault: None,
            brief_evidence: Vec::new(),
        }
    };
    let publication_counts = || {
        target
            .runtime_db()
            .transaction(|tx| {
                Ok((
                    tx.query_row(
                        "SELECT COUNT(*) FROM audit_events WHERE agent_id = ?1",
                        [&old.agent_id],
                        |row| row.get::<_, u64>(0),
                    )?,
                    tx.query_row(
                        "SELECT COUNT(*) FROM runtime_index_outbox WHERE agent_id = ?1",
                        [&old.agent_id],
                        |row| row.get::<_, u64>(0),
                    )?,
                ))
            })
            .unwrap()
    };
    let publications_before = publication_counts();
    let protocol_before = target
        .runtime_db()
        .transitions()
        .load_execution_protocol_state_if_initialized(&old.agent_id)
        .unwrap();
    let queued_before = target.runtime_db().queue_entries().latest(&old_id).unwrap();

    let rejected = target
        .runtime_db()
        .transitions()
        .commit_queue(&command(&old_id))
        .unwrap();

    assert!(
        !rejected.applied,
        "commit_queue must reject the retained source"
    );
    assert!(rejected.effects.agent_state.is_none());
    assert!(rejected.effects.audit_events.is_empty());
    assert!(!rejected.effects.notify_scheduler);
    assert_eq!(
        target
            .runtime_db()
            .agent_states()
            .latest(&old.agent_id)
            .unwrap(),
        Some(before.clone())
    );
    assert_eq!(
        target.runtime_db().queue_entries().latest(&old_id).unwrap(),
        queued_before
    );
    assert_eq!(publication_counts(), publications_before);
    assert_eq!(
        serde_json::to_value(
            target
                .runtime_db()
                .transitions()
                .load_execution_protocol_state_if_initialized(&old.agent_id)
                .unwrap()
        )
        .unwrap(),
        serde_json::to_value(protocol_before).unwrap()
    );

    // The same transaction is valid for a fresh source, not a malformed command.
    let accepted = target
        .runtime_db()
        .transitions()
        .commit_queue(&command(&fresh_id))
        .unwrap();
    assert!(accepted.applied);
    assert_eq!(
        target
            .runtime_db()
            .agent_states()
            .latest(&old.agent_id)
            .unwrap(),
        Some(changed)
    );
    assert_eq!(
        target
            .runtime_db()
            .queue_entries()
            .latest(&fresh_id)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Interjected
    );
    assert_eq!(publication_counts().0, publications_before.0 + 1);
    assert_source_still_queued(&target, &old).await;
}
