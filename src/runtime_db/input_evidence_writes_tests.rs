use super::*;
use crate::{
    runtime::resident_input_evidence::{InputEvidenceFamily as Family, InputEvidenceKey as Key},
    runtime_db::{
        evidence::{upsert_agent_identity_tx, upsert_agent_state_tx},
        input_evidence_snapshot::{capture_input_evidence_upgrade_tx, INPUT_EVIDENCE_SCHEMA},
        DestructiveOperationPhase, DestructiveOperationRecord, RuntimeDb, RuntimeDbRetentionPolicy,
    },
    types::{
        AgentKind, AgentOwnership, AgentProfilePreset, AgentState, AgentVisibility, AuthorityClass,
        MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority, TranscriptEntry,
        TranscriptEntryKind, TurnRecord,
    },
};
use chrono::{Duration, Utc};

struct Fixture {
    _dir: tempfile::TempDir,
    db: RuntimeDb,
    scope: InputEvidenceScope,
}
impl Fixture {
    fn new() -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            dir.path().join("runtime.sqlite"),
            dir.path().join("runtime.lock"),
        )?;
        let scope = InputEvidenceScope {
            agent_id: "agent-a".into(),
            identity_incarnation: 1,
        };
        db.transaction(|tx| {
            tx.execute_batch(INPUT_EVIDENCE_SCHEMA)?;
            capture_input_evidence_upgrade_tx(tx, Utc::now())?;
            upsert_agent_identity_tx(
                tx,
                &crate::types::AgentIdentityRecord::new(
                    "agent-a",
                    AgentKind::Named,
                    AgentVisibility::Public,
                    AgentOwnership::SelfOwned,
                    AgentProfilePreset::PublicNamed,
                    None,
                    None,
                ),
            )?;
            upsert_agent_state_tx(tx, &AgentState::new("agent-a"))
        })?;
        Ok(Self {
            _dir: dir,
            db,
            scope,
        })
    }
    fn message(&self, id: &str) -> MessageEnvelope {
        let mut m = MessageEnvelope::new(
            "agent-a",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text { text: id.into() },
        );
        m.id = id.into();
        m
    }
    fn aliases(&self) -> Result<i64> {
        Ok(self.db.connection()?.query_row(
            "SELECT COUNT(*) FROM input_evidence_current",
            [],
            |r| r.get(0),
        )?)
    }
}
#[test]
fn scoped_message_ack_uses_actual_sequence_and_replay_row() -> Result<()> {
    let f = Fixture::new()?;
    let m = f.message("message-a");
    let first =
        f.db.messages()
            .append_with_index_changes_in_scope(&f.scope, &m, &[])?;
    assert!(first.record.message_seq.unwrap_or_default() > 0);
    let second =
        f.db.messages()
            .append_with_index_changes_in_scope(&f.scope, &m, &[])?;
    assert_eq!(first.record, second.record);
    assert_eq!(
        first.input_delta.records[0].locator,
        second.input_delta.records[0].locator
    );
    assert_eq!(f.aliases()?, 1);
    Ok(())
}
#[test]
fn wrong_original_incarnation_rejects_before_any_evidence_write() -> Result<()> {
    let f = Fixture::new()?;
    let mut wrong = f.scope.clone();
    wrong.identity_incarnation = 2;
    assert!(f
        .db
        .messages()
        .upsert_in_scope(&wrong, &f.message("message-a"))
        .is_err());
    assert!(f
        .db
        .messages()
        .by_id(Some("agent-a"), "message-a")?
        .is_none());
    assert_eq!(f.aliases()?, 0);
    Ok(())
}
#[test]
fn qualification_failure_rolls_back_canonical_sequence_and_alias() -> Result<()> {
    let f = Fixture::new()?;
    f.db.connection()?.execute_batch("CREATE TRIGGER reject_alias BEFORE INSERT ON input_evidence_current BEGIN SELECT RAISE(ABORT,'fixture alias failure'); END;")?;
    assert!(f
        .db
        .messages()
        .append_with_index_changes_in_scope(&f.scope, &f.message("message-a"), &[])
        .is_err());
    assert!(f
        .db
        .messages()
        .by_id(Some("agent-a"), "message-a")?
        .is_none());
    assert_eq!(f.aliases()?, 0);
    f.db.connection()?
        .execute_batch("DROP TRIGGER reject_alias")?;
    let ack = f.db.messages().append_with_index_changes_in_scope(
        &f.scope,
        &f.message("message-b"),
        &[],
    )?;
    assert_eq!(ack.record.message_seq, Some(1));
    Ok(())
}
#[test]
fn compound_scope_mismatch_cannot_write_another_agent() -> Result<()> {
    let f = Fixture::new()?;
    let command = crate::runtime_db::transitions::AgentPostureTransitionCommand {
        agent_id: "agent-b".into(),
        expected_incarnation: None,
        agent_state: crate::runtime_db::transitions::AgentStateMutation {
            expected: None,
            record: Box::new(AgentState::new("agent-b")),
        },
        audit_events: vec![],
        fault: None,
    };
    assert!(f
        .db
        .transitions_in_scope(&f.scope)
        .commit_agent_posture(&command)
        .is_err());
    assert!(f.db.agent_states().latest("agent-b")?.is_none());
    Ok(())
}
#[test]
fn original_destructive_operation_tx_qualifies_only_fresh_turn_write() -> Result<()> {
    let f = Fixture::new()?;
    let turn = TurnRecord::new("agent-a", "turn-a", 1);
    f.db.turn_records().upsert_in_scope(&f.scope, &turn)?;
    let now = Utc::now();
    let command = DestructiveOperationRecord {
        operation_id: "operation-a".into(),
        owner_turn_id: "turn-a".into(),
        owner_work_item_id: None,
        command_digest: "digest-a".into(),
        phase: DestructiveOperationPhase::Planned,
        verification_target: "daemon".into(),
        recovery_policy: "verify_only".into(),
        created_at: now,
        updated_at: now,
    };
    let first =
        f.db.destructive_operations()
            .plan_and_schedule_in_scope(&f.scope, &command)?;
    assert!(first.record.should_dispatch);
    assert!(first
        .input_delta
        .records
        .iter()
        .any(|r| r.locator.key == Key::new(Family::Turn, "turn-a") && r.locator.revision == 2));
    let replay =
        f.db.destructive_operations()
            .plan_and_schedule_in_scope(&f.scope, &command)?;
    assert!(!replay.record.should_dispatch);
    assert!(replay.input_delta.records.is_empty());
    Ok(())
}
fn retention_policy() -> RuntimeDbRetentionPolicy {
    RuntimeDbRetentionPolicy {
        enabled: true,
        transcript_entries_days: 1,
        transcript_entries_min_rows: 1,
        ..RuntimeDbRetentionPolicy::default()
    }
}
fn seed_retention(f: &Fixture) -> Result<()> {
    for (id, age) in [("old", 3), ("new", 0)] {
        let mut entry = TranscriptEntry::new(
            "agent-a",
            TranscriptEntryKind::IncomingMessage,
            None,
            None,
            serde_json::json!({}),
        );
        entry.id = id.into();
        entry.created_at = Utc::now() - Duration::days(age);
        f.db.transcript_entries()
            .append_in_scope(&f.scope, &entry)?;
    }
    Ok(())
}
#[test]
fn retention_same_tx_prunes_only_actual_deleted_current_alias() -> Result<()> {
    let f = Fixture::new()?;
    seed_retention(&f)?;
    let scopes =
        f.db.retention_input_scopes(retention_policy(), Utc::now())?;
    let ack =
        f.db.commit_retention_pass_in_scopes(retention_policy(), Utc::now(), &scopes)?;
    assert_eq!(ack.committed.removed_transcript_entries.len(), 1);
    assert_eq!(ack.input_deltas.len(), 1);
    assert_eq!(
        ack.input_deltas[0].1.removals[0].key,
        Key::new(Family::Transcript, "old")
    );
    let availability:String=f.db.connection()?.query_row("SELECT availability FROM input_evidence_current WHERE family='transcript' AND record_id='old'",[],|r|r.get(0))?;
    assert_eq!(availability, "pruned");
    Ok(())
}
#[test]
fn retention_missing_reservation_rolls_back_all_deletes() -> Result<()> {
    let f = Fixture::new()?;
    seed_retention(&f)?;
    assert!(f
        .db
        .commit_retention_pass_in_scopes(retention_policy(), Utc::now(), &[])
        .is_err());
    let n: i64 =
        f.db.connection()?
            .query_row("SELECT COUNT(*) FROM transcript_entries", [], |r| r.get(0))?;
    assert_eq!(n, 2);
    Ok(())
}
#[test]
fn retention_ignored_delete_does_not_publish_pruned_delta() -> Result<()> {
    let f = Fixture::new()?;
    seed_retention(&f)?;
    f.db.connection()?.execute_batch("CREATE TRIGGER keep_old BEFORE DELETE ON transcript_entries WHEN OLD.evidence_id='old' BEGIN SELECT RAISE(IGNORE); END;")?;
    let ack = f.db.commit_retention_pass_in_scopes(
        retention_policy(),
        Utc::now(),
        &f.db
            .retention_input_scopes(retention_policy(), Utc::now())?,
    )?;
    assert!(ack.committed.removed_transcript_entries.is_empty());
    assert!(ack.input_deltas.is_empty());
    let present:String=f.db.connection()?.query_row("SELECT availability FROM input_evidence_current WHERE family='transcript' AND record_id='old'",[],|r|r.get(0))?;
    assert_eq!(present, "present");
    Ok(())
}

