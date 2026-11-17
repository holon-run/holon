use super::*;
use crate::types::{
    AdmissionContext, AuthorityClass, MessageBody, MessageDeliverySurface, MessageEnvelope,
    MessageKind, MessageOrigin, Priority,
};

fn input(id: &str) -> MessageEnvelope {
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
            text: "continue".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::CliPrompt,
        AdmissionContext::LocalProcess,
    );
    message.id = id.into();
    message
}

fn strand(db: &RuntimeDb, revision: u64) -> Result<()> {
    let mut work = db.work_items().latest("work-a")?.unwrap();
    work.revision = revision;
    db.transaction(|tx| {
        tx.execute(
            "UPDATE work_items SET revision = ?1, payload_json = ?2
             WHERE agent_id = 'agent-a' AND work_item_id = 'work-a'",
            params![revision, serde_json::to_string(&work)?],
        )?;
        Ok(())
    })
}

fn publish(db: &RuntimeDb, brief: &crate::types::BriefRecord) -> Result<u64> {
    let committed = db
        .commit_source_revision_repair_brief(brief, &[])?
        .context("expected current repair publication")?;
    Ok(committed.event.event_seq)
}

#[test]
fn repair_blocks_deduplicate_episode_but_receipt_every_operator_input_across_restart() -> Result<()>
{
    let (dir, db) = database()?;
    canonical_root(&db)?;
    strand(&db, 2)?;
    let before = dispatch_authorization_snapshot(&db)?;
    assert!(db.source_revision_repair_blocks_source(&input("one"), now())?);
    assert!(db.source_revision_repair_blocks_source(&input("one"), now())?);
    assert!(db.source_revision_repair_blocks_source(&input("two"), now())?);
    assert_eq!(dispatch_authorization_snapshot(&db)?, before);
    let assessments = db.source_revision_repair_assessments("agent-a")?;
    assert_eq!(assessments.len(), 1);
    assert_eq!(assessments[0].incarnation, 1);
    assert_eq!(assessments[0].drift.generation, 1);
    assert_eq!(assessments[0].retained_source_count, 2);
    assert!(assessments[0].drift_present);
    assert!(assessments[0].alert_created_event_seq.is_none());
    let pending = db.pending_source_revision_repair_briefs("agent-a")?;
    assert_eq!(pending.len(), 3, "one alert and one receipt per input");
    assert_eq!(
        pending
            .iter()
            .filter(|brief| brief.id.ends_with(":alert"))
            .count(),
        1
    );
    for id in ["one", "two"] {
        assert!(pending.iter().any(|brief| {
            brief.related_message_id.as_deref() == Some(id)
                && brief.text.contains("this input was not executed")
        }));
    }
    drop(db);
    let db = reopen(&dir)?;
    assert_eq!(
        serde_json::to_value(db.pending_source_revision_repair_briefs("agent-a")?)?,
        serde_json::to_value(&pending)?,
        "crash before publication preserves exact payloads"
    );
    let published = pending
        .iter()
        .map(|brief| publish(&db, brief))
        .collect::<Result<Vec<_>>>()?;
    drop(db);
    let db = reopen(&dir)?;
    for (brief, sequence) in pending.iter().zip(published) {
        assert_eq!(
            publish(&db, brief)?,
            sequence,
            "crash after commit reuses event"
        );
    }
    assert!(db
        .pending_source_revision_repair_briefs("agent-a")?
        .is_empty());
    assert_eq!(
        db.audit_events()
            .recent(Some("agent-a"), 100)?
            .iter()
            .filter(|event| event.kind == "brief_created")
            .count(),
        3,
    );
    assert!(db.source_revision_repair_assessments("agent-a")?[0]
        .alert_created_event_seq
        .is_some());
    assert_eq!(dispatch_authorization_snapshot(&db)?, before);
    Ok(())
}

