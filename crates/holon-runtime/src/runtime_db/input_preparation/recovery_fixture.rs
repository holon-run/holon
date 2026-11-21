//! A normally migrated historical completion with an untracked provider source.
use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Connection, Transaction};

use crate::{
    runtime_db::{
        evidence::{append_message_tx, upsert_agent_identity_tx, upsert_agent_state_tx},
        input_evidence_snapshot::capture_input_evidence_upgrade_tx,
        migrations::{apply_migration, ensure_migration_table, MIGRATIONS},
        repositories::upsert_turn_record_tx,
        RuntimeDb,
    },
    types::{
        AdmissionContext, AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset,
        AgentState, AgentVisibility, AuthorityClass, CompletionReportRequirement,
        CompletionReportState, MessageBody, MessageDeliverySurface, MessageEnvelope, MessageKind,
        MessageOrigin, Priority, TurnRecord, TurnTerminalKind, TurnTerminalSummary,
        TurnTriggerSummary, WorkItemCompletionIntent, WorkItemRecord, WorkItemState,
    },
};

pub(crate) const SOURCE_ID: &str = "historical-provider-source";
pub(crate) const RECOVERY_ID: &str = "historical-provider-recovery";
pub(crate) const WORK_ID: &str = "historical-completed-work";

/// Capture old data through the real upgrade, without deleting qualifications
/// or synthesizing a failed receipt. The old source is history, never live.
pub(crate) fn legacy_completed_work_fixture(
    history_count: usize,
) -> Result<(tempfile::TempDir, RuntimeDb)> {
    assert!(history_count > 0);
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("runtime.sqlite");
    let at: DateTime<Utc> = "2026-10-09T08:00:00Z".parse()?;
    write_legacy(&path, at, history_count)?;
    let db = RuntimeDb::open_and_migrate(&path, directory.path().join("runtime.lock"))?;
    db.transaction(|tx| capture_input_evidence_upgrade_tx(tx, at))?;
    Ok((directory, db))
}

fn write_legacy(path: &Path, at: DateTime<Utc>, history_count: usize) -> Result<()> {
    let mut connection = Connection::open(path)?;
    ensure_migration_table(&connection)?;
    for migration in MIGRATIONS
        .iter()
        .filter(|migration| migration.version <= 89)
    {
        apply_migration(&mut connection, migration)?;
    }
    let tx = connection.transaction()?;
    upsert_agent_identity_tx(
        &tx,
        &AgentIdentityRecord::new(
            "default",
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        ),
    )?;
    upsert_agent_state_tx(&tx, &AgentState::new("default"))?;
    tx.execute(
        "INSERT INTO storage_domains(domain,schema_version,import_status,canonical_source,updated_at)
         VALUES('context_episode_anchors',1,'complete','db',?1)",
        [at.to_rfc3339()],
    )?;
    let mut source = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "historical prompt".into(),
        },
    );
    source.id = SOURCE_ID.into();
    source.turn_id = Some("historical-provider-turn".into());
    source.created_at = at;
    let source = append_message_tx(&tx, &source)?.0;
    let mut turn = TurnRecord::new("default", "historical-provider-turn", 1);
    turn.created_at = at;
    turn.trigger = Some(TurnTriggerSummary::from_message(&source));
    turn.input_message_ids = vec![source.id.clone()];
    turn.terminal = Some(TurnTerminalSummary {
        kind: TurnTerminalKind::DeferredToFallback,
        reason: None,
        no_brief_reason: None,
        completed_at: at,
        duration_ms: 1,
    });
    upsert_turn_record_tx(&tx, &turn)?;
    let mut recovery = MessageEnvelope::new(
        "default",
        MessageKind::InternalFollowup,
        MessageOrigin::System {
            subsystem: "model_lineage_recovery".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "historical recovery".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::RuntimeSystem,
        AdmissionContext::RuntimeOwned,
    );
    recovery.id = RECOVERY_ID.into();
    recovery.created_at = at;
    recovery.work_item_id = Some(WORK_ID.into());
    recovery.causation_id = Some(source.id.clone());
    recovery
        .source_refs
        .insert("source_message_id".into(), source.id.clone());
    recovery
        .source_refs
        .insert("source_turn_id".into(), turn.turn_id.clone());
    recovery.metadata = Some(serde_json::json!({"provider_recovery": {
        "fallback_model_ref": "anthropic/claude-sonnet-4-6",
        "source_message_id": source.id,
        "source_turn_id": turn.turn_id,
        "source_terminal_kind": "deferred_to_fallback", "source_round": 1
    }}));
    append_message_tx(&tx, &recovery)?;
    for index in 0..history_count {
        let mut work = WorkItemRecord::new("default", "historical work", WorkItemState::Completed);
        work.id = if index == 0 {
            WORK_ID.into()
        } else {
            format!("historical-work-{index:03}")
        };
        work.created_at = at;
        work.updated_at = at + Duration::seconds(i64::try_from(index)?);
        if index == 0 {
            work.completion_intent = Some(WorkItemCompletionIntent {
                work_item_id: work.id.clone(),
                source_activation_id: None,
                source_message_id: Some(recovery.id.clone()),
                source_turn_id: None,
                expected_work_revision: work.revision,
                report_requirement: CompletionReportRequirement::Required,
                report_state: CompletionReportState::Missing,
                result_brief_id: None,
                created_at: at,
                updated_at: at,
            });
        }
        insert_legacy_work(&tx, &work)?;
    }
    tx.commit()?;
    Ok(())
}

fn insert_legacy_work(tx: &Transaction<'_>, work: &WorkItemRecord) -> Result<()> {
    tx.execute(
        "INSERT INTO work_items(work_item_id,agent_id,state,objective,revision,created_at,updated_at,payload_json)
         VALUES(?1,?2,'completed',?3,?4,?5,?6,?7)",
        params![work.id, work.agent_id, work.objective, work.revision,
            work.created_at.to_rfc3339(), work.updated_at.to_rfc3339(), serde_json::to_string(work)?],
    )?;
    Ok(())
}