fn retention_identity_status(f: &Fixture, status: AgentRegistryStatus) -> Result<()> {
    let mut identity = f.db.agent_identities().latest(&f.scope.agent_id)?.unwrap();
    identity.status = status;
    identity.updated_at += Duration::milliseconds(1);
    if status == AgentRegistryStatus::Deleted {
        identity.deleted_at = Some(identity.updated_at);
    }
    f.db.agent_identities().upsert(&identity)
}

#[test]
fn retention_protects_deleting_evidence_without_granting_or_pruning_qualification() -> Result<()> {
    let f = Fixture::new()?;
    seed_retention(&f)?;
    let now = Utc::now();
    for (id, age) in [("old-tool", 3), ("new-tool", 0)] {
        let tool = crate::types::ToolExecutionRecord {
            id: id.into(),
            agent_id: f.scope.agent_id.clone(),
            work_item_id: None,
            turn_index: 0,
            turn_id: None,
            tool_name: "fixture".into(),
            created_at: now - Duration::days(age),
            completed_at: Some(now - Duration::days(age)),
            duration_ms: 1,
            authority_class: AuthorityClass::RuntimeInstruction,
            status: crate::types::ToolExecutionStatus::Success,
            input: serde_json::json!({}),
            output: serde_json::json!({}),
            summary: id.into(),
            invocation_surface: None,
        };
        f.db.evidence()
            .append_tool_execution_in_scope(&f.scope, &tool, &[])?;
    }
    // Unqualified historical evidence is protected too, but never adopted.
    let mut legacy = TranscriptEntry::new(
        &f.scope.agent_id,
        TranscriptEntryKind::IncomingMessage,
        None,
        None,
        serde_json::json!({}),
    );
    legacy.id = "legacy-unqualified".into();
    legacy.created_at = now - Duration::days(3);
    f.db.transcript_entries().upsert(&legacy)?;
    let aliases = f.aliases()?;
    assert_eq!(aliases, 4);
    retention_identity_status(&f, AgentRegistryStatus::Deleting)?;
    let policy = RuntimeDbRetentionPolicy {
        tool_executions_days: 1,
        tool_executions_min_rows: 1,
        ..retention_policy()
    };

    let dry = f.db.plan_retention(policy.clone(), now)?;
    assert_eq!(
        dry.transcript_entries
            .protection_reasons
            .get("identity_deleting"),
        Some(&2)
    );
    assert_eq!(
        dry.tool_executions
            .protection_reasons
            .get("identity_deleting"),
        Some(&1)
    );
    assert_eq!(dry.transcript_entries.planned_delete_rows, 0);
    assert_eq!(dry.tool_executions.planned_delete_rows, 0);
    assert!(f.db.retention_input_scopes(policy.clone(), now)?.is_empty());
    let scoped =
        f.db.commit_retention_pass_in_scopes(policy.clone(), now, &[])?;
    assert!(scoped.input_deltas.is_empty());
    assert!(scoped.committed.removed_transcript_entries.is_empty());
    assert!(scoped.committed.removed_tool_executions.is_empty());
    assert_eq!(
        scoped
            .committed
            .report
            .transcript_entries
            .protection_reasons
            .get("identity_deleting"),
        Some(&2)
    );
    // The unscoped/offline wrapper uses the same protected candidate planner.
    let unscoped = f.db.commit_retention_pass(policy.clone(), now)?;
    assert!(unscoped.removed_transcript_entries.is_empty());
    assert!(unscoped.removed_tool_executions.is_empty());
    assert_eq!(f.aliases()?, aliases);
    let present: i64 = f.db.connection()?.query_row(
        "SELECT COUNT(*) FROM input_evidence_current WHERE availability='present'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(present, aliases);
    assert!(f.db.transcript_entries().by_id(None, &legacy.id)?.is_some());

    // Retirement completion restores the original historical retention policy.
    retention_identity_status(&f, AgentRegistryStatus::Deleted)?;
    let retired = f.db.commit_retention_pass_in_scopes(policy, now, &[])?;
    assert_eq!(retired.committed.removed_transcript_entries.len(), 2);
    assert_eq!(retired.committed.removed_tool_executions.len(), 1);
    assert!(
        retired.input_deltas.is_empty(),
        "retired aliases are never installed as current facts"
    );
    assert!(f.db.transcript_entries().by_id(None, "old")?.is_none());
    assert!(f.db.transcript_entries().by_id(None, &legacy.id)?.is_none());
    assert!(f.db.transcript_entries().by_id(None, "new")?.is_some());
    assert_eq!(f.aliases()?, aliases);
    Ok(())
}

#[test]
fn retention_revalidates_identity_projection_and_original_incarnation_before_deletion() -> Result<()>
{
    for corruption in ["status", "agent", "incarnation"] {
        let f = Fixture::new()?;
        seed_retention(&f)?;
        let mut identity = f.db.agent_identities().latest(&f.scope.agent_id)?.unwrap();
        match corruption {
            "status" => identity.status = AgentRegistryStatus::Deleting,
            "agent" => identity.agent_id = "foreign-agent".into(),
            "incarnation" => identity.incarnation = 0,
            _ => unreachable!(),
        }
        f.db.connection()?.execute(
            "UPDATE agent_identities SET payload_json=?1 WHERE agent_id=?2",
            rusqlite::params![serde_json::to_string(&identity)?, f.scope.agent_id],
        )?;
        let error =
            f.db.commit_retention_pass_in_scopes(retention_policy(), Utc::now(), &[])
                .expect_err("mismatched normalized/payload identity cannot authorize deletion");
        assert!(error
            .to_string()
            .contains("retention identity projection mismatch"));
        assert!(f.db.transcript_entries().by_id(None, "old")?.is_some());
        assert_eq!(f.aliases()?, 2);
    }

    let f = Fixture::new()?;
    seed_retention(&f)?;
    let original =
        f.db.retention_input_scopes(retention_policy(), Utc::now())?;
    assert_eq!(original, vec![f.scope.clone()]);
    retention_identity_status(&f, AgentRegistryStatus::Deleting)?;
    let error =
        f.db.commit_retention_pass_in_scopes(retention_policy(), Utc::now(), &original)
            .expect_err("actual transaction must replan a changed identity status");
    assert!(error
        .to_string()
        .contains("retention reservation inventory changed before SQL"));
    assert!(f.db.transcript_entries().by_id(None, "old")?.is_some());
    assert_eq!(f.aliases()?, 2);

    let f = Fixture::new()?;
    seed_retention(&f)?;
    let mut wrong = f.scope.clone();
    wrong.identity_incarnation += 1;
    let error =
        f.db.commit_retention_pass_in_scopes(retention_policy(), Utc::now(), &[wrong])
            .expect_err("reservation of a successor incarnation cannot prune the original scope");
    assert!(error
        .to_string()
        .contains("retention reservation inventory changed before SQL"));
    assert!(f.db.transcript_entries().by_id(None, "old")?.is_some());
    assert_eq!(f.aliases()?, 2);
    Ok(())
}

#[test]
fn fresh_scope_cannot_qualify_old_append_or_equal_upsert_replays() -> Result<()> {
    let f = Fixture::new()?;
    let message = f.message("old-message");
    let mut transcript = TranscriptEntry::new(
        "agent-a",
        TranscriptEntryKind::IncomingMessage,
        None,
        None,
        serde_json::json!({}),
    );
    transcript.id = "old-transcript".into();
    let turn = TurnRecord::new("agent-a", "old-turn", 1);
    // Unscoped fixtures model history left by an old incarnation, not a current
    // scoped acknowledgement. FreshEmpty must not adopt any of these rows.
    f.db.messages().upsert(&message)?;
    f.db.transcript_entries().upsert(&transcript)?;
    f.db.turn_records().upsert(&turn)?;
    assert!(f
        .db
        .messages()
        .append_with_index_changes_in_scope(&f.scope, &message, &[])
        .is_err());
    assert!(f.db.messages().upsert_in_scope(&f.scope, &message).is_err());
    assert!(f
        .db
        .evidence()
        .append_message_in_scope(&f.scope, &message)
        .is_err());
    assert!(f
        .db
        .transcript_entries()
        .append_in_scope(&f.scope, &transcript)
        .is_err());
    assert!(f
        .db
        .transcript_entries()
        .upsert_in_scope(&f.scope, &transcript)
        .is_err());
    assert!(f
        .db
        .evidence()
        .append_transcript_entry_in_scope(&f.scope, &transcript)
        .is_err());
    assert!(f
        .db
        .turn_records()
        .upsert_in_scope(&f.scope, &turn)
        .is_err());
    assert_eq!(f.aliases()?, 0);
    Ok(())
}

#[test]
fn scoped_transcript_append_retry_keeps_original_qualification() -> Result<()> {
    let f = Fixture::new()?;
    let mut transcript = TranscriptEntry::new(
        "agent-a",
        TranscriptEntryKind::IncomingMessage,
        None,
        None,
        serde_json::json!({}),
    );
    transcript.id = "current-transcript".into();
    let first =
        f.db.transcript_entries()
            .append_in_scope(&f.scope, &transcript)?;
    let replay =
        f.db.transcript_entries()
            .append_in_scope(&f.scope, &transcript)?;
    assert_eq!(first.record, replay.record);
    assert_eq!(
        first.input_delta.records[0].locator,
        replay.input_delta.records[0].locator
    );
    assert_eq!(f.aliases()?, 1);
    Ok(())
}

#[test]
fn reincarnated_scope_cannot_reuse_original_scope_message_ack() -> Result<()> {
    let f = Fixture::new()?;
    let original =
        f.db.messages()
            .append_with_index_changes_in_scope(&f.scope, &f.message("same-id"), &[])?;
    let mut successor = f.db.agent_identities().latest("agent-a")?.unwrap();
    successor.incarnation = 2;
    successor.updated_at = Utc::now() + Duration::seconds(1);
    f.db.transaction(|tx| upsert_agent_identity_tx(tx, &successor))?;
    let next = InputEvidenceScope {
        agent_id: "agent-a".into(),
        identity_incarnation: 2,
    };
    assert!(f
        .db
        .messages()
        .append_with_index_changes_in_scope(&next, &original.record, &[])
        .is_err());
    let n: i64 = f.db.connection()?.query_row(
        "SELECT COUNT(*) FROM input_evidence_current WHERE identity_incarnation=2",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(n, 0);
    assert_eq!(
        f.db.messages().by_id(Some("agent-a"), "same-id")?,
        Some(original.record)
    );
    Ok(())
}

#[test]
fn fresh_scope_cannot_requalify_equal_tool_or_brief() -> Result<()> {
    let f = Fixture::new()?;
    let tool = crate::types::ToolExecutionRecord {
        id: "old-tool".into(),
        agent_id: "agent-a".into(),
        work_item_id: None,
        turn_index: 0,
        turn_id: None,
        tool_name: "test".into(),
        created_at: Utc::now(),
        completed_at: None,
        duration_ms: 0,
        authority_class: AuthorityClass::RuntimeInstruction,
        status: crate::types::ToolExecutionStatus::Success,
        input: serde_json::json!({}),
        output: serde_json::json!({}),
        summary: "test".into(),
        invocation_surface: None,
    };
    let brief = crate::types::BriefRecord::new(
        "agent-a",
        crate::types::BriefKind::Result,
        "test",
        None,
        None,
    );
    f.db.transaction(|tx| {
        crate::runtime_db::evidence::insert_tool_evidence_tx(tx, &tool)?;
        crate::runtime_db::evidence::insert_brief_evidence_tx(tx, &brief)
    })?;
    assert!(f
        .db
        .evidence()
        .append_tool_execution_in_scope(&f.scope, &tool, &[])
        .is_err());
    assert!(f
        .db
        .evidence()
        .append_brief_in_scope(&f.scope, &brief, &[])
        .is_err());
    assert!(f
        .db
        .evidence()
        .append_brief_with_created_event_in_scope(
            &f.scope,
            Some("agent-a"),
            &brief,
            &crate::types::brief_created_event_for(&brief)?,
            &[]
        )
        .is_err());
    assert_eq!(f.aliases()?, 0);
    Ok(())
}

#[test]
fn started_posture_turn_and_assignment_commit_together_and_roll_back_together() -> Result<()> {
    use crate::runtime_db::transitions::{
        AgentPostureTransitionCommand, AgentStateMutation, TransitionFaultPoint,
    };
    let f = Fixture::new()?;
    let message = f.message("started-input");
    f.db.messages().upsert_in_scope(&f.scope, &message)?;
    let previous = f.db.agent_states().latest("agent-a")?.unwrap();
    let mut next = previous.clone();
    next.turn_index = 1;
    next.current_turn_id = Some("started-turn".into());
    let mut turn = TurnRecord::new("agent-a", "started-turn", 1);
    turn.input_message_ids.push(message.id.clone());
    let mut command = AgentPostureTransitionCommand {
        agent_id: "agent-a".into(),
        expected_incarnation: Some(1),
        agent_state: AgentStateMutation {
            expected: Some(Box::new(previous.clone())),
            record: Box::new(next.clone()),
        },
        audit_events: Vec::new(),
        fault: Some(TransitionFaultPoint::AfterCanonicalWrites),
    };
    assert!(f
        .db
        .transitions_in_scope(&f.scope)
        .commit_agent_posture_with_started_turn(&command, Some(&turn))
        .is_err());
    assert_eq!(f.db.agent_states().latest("agent-a")?, Some(previous));
    assert!(f
        .db
        .turn_records()
        .by_id(Some("agent-a"), "started-turn")?
        .is_none());
    assert_eq!(
        f.db.connection()?.query_row(
            "SELECT COUNT(*) FROM conversation_input_assignments WHERE message_id=?1",
            [&message.id],
            |row| row.get::<_, i64>(0)
        )?,
        0
    );
    command.fault = None;
    let committed =
        f.db.transitions_in_scope(&f.scope)
            .commit_agent_posture_with_started_turn(&command, Some(&turn))?;
    assert!(committed.applied);
    assert_eq!(f.db.agent_states().latest("agent-a")?, Some(next));
    assert_eq!(committed.effects.turns, vec![turn]);
    assert_eq!(
        committed.effects.input_evidence_delta.message_turn_keys[&message.id],
        Some("started-turn".into())
    );
    assert!(committed
        .effects
        .input_evidence_delta
        .records
        .iter()
        .any(|record| record.locator.key == Key::new(Family::Turn, "started-turn")));
    Ok(())
}

fn timer_fire_fixture(f: &Fixture, message_id: &str) -> Result<crate::runtime_db::TimerFire> {
    use crate::types::{AdmissionContext, MessageDeliverySurface, TimerRecord, TimerStatus};
    let now = Utc::now();
    let timer = TimerRecord {
        id: format!("timer-{message_id}"),
        agent_id: f.scope.agent_id.clone(),
        created_at: now,
        duration_ms: 1,
        interval_ms: None,
        repeat: false,
        status: TimerStatus::Active,
        summary: None,
        next_fire_at: Some(now),
        last_fired_at: None,
        fire_count: 0,
    };
    f.db.timers().upsert(&timer)?;
    let mut fired = timer.clone();
    fired.status = TimerStatus::Completed;
    fired.fire_count = 1;
    fired.last_fired_at = Some(now);
    fired.next_fire_at = None;
    let mut message = MessageEnvelope::new(
        &f.scope.agent_id,
        MessageKind::TimerTick,
        MessageOrigin::Timer {
            timer_id: timer.id.clone(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "timer fired".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::TimerScheduler,
        AdmissionContext::RuntimeOwned,
    );
    message.id = message_id.into();
    message.created_at = now;
    message.turn_id = Some(format!("unstarted-{message_id}"));
    message.metadata = Some(serde_json::json!({"timer_id": timer.id, "timer_fire_count": 1}));
    message
        .source_refs
        .insert("timer_id".into(), timer.id.clone());
    message.normalize_admission_fields();
    let before = f.db.agent_states().latest(&f.scope.agent_id)?.unwrap();
    let mut after = before.clone();
    after.pending += 1;
    // The ACK must use the count actually allocated by the original SQL transaction.
    after.total_message_count = 999;
    Ok(crate::runtime_db::TimerFire {
        expected: timer,
        record: fired,
        queue_entry: crate::types::QueueEntryRecord {
            message_id: message.id.clone(),
            agent_id: message.agent_id.clone(),
            priority: message.priority.clone(),
            status: crate::types::QueueEntryStatus::Queued,
            created_at: now,
            updated_at: now,
        },
        message,
        agent_state: (before, after),
    })
}

#[test]
fn scoped_timer_fire_returns_qualified_actual_wake_and_rolls_back_the_whole_transaction(
) -> Result<()> {
    use crate::runtime_db::{
        input_evidence_snapshot::{
            load_input_evidence_snapshot_tx, InputEvidenceBootstrapSnapshot,
            InputEvidenceLoadRequest,
        },
        transitions::TransitionFaultPoint,
    };
    let f = Fixture::new()?;
    let command = timer_fire_fixture(&f, "scoped-timer-wake")?;
    assert!(f
        .db
        .timers()
        .fire_with_wait_in_scope(
            &f.scope,
            &command,
            &timer_work_tests::work_request(),
            &timer_work_tests::budget_ledger()?,
            Some(TransitionFaultPoint::AfterCanonicalWrites)
        )
        .is_err());
    assert_eq!(
        f.db.timers().latest(&command.expected.id)?,
        Some(command.expected.clone())
    );
    assert!(f
        .db
        .messages()
        .by_id(Some(&f.scope.agent_id), &command.message.id)?
        .is_none());
    assert!(f.db.queue_entries().latest(&command.message.id)?.is_none());
    assert!(f.db.timers().pending_wake(&command.expected.id)?.is_none());
    assert_eq!(
        f.db.agent_states().latest(&f.scope.agent_id)?,
        Some(command.agent_state.0.clone())
    );
    assert_eq!(f.aliases()?, 0);
    let acknowledgement = f.db.timers().fire_with_wait_in_scope(
        &f.scope,
        &command,
        &timer_work_tests::work_request(),
        &timer_work_tests::budget_ledger()?,
        None,
    )?;
    assert!(acknowledgement.work_delta.is_none());
    let actual = acknowledgement.record.message.as_ref().unwrap();
    assert!(actual.message_seq.unwrap_or_default() > 0);
    assert_ne!(actual.message_seq, command.message.message_seq);
    assert_eq!(acknowledgement.input_delta.records.len(), 1);
    assert_eq!(
        acknowledgement.input_delta.records[0].locator.key,
        Key::new(Family::Message, &actual.id)
    );
    assert_eq!(
        acknowledgement.input_delta.message_turn_keys[&actual.id],
        None
    );
    let state = acknowledgement.agent_state.as_ref().unwrap();
    assert_eq!(state.expected.as_deref(), Some(&command.agent_state.0));
    assert_eq!(state.record.total_message_count, 1);
    assert_eq!(state.record.pending, 1);
    assert_eq!(
        f.db.agent_states().latest(&f.scope.agent_id)?.as_ref(),
        Some(state.record.as_ref())
    );
    let request = InputEvidenceLoadRequest {
        windows: Vec::new(),
        mandatory_roots: [Key::new(Family::Message, &actual.id)].into(),
        selected_completion_brief_ids: Default::default(),
        interrupted_sources: [actual.id.clone()].into(),
        record_limit: 128,
        byte_limit: 1024 * 1024,
    };
    let snapshot =
        f.db.transaction(|tx| load_input_evidence_snapshot_tx(tx, &f.scope, &request))?;
    let InputEvidenceBootstrapSnapshot::Current(snapshot) = snapshot else {
        panic!("scope retired")
    };
    assert!(snapshot
        .resident
        .capture()?
        .lookup(&Key::new(Family::Message, &actual.id))?
        .is_some());
    let replay = f.db.timers().fire_with_wait_in_scope(
        &f.scope,
        &command,
        &timer_work_tests::work_request(),
        &timer_work_tests::budget_ledger()?,
        None,
    )?;
    assert!(!replay.record.advanced && !replay.record.wake_created);
    assert!(replay.input_delta.records.is_empty());
    assert_eq!(f.aliases()?, 1);
    assert_eq!(
        f.db.timers()
            .latest(&command.expected.id)?
            .unwrap()
            .fire_count,
        1
    );
    Ok(())
}

#[test]
fn scoped_timer_fire_does_not_adopt_an_unqualified_same_name_message() -> Result<()> {
    let mut f = Fixture::new()?;
    let mut command = timer_fire_fixture(&f, "prior-incarnation-timer-wake")?;
    // A preexisting same-name body is not a canonical insertion by this fire.
    f.db.messages().upsert_committed(&command.message)?;
    let mut successor = f.db.agent_identities().latest(&f.scope.agent_id)?.unwrap();
    successor.incarnation = 2;
    f.db.transaction(|tx| upsert_agent_identity_tx(tx, &successor))?;
    f.scope.identity_incarnation = 2;
    command.agent_state.0 = f.db.agent_states().latest(&f.scope.agent_id)?.unwrap();
    command.agent_state.1 = command.agent_state.0.clone();
    command.agent_state.1.pending = 1;
    command.agent_state.1.total_message_count = 1;
    assert!(f
        .db
        .timers()
        .fire_with_wait_in_scope(
            &f.scope,
            &command,
            &timer_work_tests::work_request(),
            &timer_work_tests::budget_ledger()?,
            None
        )
        .is_err());
    assert_eq!(
        f.db.timers().latest(&command.expected.id)?,
        Some(command.expected.clone())
    );
    assert!(f.db.timers().pending_wake(&command.expected.id)?.is_none());
    assert!(f.db.queue_entries().latest(&command.message.id)?.is_none());
    assert_eq!(
        f.db.agent_states().latest(&f.scope.agent_id)?,
        Some(command.agent_state.0)
    );
    assert_eq!(f.aliases()?, 0);
    Ok(())
}

#[path = "timer_wait_work_tests.rs"]
mod timer_work_tests;