#[test]
fn repair_blocks_brief_event_and_publication_linkage_commit_atomically() -> Result<()> {
    let (_dir, db) = database()?;
    canonical_root(&db)?;
    strand(&db, 2)?;
    assert!(db.source_revision_repair_blocks_source(&input("one"), now())?);
    let pending = db.pending_source_revision_repair_briefs("agent-a")?;
    let brief = &pending[0];
    let mut changed = brief.clone();
    changed.text.push_str(" changed payload");
    assert!(db
        .commit_source_revision_repair_brief(&changed, &[])
        .is_err());
    db.transaction(|tx| {
        tx.execute_batch(
            "CREATE TRIGGER reject_repair_linkage
             BEFORE UPDATE OF created_event_seq ON source_revision_repair_block_publications
             BEGIN SELECT RAISE(ABORT, 'test linkage failure'); END;",
        )?;
        Ok(())
    })?;
    let before = dispatch_authorization_snapshot(&db)?;
    assert!(db.commit_source_revision_repair_brief(brief, &[]).is_err());
    assert_eq!(dispatch_authorization_snapshot(&db)?, before);
    assert_eq!(
        serde_json::to_value(db.pending_source_revision_repair_briefs("agent-a")?)?,
        serde_json::to_value(&pending)?,
    );
    db.with_read(|connection| {
        let count: u64 =
            connection.query_row("SELECT COUNT(*) FROM briefs", [], |row| row.get(0))?;
        assert_eq!(count, 0, "Brief insertion rolls back with linkage failure");
        Ok(())
    })?;
    assert!(!db
        .audit_events()
        .recent(Some("agent-a"), 100)?
        .iter()
        .any(|event| event.kind == "brief_created"));
    db.transaction(|tx| {
        tx.execute_batch("DROP TRIGGER reject_repair_linkage")?;
        Ok(())
    })?;
    let sequence = publish(&db, brief)?;
    assert_eq!(publish(&db, brief)?, sequence);
    assert!(!db
        .pending_source_revision_repair_briefs("agent-a")?
        .iter()
        .any(|pending| pending.id == brief.id));
    Ok(())
}

#[test]
fn repair_blocks_publication_rechecks_incarnation_after_reading_outbox() -> Result<()> {
    let (_dir, db) = database()?;
    canonical_root(&db)?;
    strand(&db, 2)?;
    assert!(db.source_revision_repair_blocks_source(&input("old"), now())?);
    let pending = db.pending_source_revision_repair_briefs("agent-a")?;
    db.transaction(|tx| {
        let payload: String = tx.query_row(
            "SELECT payload_json FROM agent_identities WHERE agent_id = 'agent-a'",
            [],
            |row| row.get(0),
        )?;
        let mut identity: AgentIdentityRecord = serde_json::from_str(&payload)?;
        identity.incarnation += 1;
        tx.execute(
            "UPDATE agent_identities SET payload_json = ?1 WHERE agent_id = 'agent-a'",
            [serde_json::to_string(&identity)?],
        )?;
        Ok(())
    })?;
    for brief in pending {
        assert!(db
            .commit_source_revision_repair_brief(&brief, &[])?
            .is_none());
    }
    assert!(!db
        .audit_events()
        .recent(Some("agent-a"), 100)?
        .iter()
        .any(|event| event.kind == "brief_created"));
    assert!(db
        .pending_source_revision_repair_briefs("agent-a")?
        .is_empty());
    assert!(db.source_revision_repair_blocks_source(&input("new"), now())?);
    assert_eq!(
        db.pending_source_revision_repair_briefs("agent-a")?.len(),
        2
    );
    Ok(())
}

#[test]
fn repair_blocks_publication_failure_rolls_back_episode_and_source() -> Result<()> {
    let (_dir, db) = database()?;
    canonical_root(&db)?;
    strand(&db, 2)?;
    db.transaction(|tx| {
        tx.execute_batch(
            "CREATE TRIGGER reject_repair_publication
             BEFORE INSERT ON source_revision_repair_block_publications
             BEGIN SELECT RAISE(ABORT, 'test publication failure'); END;",
        )?;
        Ok(())
    })?;
    let before = dispatch_authorization_snapshot(&db)?;
    assert!(db
        .source_revision_repair_blocks_source(&input("one"), now())
        .is_err());
    assert!(db.source_revision_repair_assessments("agent-a")?.is_empty());
    assert!(db
        .pending_source_revision_repair_briefs("agent-a")?
        .is_empty());
    assert_eq!(dispatch_authorization_snapshot(&db)?, before);
    db.transaction(|tx| {
        tx.execute_batch("DROP TRIGGER reject_repair_publication")?;
        Ok(())
    })?;
    assert!(db.source_revision_repair_blocks_source(&input("one"), now())?);
    assert_eq!(db.source_revision_repair_assessments("agent-a")?.len(), 1);
    Ok(())
}

#[test]
fn repair_blocks_revalidate_drift_without_reviving_retained_or_paused_owner() -> Result<()> {
    let (_dir, db) = database()?;
    canonical_root(&db)?;
    strand(&db, 2)?;
    assert!(db.source_revision_repair_blocks_source(&input("retained"), now())?);
    let old = db.source_revision_repair_assessments("agent-a")?[0]
        .episode_id
        .clone();
    db.transaction(|tx| {
        let execution = WorkItemExecutionRecord {
            source_revision: 2,
            state: WorkItemExecutionState::Paused {
                generation: 1,
                reason: "real-business-repair".into(),
            },
        };
        tx.execute(
            "UPDATE execution_protocol_work_items SET payload_json = ?1
             WHERE agent_id = 'agent-a' AND work_item_id = 'work-a'",
            [serde_json::to_string(&execution)?],
        )?;
        Ok(())
    })?;
    let before = dispatch_authorization_snapshot(&db)?;
    assert!(!db.source_revision_repair_assessments("agent-a")?[0].drift_present);
    assert!(db.source_revision_repair_blocks_source(&input("retained"), now())?);
    assert!(!db.source_revision_repair_blocks_source(&input("fresh"), now())?);
    assert_eq!(dispatch_authorization_snapshot(&db)?, before);
    strand(&db, 3)?;
    assert!(db.source_revision_repair_blocks_source(&input("new-fault"), now())?);
    let assessments = db.source_revision_repair_assessments("agent-a")?;
    assert_eq!(assessments.len(), 2);
    assert_eq!(
        assessments
            .iter()
            .filter(|episode| episode.drift_present)
            .count(),
        1
    );
    assert_ne!(
        assessments
            .iter()
            .find(|episode| episode.drift_present)
            .unwrap()
            .episode_id,
        old,
    );
    Ok(())
}

#[test]
fn repair_blocks_concurrent_observers_share_one_episode_and_alert() -> Result<()> {
    let (_dir, db) = database()?;
    canonical_root(&db)?;
    strand(&db, 2)?;
    let left = db.clone();
    let right = db.clone();
    std::thread::scope(|scope| {
        let one = scope.spawn(|| left.source_revision_repair_blocks_source(&input("one"), now()));
        let two = scope.spawn(|| right.source_revision_repair_blocks_source(&input("two"), now()));
        assert!(one.join().unwrap().unwrap());
        assert!(two.join().unwrap().unwrap());
    });
    assert_eq!(db.source_revision_repair_assessments("agent-a")?.len(), 1);
    assert_eq!(
        db.pending_source_revision_repair_briefs("agent-a")?.len(),
        3
    );
    Ok(())
}

#[test]
fn repair_blocks_incarnation_and_generation_are_separate_expectations() -> Result<()> {
    let (_dir, db) = database()?;
    canonical_root(&db)?;
    strand(&db, 2)?;
    assert!(db.source_revision_repair_blocks_source(&input("old"), now())?);
    db.transaction(|tx| {
        let payload: String = tx.query_row(
            "SELECT payload_json FROM agent_identities WHERE agent_id = 'agent-a'",
            [],
            |row| row.get(0),
        )?;
        let mut identity: AgentIdentityRecord = serde_json::from_str(&payload)?;
        identity.incarnation += 1;
        tx.execute(
            "UPDATE agent_identities SET payload_json = ?1 WHERE agent_id = 'agent-a'",
            [serde_json::to_string(&identity)?],
        )?;
        Ok(())
    })?;
    assert!(db
        .pending_source_revision_repair_briefs("agent-a")?
        .is_empty());
    assert!(db.source_revision_repair_blocks_source(&input("new"), now())?);
    let assessments = db.source_revision_repair_assessments("agent-a")?;
    assert_eq!(assessments.len(), 2);
    assert_eq!(
        assessments
            .iter()
            .filter(|episode| episode.drift_present)
            .count(),
        1
    );
    assert_eq!(
        assessments
            .iter()
            .find(|episode| episode.drift_present)
            .unwrap()
            .incarnation,
        2
    );
    db.transaction(|tx| {
        let execution = WorkItemExecutionRecord {
            source_revision: 1,
            state: WorkItemExecutionState::Paused {
                generation: 2,
                reason: "next".into(),
            },
        };
        tx.execute(
            "UPDATE execution_protocol_work_items SET payload_json = ?1
             WHERE agent_id = 'agent-a' AND work_item_id = 'work-a'",
            [serde_json::to_string(&execution)?],
        )?;
        Ok(())
    })?;
    assert!(db.source_revision_repair_blocks_source(&input("next"), now())?);
    let assessments = db.source_revision_repair_assessments("agent-a")?;
    assert_eq!(assessments.len(), 3);
    let current = assessments
        .iter()
        .find(|episode| episode.drift_present)
        .unwrap();
    assert_eq!(current.incarnation, 2);
    assert_eq!(current.drift.generation, 2);
    Ok(())
}

#[test]
fn repair_blocks_repaired_then_identical_fault_gets_a_new_episode() -> Result<()> {
    let (_dir, db) = database()?;
    canonical_root(&db)?;
    strand(&db, 2)?;
    assert!(db.source_revision_repair_blocks_source(&input("first"), now())?);
    let first = db.source_revision_repair_assessments("agent-a")?[0]
        .episode_id
        .clone();
    strand(&db, 1)?;
    assert!(!db.source_revision_repair_blocks_source(&input("after-repair"), now())?);
    strand(&db, 2)?;
    assert!(db.source_revision_repair_blocks_source(&input("recurrence"), now())?);
    let assessments = db.source_revision_repair_assessments("agent-a")?;
    assert_eq!(assessments.len(), 2);
    let current = assessments
        .iter()
        .filter(|episode| episode.drift_present)
        .collect::<Vec<_>>();
    assert_eq!(current.len(), 1);
    assert_ne!(current[0].episode_id, first);
    assert_eq!(
        db.pending_source_revision_repair_briefs("agent-a")?
            .iter()
            .filter(|brief| brief.id.ends_with(":alert"))
            .count(),
        2
    );
    Ok(())
}

#[test]
fn repair_blocks_preserve_prior_io_side_effects_and_terminal_source_fences() -> Result<()> {
    use crate::types::{QueueEntryRecord, QueueEntryStatus};
    let (_dir, db) = database()?;
    let attempt = canonical_root(&db)?;
    let episode = canonical_episode(&db, &attempt)?;
    let scope = transport("prior-io");
    let admission = lease(&db, std::slice::from_ref(&scope))?;
    let dispatch = charge(&db, &episode, &admission, "prior-send", 1)?;
    assert!(db.record_provider_recovery_request_outcome(
        &dispatch,
        ProviderRecoveryRequestOutcome::NetworkFailure(&scope),
        true,
        now(),
    )?);
    let episode = refresh(&db, &episode)?;
    assert_eq!(episode.total_requests, 1);
    assert!(episode.side_effect_boundary);
    strand(&db, 2)?;
    for (index, status) in [
        QueueEntryStatus::Processed,
        QueueEntryStatus::Aborted,
        QueueEntryStatus::Dropped,
        QueueEntryStatus::Quarantined,
    ]
    .into_iter()
    .enumerate()
    {
        let source = input(&format!("terminal-source-{index}"));
        assert!(db.source_revision_repair_blocks_source(&source, now())?);
        let record = QueueEntryRecord {
            message_id: source.id.clone(),
            agent_id: source.agent_id.clone(),
            priority: Priority::Normal,
            status,
            created_at: now(),
            updated_at: now(),
        };
        db.queue_entries().upsert(&record)?;
        let before = dispatch_authorization_snapshot(&db)?;
        assert!(db.source_revision_repair_blocks_source(&source, now())?);
        assert_eq!(db.queue_entries().latest(&source.id)?, Some(record));
        assert_eq!(dispatch_authorization_snapshot(&db)?, before);
        let current = refresh(&db, &episode)?;
        assert_eq!(current.total_requests, 1);
        assert!(current.side_effect_boundary);
    }
    Ok(())
}

#[test]
fn legacy_needs_repair_rows_reevaluate_when_loaded() -> Result<()> {
    let (_dir, db) = database()?;
    let attempt = canonical_root(&db)?;
    let legacy = serde_json::json!({
        "source_revision": 2,
        "state": {
            "state": "needs_repair",
            "generation": 1,
            "repair_id": "work_item_waits_ambiguous:work-a"
        }
    });
    db.transaction(|tx| {
        // A stored needs_repair row always pairs with the attempt that was
        // settled by the repair outcome; the fixture keeps that pairing so
        // loading must migrate both sides coherently.
        tx.execute(
            "UPDATE execution_protocol_work_items
             SET payload_json = ?1, source_revision = 2, generation = 1,
                 lifecycle_state = 'paused'
             WHERE agent_id = 'agent-a' AND work_item_id = 'work-a'",
            params![legacy.to_string()],
        )?;
        let mut attempt_payload: serde_json::Value = serde_json::from_str(&tx.query_row(
            "SELECT payload_json FROM execution_protocol_attempts
                 WHERE agent_id = 'agent-a' AND attempt_id = ?1",
            params![attempt.attempt_id],
            |row| row.get::<_, String>(0),
        )?)?;
        attempt_payload["state"] = serde_json::json!("settled");
        attempt_payload["terminal_outcome_id"] = serde_json::json!("outcome-legacy-repair");
        attempt_payload["terminal_at"] = serde_json::json!("2026-11-17T00:00:01Z");
        tx.execute(
            "UPDATE execution_protocol_attempts SET payload_json = ?1
             WHERE agent_id = 'agent-a' AND attempt_id = ?2",
            params![attempt_payload.to_string(), attempt.attempt_id],
        )?;
        tx.execute(
            "INSERT INTO execution_protocol_outcomes
             (agent_id, outcome_id, attempt_id, payload_json)
             VALUES ('agent-a', 'outcome-legacy-repair', ?1, ?2)",
            params![
                attempt.attempt_id,
                serde_json::json!({
                    "outcome_id": "outcome-legacy-repair",
                    "attempt_id": attempt.attempt_id,
                    "outcome": {
                        "owner": "work_item",
                        "outcome": {
                            "kind": "needs_repair",
                            "repair_id": "provider_dispatch_rejected:turn-legacy"
                        }
                    },
                    "created_at": "2026-11-17T00:00:01Z"
                })
                .to_string()
            ],
        )?;
        Ok(())
    })?;
    strand(&db, 2)?;
    let protocol = db
        .transitions()
        .load_execution_protocol_state_if_initialized("agent-a")?
        .expect("protocol must stay initialized");
    assert_eq!(
        protocol.work_items["work-a"].state,
        WorkItemExecutionState::Runnable {
            generation: 1,
            recovery_ref: None,
        },
        "an aligned durable revision means the fence is gone: Runnable again"
    );
    assert_eq!(
        protocol.outcomes["outcome-legacy-repair"].outcome,
        crate::domain::execution_protocol::ExecutionOutcome::WorkItem(
            crate::domain::execution_protocol::WorkItemOutcome::Interrupted {
                reason: "provider_dispatch_rejected:turn-legacy".into()
            }
        ),
        "the paired legacy outcome migrates to interrupted with its repair reason"
    );
    strand(&db, 3)?;
    let protocol = db
        .transitions()
        .load_execution_protocol_state_if_initialized("agent-a")?
        .expect("protocol must stay initialized");
    assert_eq!(
        protocol.work_items["work-a"].state,
        WorkItemExecutionState::Paused {
            generation: 1,
            reason: "work_item_waits_ambiguous:work-a".into(),
        },
        "unaligned revisions pause with the original repair reason"
    );
    Ok(())
}
