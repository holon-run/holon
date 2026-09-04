// Common imports used by both `test_support` and `tests` submodules via `use super::*`.
#[cfg(test)]
use crate::runtime_db::connection::{is_retryable_db_error, open_connection};
#[cfg(test)]
use crate::runtime_db::evidence::content_hash;
#[cfg(test)]
use crate::runtime_db::evidence::insert_audit_event_tx;
#[cfg(test)]
use crate::runtime_db::migrations::{
    apply_migration, apply_release_baseline, backfill_wait_condition_payload_columns,
    backfill_work_item_recheck_columns, current_schema_version, ensure_migration_table,
    max_known_migration_version, schema_fingerprint, table_exists, MIGRATIONS,
    PUBLISHED_MIGRATION_FLOOR, RELEASE_BASELINE_TARGET,
};
#[cfg(test)]
use crate::runtime_db::storage_domain::upsert_storage_domain;
#[cfg(test)]
use crate::runtime_db::{
    RuntimeDb, RuntimeDbRetryableError, RuntimeStateTransitionConflict, TASK_PAYLOAD_ARRAY_LIMIT,
    TASK_PAYLOAD_STRING_LIMIT,
};
#[cfg(test)]
use crate::types::{
    AgentIdentityRecord, AgentState, AuditEvent, BriefRecord, CallbackDeliveryMode,
    ExecutionRootEntry, ExternalTriggerRecord, ExternalTriggerScope, ExternalTriggerStatus,
    MessageEnvelope, QueueEntryRecord, QueueEntryStatus, TaskRecord, TaskStatus,
    ToolExecutionRecord, TranscriptEntry, TranscriptEntryKind, TurnOwner, TurnRecord,
    WaitConditionKind, WaitConditionRecord, WaitConditionStatus, WorkItemRecord, WorkItemState,
    WorkspaceEntry, WorkspaceOccupancyRecord,
};
#[cfg(test)]
use anyhow::{anyhow, bail, Context, Result};
#[cfg(test)]
use chrono::Utc;
#[cfg(test)]
use rusqlite::params;
#[cfg(test)]
use std::path::PathBuf;
#[cfg(test)]
use std::time::{Duration, Instant};

#[cfg(test)]
use crate::runtime_db::migrations::timestamp;
#[cfg(test)]
use crate::runtime_db::RuntimeDbLock;
#[cfg(test)]
use rusqlite::{ffi::ErrorCode, TransactionBehavior};
#[cfg(test)]
use std::collections::BTreeSet;

#[cfg(test)]
pub mod test_support {
    use super::*;
    use tempfile::TempDir;

    pub struct TempRuntimeDb {
        pub db: RuntimeDb,
        _temp_dir: TempDir,
    }

    impl TempRuntimeDb {
        pub fn new() -> Result<Self> {
            let temp_dir = tempfile::tempdir()?;
            let db = RuntimeDb::open_and_migrate(
                temp_dir.path().join("state/runtime.sqlite"),
                temp_dir.path().join("state/runtime.lock"),
            )?;
            Ok(Self {
                db,
                _temp_dir: temp_dir,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domain::execution_protocol::{
            ExecutionAttempt, ExecutionSourceIdentity, WaitReference, WorkItemExecutionRecord,
            WorkItemExecutionState,
        },
        domain::scheduler::SchedulerOwner,
        runtime_db::legacy_scheduler_wire::{
            ActivationBinding, ActivationCause, ActivationInputAttachment,
            ActivationLifecycleState, ActivationOrigin, ActivationPriority, ActivationProvenance,
            ActivationTrust, AdmitActivationCommand, AgentActivation, PreemptionPolicy, WorkDemand,
            WorkStatus,
        },
        runtime_db::migrations::{
            RETAINED_SCHEDULER_AUDIT_TABLES, RETIRED_SCHEDULER_SCHEMA_PREDECESSOR,
            RETIRED_SCHEDULER_TABLES,
        },
        runtime_db::observer_sync::{
            AGENT_ROSTER_LATEST_BRIEFS_SQL, EVENT_PROJECTION_EFFECT_VERIFIER_VERSION,
        },
        runtime_db::repositories::{enum_string, slim_task_record_for_payload},
        runtime_db::transitions::{
            AgentStateMutation, QueueHeadNoProgressCommand, QueueHeadNoProgressOutcome,
            TransitionFaultPoint,
        },
        system::WorkspaceAccessMode,
        types::{
            AgentKind, AgentOwnership, AgentProfilePreset, AgentRegistryStatus, AgentStatus,
            AgentVisibility, BriefKind,
        },
    };
    use rusqlite::OptionalExtension;
    use std::process::Command;
    use tempfile::tempdir;

    fn temp_paths() -> Result<(tempfile::TempDir, PathBuf, PathBuf)> {
        let temp_dir = tempdir()?;
        let db_path = temp_dir.path().join("state/runtime.sqlite");
        let lock_path = temp_dir.path().join("state/runtime.lock");
        Ok((temp_dir, db_path, lock_path))
    }

    fn wait_until(mut condition: impl FnMut() -> Result<bool>, label: &str) -> Result<()> {
        let started_at = Instant::now();
        loop {
            if condition()? {
                return Ok(());
            }
            if started_at.elapsed() > Duration::from_secs(2) {
                bail!("{label} did not become true");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn mark_migration_applied(connection: &rusqlite::Connection, name: &str) -> Result<()> {
        let migration = MIGRATIONS
            .iter()
            .find(|migration| migration.name == name)
            .ok_or_else(|| anyhow!("missing migration {name}"))?;
        connection.execute(
            "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
            (
                migration.version,
                migration.name,
                Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            ),
        )?;
        Ok(())
    }

    fn migrate_through(connection: &mut rusqlite::Connection, target_version: i64) -> Result<()> {
        ensure_migration_table(connection)?;
        for migration in MIGRATIONS
            .iter()
            .filter(|migration| migration.version <= target_version)
        {
            apply_migration(connection, migration)?;
        }
        assert_eq!(current_schema_version(connection)?, target_version);
        Ok(())
    }

    #[test]
    fn runtime_db_retryable_error_classification_survives_context() -> Result<()> {
        let (_temp_dir, db_path, _lock_path) = temp_paths()?;
        let error: anyhow::Error = RuntimeDbRetryableError::new(
            "starting immediate transaction",
            &db_path,
            "database is locked",
        )
        .into();
        let error = error.context("processing message");
        assert!(is_retryable_db_error(&error));
        assert!(!is_retryable_db_error(&anyhow!("not a db lock")));
        Ok(())
    }

    #[test]
    fn runtime_db_raw_sqlite_lock_errors_are_retryable() {
        let locked: anyhow::Error = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: ErrorCode::DatabaseLocked,
                extended_code: 0,
            },
            Some("database is locked".to_string()),
        )
        .into();
        let busy: anyhow::Error = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: ErrorCode::DatabaseBusy,
                extended_code: 0,
            },
            Some("database is busy".to_string()),
        )
        .into();
        let constraint: anyhow::Error = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: ErrorCode::ConstraintViolation,
                extended_code: 0,
            },
            None,
        )
        .into();

        assert!(is_retryable_db_error(
            &locked.context("inserting audit event")
        ));
        assert!(is_retryable_db_error(
            &busy.context("updating transcript entry")
        ));
        assert!(!is_retryable_db_error(&constraint));
    }

    #[test]
    fn runtime_db_sync_transaction_retries_retryable_body_error() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let mut attempts = 0usize;

        db.transaction(|tx| {
            attempts += 1;
            if attempts == 1 {
                return Err(RuntimeDbRetryableError::new(
                    "inserting audit event",
                    &db_path,
                    "database is locked",
                )
                .into());
            }
            insert_audit_event_tx(
                tx,
                Some("agent-a"),
                &AuditEvent::legacy("runtime_db_retry_body", serde_json::json!({})),
            )
        })?;

        assert_eq!(attempts, 2);
        let events = db.audit_events().recent(Some("agent-a"), 10)?;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "runtime_db_retry_body");
        Ok(())
    }

    fn task_record(id: &str, agent_id: &str, status: TaskStatus, offset: i64) -> TaskRecord {
        let created_at = Utc::now();
        TaskRecord {
            id: id.into(),
            agent_id: agent_id.into(),
            kind: crate::types::TaskKind::CommandTask,
            status,
            created_at,
            updated_at: created_at + chrono::Duration::seconds(offset),
            parent_message_id: None,
            work_item_id: None,
            summary: Some(id.into()),
            detail: Some(serde_json::json!({
                "cmd": "printf test",
                "output_path": format!("/tmp/{id}.log"),
                "output_summary": format!("{id} summary"),
                "exit_status": 0,
                "accepts_input": true,
                "input_target": "stdin"
            })),
            recovery: None,
        }
    }

    fn external_trigger_record(
        id: &str,
        agent_id: &str,
        status: ExternalTriggerStatus,
        offset: i64,
    ) -> ExternalTriggerRecord {
        let created_at = Utc::now() + chrono::Duration::seconds(offset);
        ExternalTriggerRecord {
            external_trigger_id: id.into(),
            target_agent_id: agent_id.into(),
            scope: ExternalTriggerScope::Agent,
            delivery_mode: CallbackDeliveryMode::EnqueueMessage,
            token: Some(format!("https://example.test/{id}")),
            token_hash: format!("hash-{id}"),
            status,
            created_at,
            revoked_at: None,
            last_delivered_at: None,
            delivery_count: 0,
        }
    }

    fn workspace_entry(id: &str, updated_offset: i64) -> WorkspaceEntry {
        let created_at = Utc::now();
        let mut entry = WorkspaceEntry::new(
            id,
            PathBuf::from(format!("/tmp/{id}")),
            Some(format!("repo-{id}")),
        );
        entry.workspace_alias = Some(format!("alias-{id}"));
        entry.workspace_kind = Some("project".into());
        entry.owner_agent_id = Some("agent-a".into());
        entry.created_at = created_at;
        entry.updated_at = created_at + chrono::Duration::seconds(updated_offset);
        entry
    }

    fn workspace_occupancy(id: &str, released_offset: Option<i64>) -> WorkspaceOccupancyRecord {
        let acquired_at = Utc::now();
        WorkspaceOccupancyRecord {
            occupancy_id: id.into(),
            execution_root_id: format!("exec-{id}"),
            workspace_id: format!("ws-{id}"),
            holder_agent_id: "agent-a".into(),
            access_mode: WorkspaceAccessMode::ExclusiveWrite,
            acquired_at,
            released_at: released_offset
                .map(|offset| acquired_at + chrono::Duration::seconds(offset)),
        }
    }

    fn agent_identity(agent_id: &str, updated_offset: i64) -> AgentIdentityRecord {
        let mut identity = AgentIdentityRecord::new(
            agent_id,
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        identity.created_at = Utc::now();
        identity.updated_at = identity.created_at + chrono::Duration::seconds(updated_offset);
        identity.status = AgentRegistryStatus::Active;
        identity
    }
    fn prepare_pre_execution_protocol_claim(
        db: &RuntimeDb,
        agent_id: &str,
        work_item_id: &str,
        message_id: &str,
        activation_id: &str,
    ) -> Result<()> {
        db.agent_states().upsert(&AgentState::new(agent_id))?;
        db.agent_identities().upsert(&agent_identity(agent_id, 0))?;

        let mut work = WorkItemRecord::new(agent_id, "migration claim", WorkItemState::Open);
        work.id = work_item_id.into();
        db.work_items().insert_new(&work)?;

        let mut message = MessageEnvelope::new(
            agent_id,
            crate::types::MessageKind::OperatorPrompt,
            crate::types::MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            crate::types::AuthorityClass::OperatorInstruction,
            crate::types::Priority::Normal,
            crate::types::MessageBody::Text {
                text: "resume after upgrade".into(),
            },
        );
        message.id = message_id.into();
        let message = db.messages().append_with_index_changes(&message, &[])?;

        let queued = QueueEntryRecord {
            message_id: message.id.clone(),
            agent_id: agent_id.into(),
            priority: message.priority.clone(),
            status: QueueEntryStatus::Queued,
            created_at: message.created_at,
            updated_at: message.created_at,
        };
        db.queue_entries().upsert(&queued)?;
        let mut dequeued = queued;
        dequeued.status = QueueEntryStatus::Dequeued;
        dequeued.updated_at = Utc::now();
        assert!(db.queue_entries().try_claim_queued_message(&dequeued)?);

        let scheduling_generation = 1;
        let admission = AdmitActivationCommand {
            authority_id: format!("authority-{activation_id}"),
            activation: AgentActivation {
                id: activation_id.into(),
                agent_id: agent_id.into(),
                state: ActivationLifecycleState::Admitted,
                cause: ActivationCause::OperatorInput {
                    message_id: message.id.clone(),
                    resume: None,
                },
                binding: ActivationBinding::WorkItem {
                    work_item_id: work_item_id.into(),
                },
                priority: ActivationPriority::Normal,
                preemption: PreemptionPolicy::AllowOperatorInterjection,
                source_revision: message.message_seq,
                idempotency_key: format!("operator-message:{}", message.id),
                provenance: ActivationProvenance {
                    origin: ActivationOrigin::Operator,
                    trust: ActivationTrust::OperatorInstruction,
                    source_id: message.id,
                    correlation_id: None,
                    causation_id: None,
                },
            },
            expected_scheduling_generation: scheduling_generation,
            expected_dispatch_revision: 0,
        };
        let now = timestamp(Utc::now());
        db.connection()?.execute(
            "INSERT INTO scheduler_activations (
               agent_id, activation_id, authority_id, owner_kind, owner_id, work_item_id,
               admitted_generation, admission_kind, recovery_for_activation_id, wait_id,
               wait_generation, lifecycle_state, idempotency_key, payload_json, created_at,
               updated_at
             ) VALUES (
               ?1, ?2, ?3, 'work_item', ?4, ?4, ?5, 'scheduling', NULL, NULL, NULL,
               'admitted', ?6, ?7, ?8, ?8
             )",
            params![
                agent_id,
                activation_id,
                admission.authority_id,
                work_item_id,
                scheduling_generation,
                admission.activation.idempotency_key,
                serde_json::to_string(&admission)?,
                now,
            ],
        )?;
        Ok(())
    }

    fn downgrade_before_execution_protocol(db_path: &std::path::Path) -> Result<()> {
        let connection = open_connection(db_path)?;
        connection.execute_batch(
            "DELETE FROM schema_migrations WHERE version >= 41;
             DROP TABLE execution_protocol_command_results;
             DROP TABLE execution_protocol_outcomes;
             DROP INDEX execution_protocol_one_open_attempt;
             DROP TABLE execution_protocol_attempts;
             DROP TABLE execution_protocol_work_items;
             DROP TABLE execution_protocol_partitions;
             ALTER TABLE agent_states DROP COLUMN control_revision;

             CREATE TABLE scheduler_work_demands (
               agent_id TEXT NOT NULL,
               work_item_id TEXT NOT NULL,
               metadata_revision INTEGER NOT NULL CHECK (metadata_revision >= 0),
               scheduling_generation INTEGER NOT NULL CHECK (scheduling_generation >= 0),
               status TEXT NOT NULL CHECK (
                 status IN ('runnable', 'waiting', 'needs_settlement', 'paused', 'terminal')
               ),
               status_reference_id TEXT,
               capabilities_json TEXT NOT NULL,
               locks_json TEXT NOT NULL,
               locality TEXT NOT NULL,
               cost_class TEXT NOT NULL,
               payload_json TEXT NOT NULL,
               updated_at TEXT NOT NULL,
               PRIMARY KEY (agent_id, work_item_id),
               CHECK (
                 (status IN ('runnable', 'terminal') AND status_reference_id IS NULL)
                 OR (
                   status IN ('waiting', 'needs_settlement', 'paused')
                   AND status_reference_id IS NOT NULL
                 )
               )
             );",
        )?;
        Ok(())
    }

    fn restore_pre_execution_protocol_work_demand(
        db_path: &std::path::Path,
        agent_id: &str,
        work_item_id: &str,
    ) -> Result<()> {
        let demand = WorkDemand {
            metadata_revision: 1,
            scheduling_generation: 1,
            status: WorkStatus::Runnable,
            capabilities: BTreeSet::new(),
            locks: BTreeSet::new(),
            locality: "local".into(),
            cost_class: "small".into(),
        };
        open_connection(db_path)?.execute(
            "INSERT INTO scheduler_work_demands (
               agent_id,
               work_item_id,
               metadata_revision,
               scheduling_generation,
               status,
               status_reference_id,
               capabilities_json,
               locks_json,
               locality,
               cost_class,
               payload_json,
               updated_at
             ) VALUES (?1, ?2, 1, 1, 'runnable', NULL, '[]', '[]', 'local', 'small', ?3, ?4)",
            params![
                agent_id,
                work_item_id,
                serde_json::to_string(&demand)?,
                timestamp(Utc::now()),
            ],
        )?;
        Ok(())
    }

    #[test]
    fn runtime_db_fresh_migration_creates_foundation_schema() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let connection = db.connection()?;

        let version = db.current_schema_version()?;
        assert_eq!(version, max_known_migration_version());
        for table in [
            "schema_migrations",
            "schema_migration_baselines",
            "storage_domains",
            "runtime_metadata",
            "agents",
            "audit_events",
            "audit_event_retention_watermarks",
            "runtime_sequences",
            "work_items",
            "tasks",
            "external_triggers",
            "messages",
            "transcript_entries",
            "tool_executions",
            "model_requests",
            "model_responses",
            "briefs",
            "delivery_summaries",
            "artifact_metadata",
            "wait_conditions",
            "queue_entries",
            "queue_head_no_progress",
            "timers",
            "turn_records",
            "agent_states",
            "workspace_entries",
            "workspace_occupancies",
            "agent_identities",
            "agent_identity_reservations",
            "observer_sync_capability_verifications",
            "context_episode_anchors",
            "scheduler_activations",
            "scheduler_activation_settlements",
            "scheduler_continuation_admissions",
            "scheduler_activation_sources",
            "scheduler_activation_inputs",
            "scheduler_protocol_command_results",
            "scheduler_protocol_command_conflict_attempts",
            "scheduler_rollout_command_results",
        ] {
            let count: i64 = connection.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [table],
                |row| row.get(0),
            )?;
            assert_eq!(count, 1, "missing table {table}");
        }
        for table in RETIRED_SCHEDULER_TABLES {
            assert!(
                !table_exists(&connection, table)?,
                "fresh schema must not create retired scheduler table {table}"
            );
        }
        assert!(
            !table_exists(&connection, "workspace_id_aliases")?,
            "retired workspace ID alias table should be removed"
        );
        let work_item_columns = connection
            .prepare("PRAGMA table_info(work_items)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        assert!(
            !work_item_columns.iter().any(|column| column == "readiness"),
            "derived WorkItem readiness must not be persisted"
        );
        let readiness_index_count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'index' AND name = 'idx_work_items_readiness'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(readiness_index_count, 0);
        let activation_input_columns = connection
            .prepare("PRAGMA table_info(scheduler_activation_inputs)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<std::result::Result<BTreeSet<_>, _>>()?;
        assert!(activation_input_columns.contains("owner_kind"));
        assert!(activation_input_columns.contains("owner_id"));
        assert!(activation_input_columns.contains("expected_admitted_generation"));
        assert!(!activation_input_columns.contains("work_item_id"));
        assert!(!activation_input_columns.contains("expected_scheduling_generation"));

        let baseline: (i64, i64, String, String) = connection.query_row(
            "SELECT source_version, target_version, covered_versions_json,
                    schema_fingerprint
             FROM schema_migration_baselines",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        assert_eq!(baseline.0, PUBLISHED_MIGRATION_FLOOR);
        assert_eq!(baseline.1, RELEASE_BASELINE_TARGET);
        assert_eq!(
            serde_json::from_str::<Vec<i64>>(&baseline.2)?,
            (PUBLISHED_MIGRATION_FLOOR + 1..=RELEASE_BASELINE_TARGET).collect::<Vec<_>>()
        );
        let mut baseline_connection = rusqlite::Connection::open_in_memory()?;
        ensure_migration_table(&baseline_connection)?;
        for migration in MIGRATIONS
            .iter()
            .filter(|migration| migration.version <= PUBLISHED_MIGRATION_FLOOR)
        {
            apply_migration(&mut baseline_connection, migration)?;
        }
        apply_release_baseline(&mut baseline_connection)?;
        assert_eq!(baseline.3, schema_fingerprint(&baseline_connection)?);

        Ok(())
    }

    #[test]
    fn append_brief_with_created_event_commits_and_deduplicates_publication() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let mut brief = BriefRecord::new(
            "agent-a",
            crate::types::BriefKind::Result,
            "atomic brief",
            None,
            None,
        );
        brief.turn_id = Some("turn-1".into());
        let event = crate::types::brief_created_event_for(&brief)?;

        let commit =
            db.evidence()
                .append_brief_with_created_event(Some("agent-a"), &brief, &event, &[])?;
        assert!(commit.event_inserted);
        assert_eq!(commit.brief.created_event_seq, Some(commit.event.event_seq));

        // A retry of the same publication reuses the committed event and
        // linkage instead of appending a duplicate event.
        let retry =
            db.evidence()
                .append_brief_with_created_event(Some("agent-a"), &brief, &event, &[])?;
        assert!(!retry.event_inserted);
        assert_eq!(retry.event.event_seq, commit.event.event_seq);
        assert_eq!(retry.event.id, commit.event.id);
        assert_eq!(retry.brief.created_event_seq, Some(commit.event.event_seq));

        let events = db.audit_events().recent(Some("agent-a"), 10)?;
        let brief_created: Vec<_> = events
            .iter()
            .filter(|event| event.kind == "brief_created")
            .collect();
        assert_eq!(brief_created.len(), 1);
        let stored = db
            .evidence()
            .brief_by_id("agent-a", &brief.id)?
            .expect("brief stored");
        assert_eq!(stored.created_event_seq, Some(commit.event.event_seq));

        drop(db);
        let reopened = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        assert!(
            reopened
                .observer_sync_foundations()?
                .brief_atomic_linkage_verified
        );
        Ok(())
    }

    #[test]
    fn append_brief_with_created_event_rejects_conflicting_relink_and_rolls_back() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let brief = BriefRecord::new(
            "agent-a",
            crate::types::BriefKind::Result,
            "linked once",
            None,
            None,
        );
        let event = crate::types::brief_created_event_for(&brief)?;
        let commit =
            db.evidence()
                .append_brief_with_created_event(Some("agent-a"), &brief, &event, &[])?;

        // A second, different event identity for the same Brief must not
        // relink it, and the whole transaction rolls back.
        let mut other = crate::types::brief_created_event_for(&brief)?;
        other.id = "event_conflicting_relink".into();
        other.created_at = Utc::now();
        let error = db
            .evidence()
            .append_brief_with_created_event(Some("agent-a"), &brief, &other, &[])
            .unwrap_err();
        assert!(
            error.to_string().contains("refusing relink"),
            "unexpected error: {error:#}"
        );

        let events = db.audit_events().recent(Some("agent-a"), 10)?;
        let brief_created: Vec<_> = events
            .iter()
            .filter(|event| event.kind == "brief_created")
            .collect();
        assert_eq!(brief_created.len(), 1);
        let stored = db
            .evidence()
            .brief_by_id("agent-a", &brief.id)?
            .expect("brief stored");
        assert_eq!(stored.created_event_seq, Some(commit.event.event_seq));
        Ok(())
    }

    #[test]
    fn migration_49_backfills_unique_linkage_and_marks_uncertain_history() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        {
            let mut connection = open_connection(&db_path)?;
            migrate_through(&mut connection, 48)?;
            let seed_brief = |evidence_id: &str, text: &str| -> Result<BriefRecord> {
                let mut brief =
                    BriefRecord::new("agent-a", crate::types::BriefKind::Result, text, None, None);
                brief.id = evidence_id.into();
                connection.execute(
                    "INSERT INTO briefs (
                       evidence_id, agent_id, created_at, kind, preview, payload_json
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        brief.id,
                        brief.agent_id,
                        timestamp(brief.created_at),
                        "result",
                        text,
                        serde_json::to_string(&brief)?
                    ],
                )?;
                Ok(brief)
            };
            let linked = seed_brief("brief-linked", "unique candidate")?;
            let _missing = seed_brief("brief-missing", "no candidate")?;
            let ambiguous = seed_brief("brief-ambiguous", "two candidates")?;
            let missing_seq = seed_brief("brief-missing-seq", "candidate without sequence")?;
            let seed_event =
                |audit_event_id: &str, event_seq: Option<i64>, brief: &BriefRecord| -> Result<()> {
                    let mut event = crate::types::brief_created_event_for(brief)?;
                    event.id = audit_event_id.into();
                    event.event_seq = event_seq.unwrap_or_default() as u64;
                    connection.execute(
                        "INSERT INTO audit_events (
                           audit_event_id, event_seq, agent_id, kind, created_at, data_json
                         ) VALUES (?1, ?2, ?3, 'brief_created', ?4, ?5)",
                        params![
                            audit_event_id,
                            event_seq,
                            "agent-a",
                            timestamp(Utc::now()),
                            serde_json::to_string(&event)?
                        ],
                    )?;
                    Ok(())
                };
            seed_event("event-linked", Some(11), &linked)?;
            seed_event("event-ambiguous-a", Some(21), &ambiguous)?;
            seed_event("event-ambiguous-b", Some(22), &ambiguous)?;
            seed_event("event-missing-seq", None, &missing_seq)?;

            apply_migration(&mut connection, &MIGRATIONS[48])?;

            let linkage: (Option<i64>, String) = connection.query_row(
                "SELECT created_event_seq, payload_json FROM briefs
                 WHERE evidence_id = 'brief-linked'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            assert_eq!(linkage.0, Some(11));
            let stored: BriefRecord = serde_json::from_str(&linkage.1)?;
            assert_eq!(stored.created_event_seq, Some(11));
            // Migration must not modify Brief content or timestamps.
            assert_eq!(stored.text, linked.text);
            assert_eq!(stored.created_at, linked.created_at);
            assert_eq!(stored.kind, linked.kind);
            assert_eq!(stored.agent_id, linked.agent_id);

            for (brief_id, expected_reason) in [
                ("brief-missing", "no_candidate_event"),
                ("brief-ambiguous", "ambiguous_candidate_events"),
                ("brief-missing-seq", "candidate_event_missing_seq"),
            ] {
                let linkage: Option<i64> = connection.query_row(
                    "SELECT created_event_seq FROM briefs WHERE evidence_id = ?1",
                    [brief_id],
                    |row| row.get(0),
                )?;
                assert_eq!(linkage, None, "{brief_id} must stay unlinked");
                let (reason, payload_json): (String, String) = connection.query_row(
                    "SELECT reason, payload_json FROM brief_created_linkage_uncertain
                     JOIN briefs ON briefs.evidence_id = brief_created_linkage_uncertain.evidence_id
                     WHERE brief_created_linkage_uncertain.evidence_id = ?1",
                    [brief_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                assert_eq!(reason, expected_reason);
                let stored: BriefRecord = serde_json::from_str(&payload_json)?;
                assert_eq!(stored.created_event_seq, None);
            }

            // Fresh reopen persists the capability verification for the
            // sound linkage state.
            let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
            let foundations = db.observer_sync_foundations()?;
            assert!(foundations.brief_atomic_linkage_verified);
        }
        Ok(())
    }

    #[test]
    fn migration_49_backfill_is_bounded() -> Result<()> {
        let (_temp_dir, db_path, _lock_path) = temp_paths()?;
        let mut connection = open_connection(&db_path)?;
        migrate_through(&mut connection, 48)?;
        connection.execute_batch("ALTER TABLE briefs ADD COLUMN created_event_seq INTEGER;")?;

        const BRIEF_COUNT: i64 = 512;
        const NOISE_EVENT_COUNT: i64 = 4_096;
        {
            let transaction = connection.transaction()?;
            for index in 0..BRIEF_COUNT {
                let evidence_id = format!("brief-bounded-{index}");
                let mut brief = BriefRecord::new(
                    "agent-a",
                    crate::types::BriefKind::Result,
                    format!("bounded candidate {index}"),
                    None,
                    None,
                );
                brief.id = evidence_id.clone();
                let mut event = crate::types::brief_created_event_for(&brief)?;
                event.id = format!("event-bounded-{index}");
                event.event_seq = (index + 1) as u64;
                transaction.execute(
                    "INSERT INTO briefs (
                       evidence_id, agent_id, created_at, kind, preview, payload_json
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        evidence_id,
                        brief.agent_id,
                        timestamp(brief.created_at),
                        "result",
                        brief.text,
                        serde_json::to_string(&brief)?
                    ],
                )?;
                transaction.execute(
                    "INSERT INTO audit_events (
                       audit_event_id, event_seq, agent_id, kind, created_at, data_json
                     ) VALUES (?1, ?2, ?3, 'brief_created', ?4, ?5)",
                    params![
                        event.id,
                        index + 1,
                        "agent-a",
                        timestamp(Utc::now()),
                        serde_json::to_string(&event)?
                    ],
                )?;
            }
            for index in 0..NOISE_EVENT_COUNT {
                transaction.execute(
                    "INSERT INTO audit_events (
                       audit_event_id, event_seq, agent_id, kind, created_at, data_json
                     ) VALUES (?1, ?2, ?3, 'task_created', ?4, '{}')",
                    params![
                        format!("event-noise-{index}"),
                        BRIEF_COUNT + index + 1,
                        "agent-a",
                        timestamp(Utc::now()),
                    ],
                )?;
            }
            transaction.commit()?;
        }

        // Keep this candidate table and index SQL aligned with
        // backfill_brief_created_event_linkage.
        connection.execute_batch(
            "CREATE TEMP TABLE _brief_created_candidates AS
             SELECT audit_event_id,
                    event_seq,
                    agent_id AS agent_id_col,
                    COALESCE(json_extract(data_json, '$.data.agent_id'), '') AS agent_id_json,
                    COALESCE(json_extract(data_json, '$.data.brief_id'), '') AS brief_id
             FROM audit_events
             WHERE kind = 'brief_created';
             CREATE INDEX _idx_brief_created_candidates_lookup
               ON _brief_created_candidates(brief_id, agent_id_col, agent_id_json);",
        )?;
        let query_plan = {
            let mut statement = connection.prepare(
                "EXPLAIN QUERY PLAN
                 SELECT briefs.evidence_id,
                        briefs.agent_id,
                        briefs.payload_json,
                        COUNT(candidates.audit_event_id),
                        CASE WHEN COUNT(candidates.audit_event_id) = 1
                             THEN MIN(candidates.event_seq)
                             ELSE NULL
                        END
                 FROM briefs
                 LEFT JOIN _brief_created_candidates AS candidates
                   ON candidates.brief_id = briefs.evidence_id
                  AND (candidates.agent_id_col = briefs.agent_id
                       OR candidates.agent_id_json = briefs.agent_id)
                 WHERE briefs.created_event_seq IS NULL
                 GROUP BY briefs.evidence_id, briefs.agent_id, briefs.payload_json",
            )?;
            let payloads = statement
                .query_map([], |row| row.get::<_, String>(3))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            payloads
        };
        assert!(
            query_plan.iter().any(|detail| {
                detail
                    .contains("SEARCH candidates USING INDEX _idx_brief_created_candidates_lookup")
            }),
            "candidate aggregation must use the temporary lookup index: {query_plan:?}"
        );
        assert!(
            query_plan
                .iter()
                .all(|detail| !detail.contains("audit_events")),
            "candidate aggregation must not rescan audit_events: {query_plan:?}"
        );
        connection.execute_batch("DROP TABLE temp._brief_created_candidates;")?;

        apply_migration(&mut connection, &MIGRATIONS[48])?;
        let linked_count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM briefs WHERE created_event_seq IS NOT NULL",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(linked_count, BRIEF_COUNT);
        Ok(())
    }

    #[test]
    fn fresh_runtime_db_verifies_brief_atomic_linkage_capability() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let foundations = db.observer_sync_foundations()?;
        assert!(foundations.brief_atomic_linkage_verified);
        Ok(())
    }

    #[test]
    fn release_baseline_matches_compatibility_migration_schema() -> Result<()> {
        let (_baseline_temp, baseline_path, baseline_lock) = temp_paths()?;
        let baseline = RuntimeDb::open_and_migrate(&baseline_path, &baseline_lock)?;
        let baseline_connection = baseline.connection()?;
        let schema_objects = |connection: &rusqlite::Connection| -> Result<Vec<String>> {
            let mut statement = connection.prepare(
                "SELECT type, name, sql
                 FROM sqlite_master
                 WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%'
                 ORDER BY type, name",
            )?;
            let objects = statement
                .query_map([], |row| {
                    Ok(format!(
                        "{}\n{}\n{}",
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" ")
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(objects)
        };
        let baseline_schema = schema_objects(&baseline_connection)?;
        let baseline_fingerprint = schema_fingerprint(&baseline_connection)?;
        drop(baseline_connection);
        drop(baseline);

        let (_compat_temp, compat_path, compat_lock) = temp_paths()?;
        {
            let mut connection = open_connection(&compat_path)?;
            ensure_migration_table(&connection)?;
            for migration in MIGRATIONS {
                apply_migration(&mut connection, migration)?;
            }
            backfill_wait_condition_payload_columns(&connection)?;
            backfill_work_item_recheck_columns(&connection)?;
        }
        let compat = RuntimeDb::open_and_migrate(&compat_path, &compat_lock)?;
        let compat_connection = compat.connection()?;
        let compat_schema = schema_objects(&compat_connection)?;
        let baseline_only = baseline_schema
            .iter()
            .filter(|object| !compat_schema.contains(object))
            .collect::<Vec<_>>();
        let compatibility_only = compat_schema
            .iter()
            .filter(|object| !baseline_schema.contains(object))
            .collect::<Vec<_>>();
        assert_eq!(
            baseline_schema,
            compat_schema,
            "release baseline and checkpoint compatibility chain must have identical schema objects\nbaseline only: {baseline_only:#?}\ncompatibility only: {compatibility_only:#?}"
        );
        assert_eq!(
            baseline_fingerprint,
            schema_fingerprint(&compat_connection)?,
            "release baseline and checkpoint compatibility chain must converge"
        );
        let baseline_count: i64 = compat_connection.query_row(
            "SELECT COUNT(*) FROM schema_migration_baselines",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(baseline_count, 0);
        Ok(())
    }

    #[test]
    fn unreleased_checkpoints_use_compatibility_chain_without_baseline() -> Result<()> {
        for checkpoint in PUBLISHED_MIGRATION_FLOOR + 1..=RELEASE_BASELINE_TARGET {
            let (_temp_dir, db_path, lock_path) = temp_paths()?;
            {
                let mut connection = open_connection(&db_path)?;
                ensure_migration_table(&connection)?;
                for migration in MIGRATIONS
                    .iter()
                    .filter(|migration| migration.version <= checkpoint)
                {
                    apply_migration(&mut connection, migration)?;
                }
                backfill_wait_condition_payload_columns(&connection)?;
                backfill_work_item_recheck_columns(&connection)?;
            }

            let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)
                .with_context(|| format!("migrating unreleased checkpoint {checkpoint}"))?;
            let connection = db.connection()?;
            assert_eq!(
                current_schema_version(&connection)?,
                MIGRATIONS.last().map_or(0, |migration| migration.version),
                "checkpoint {checkpoint} did not reach the current schema"
            );
            let baseline_count: i64 = connection.query_row(
                "SELECT COUNT(*) FROM schema_migration_baselines",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(
                baseline_count, 0,
                "checkpoint {checkpoint} must use the compatibility chain"
            );
        }
        Ok(())
    }

    #[test]
    fn release_baseline_failure_rolls_back_to_published_floor() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        {
            let mut connection = open_connection(&db_path)?;
            ensure_migration_table(&connection)?;
            for migration in MIGRATIONS
                .iter()
                .filter(|migration| migration.version <= PUBLISHED_MIGRATION_FLOOR)
            {
                apply_migration(&mut connection, migration)?;
            }
            connection.execute(
                "INSERT INTO work_items (
                   work_item_id, agent_id, state, objective, plan_status, readiness,
                   revision, current_focus, created_at, updated_at, payload_json
                 ) VALUES (
                   'work-invalid', 'agent-a', 'completed', 'invalid focus', NULL, NULL,
                   1, 1, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP, '{}'
                 )",
                [],
            )?;
        }

        let error = RuntimeDb::open_and_migrate(&db_path, &lock_path)
            .expect_err("invalid release data must fail the baseline");
        assert!(
            error.to_string().contains("invalid legacy focus"),
            "{error:#}"
        );
        let connection = open_connection(&db_path)?;
        assert_eq!(
            current_schema_version(&connection)?,
            PUBLISHED_MIGRATION_FLOOR
        );
        assert!(!table_exists(&connection, "runtime_sequences")?);
        let baseline_count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM schema_migration_baselines",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(baseline_count, 0);
        Ok(())
    }

    #[test]
    fn migration_38_upgrades_legacy_activation_input_owner() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        {
            let mut connection = open_connection(&db_path)?;
            ensure_migration_table(&connection)?;
            for migration in MIGRATIONS
                .iter()
                .filter(|migration| migration.version <= 37)
            {
                apply_migration(&mut connection, migration)?;
            }
            connection.execute(
                "INSERT INTO scheduler_activation_inputs (
                   agent_id,
                   attachment_id,
                   activation_id,
                   work_item_id,
                   expected_scheduling_generation,
                   expected_dispatch_revision,
                   message_id,
                   turn_id,
                   boundary,
                   round,
                   payload_json,
                   created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                (
                    "agent-a",
                    "attachment-a",
                    "activation-a",
                    "work-a",
                    3_u64,
                    4_u64,
                    "message-a",
                    "turn-a",
                    "after_provider_round",
                    2_u64,
                    serde_json::json!({
                        "id": "attachment-a",
                        "activation_id": "activation-a",
                        "work_item_id": "work-a",
                        "expected_scheduling_generation": 3,
                        "expected_dispatch_revision": 4,
                        "message_id": "message-a",
                        "turn_id": "turn-a",
                        "boundary": "after_provider_round",
                        "round": 2,
                        "provenance": {
                            "origin": "operator",
                            "trust": "operator_instruction",
                            "source_id": "message-a",
                            "correlation_id": "activation-a",
                            "causation_id": null
                        },
                        "created_at": "2026-07-28T00:00:00Z"
                    })
                    .to_string(),
                    "2026-07-28T00:00:00Z",
                ),
            )?;
        }

        RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let connection = open_connection(&db_path)?;
        let (owner_kind, owner_id, generation, payload_json): (String, String, u64, String) =
            connection.query_row(
                "SELECT owner_kind, owner_id, expected_admitted_generation, payload_json
                 FROM scheduler_activation_inputs
                 WHERE agent_id = 'agent-a' AND attachment_id = 'attachment-a'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
        assert_eq!(
            (owner_kind, owner_id, generation),
            ("work_item".into(), "work-a".into(), 3)
        );
        let attachment: ActivationInputAttachment = serde_json::from_str(&payload_json)?;
        assert_eq!(
            attachment.owner,
            SchedulerOwner::WorkItem {
                work_item_id: "work-a".into(),
            }
        );
        assert_eq!(attachment.expected_admitted_generation, 3);
        let payload: serde_json::Value = serde_json::from_str(&payload_json)?;
        assert!(payload.get("work_item_id").is_none());
        assert!(payload.get("expected_scheduling_generation").is_none());
        Ok(())
    }

    #[test]
    fn migration_42_preserves_scheduler_runtime_consistency_boundary() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        {
            let mut connection = open_connection(&db_path)?;
            ensure_migration_table(&connection)?;
            for migration in MIGRATIONS
                .iter()
                .filter(|migration| migration.version <= 41)
            {
                apply_migration(&mut connection, migration)?;
            }
            let ordinary_fence_sql: String = connection.query_row(
                "SELECT sql
                 FROM sqlite_master
                 WHERE type = 'index'
                   AND name = 'idx_scheduler_activations_ordinary_admission_fence'",
                [],
                |row| row.get(0),
            )?;
            assert!(
                !ordinary_fence_sql.contains("internal_followup"),
                "migration 41 unexpectedly includes InternalFollowup admission"
            );
            connection.execute_batch(
                r#"
INSERT INTO scheduler_activation_sources (
  agent_id, activation_id, source_kind, source_identity, payload_json, created_at
) VALUES (
  'agent-a', 'activation-a', 'operator_input', 'message-a', '{}',
  '2026-08-03T00:00:00Z'
);

INSERT INTO scheduler_activations (
  agent_id, activation_id, authority_id, owner_kind, owner_id, work_item_id,
  admitted_generation, admission_kind, recovery_for_activation_id, wait_id,
  wait_generation, lifecycle_state, idempotency_key, payload_json, created_at,
  updated_at
) VALUES (
  'agent-a', 'activation-a', 'authority-a', 'work_item', 'work-a', 'work-a',
  1, 'scheduling', NULL, NULL, NULL, 'settled', 'activation-a', '{}',
  '2026-08-03T00:00:00Z', '2026-08-03T00:00:00Z'
);
"#,
            )?;
        }

        RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let connection = open_connection(&db_path)?;
        let preserved: (String, String) = connection.query_row(
            "SELECT admission_kind, lifecycle_state
             FROM scheduler_activations
             WHERE agent_id = 'agent-a' AND activation_id = 'activation-a'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(preserved, ("scheduling".into(), "settled".into()));

        let foreign_key_count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM pragma_foreign_key_list('scheduler_activations')",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(
            foreign_key_count, 0,
            "migration 42 must preserve the runtime-owned scheduler consistency boundary"
        );
        let ordinary_fence_sql: String = connection.query_row(
            "SELECT sql
             FROM sqlite_master
             WHERE type = 'index'
               AND name = 'idx_scheduler_activations_ordinary_admission_fence'",
            [],
            |row| row.get(0),
        )?;
        assert!(
            ordinary_fence_sql.contains("internal_followup"),
            "migration 42 must include InternalFollowup in the ordinary admission fence"
        );
        let state_index_count: i64 = connection.query_row(
            "SELECT COUNT(*)
             FROM sqlite_master
             WHERE type = 'index'
               AND name = 'idx_scheduler_activations_state'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(
            state_index_count, 1,
            "migration 42 must restore the scheduler activation state index"
        );
        let foreign_key_violations = connection
            .prepare("PRAGMA foreign_key_check")?
            .query_map([], |_| Ok(()))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        assert!(
            foreign_key_violations.is_empty(),
            "migration 42 introduced foreign key violations"
        );
        Ok(())
    }

    #[test]
    fn migrations_normalize_unsafe_operator_interjection_authority() -> Result<()> {
        let (_temp_dir, db_path, _lock_path) = temp_paths()?;
        {
            let mut connection = open_connection(&db_path)?;
            ensure_migration_table(&connection)?;
            for migration in MIGRATIONS
                .iter()
                .filter(|migration| migration.version <= 37)
            {
                apply_migration(&mut connection, migration)?;
            }
            connection.execute_batch(
                r#"
INSERT INTO scheduler_rollout_preflights (
  preflight_revision, manifest_revision, state, manifest_json, created_at, updated_at
) VALUES (1, 1, 'consumed', '{}', '2026-07-28T00:00:00Z', '2026-07-28T00:00:00Z');
INSERT INTO scheduler_rollout_manifests (
  manifest_revision, preflight_revision, payload_json, installed_at
) VALUES (1, 1, '{}', '2026-07-28T00:00:00Z');
INSERT INTO scheduler_scenario_authorities (
  scenario_class, mode, rollback_target, manifest_revision, preflight_revision, updated_at
) VALUES
  ('operator_interjection', 'authoritative', 'shadow', 1, 1, '2026-07-28T00:00:00Z'),
  ('work_item_autonomous_continuation', 'authoritative', 'shadow', 1, 1, '2026-07-28T00:00:00Z'),
  ('exact_task_rejoin', 'authoritative', 'shadow', 1, 1, '2026-07-28T00:00:00Z'),
  ('exact_wait_resume', 'authoritative', 'shadow', 1, 1, '2026-07-28T00:00:00Z'),
  ('explicitly_bound_operator_input', 'shadow', 'off', NULL, NULL, '2026-07-28T00:00:00Z'),
  ('settlement', 'authoritative', 'shadow', 1, 1, '2026-07-28T00:00:00Z');
"#,
            )?;
        }

        let mut connection = open_connection(&db_path)?;
        apply_migration(&mut connection, &MIGRATIONS[37])?;
        let normalized: (String, Option<i64>, Option<i64>, String) = connection.query_row(
            "SELECT mode, manifest_revision, preflight_revision, rollback_target
             FROM scheduler_scenario_authorities
             WHERE scenario_class = 'operator_interjection'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        assert_eq!(normalized, ("shadow".into(), None, None, "shadow".into()));
        Ok(())
    }
    #[test]
    fn migration_38_preserves_safe_operator_interjection_authority() -> Result<()> {
        let (_temp_dir, db_path, _lock_path) = temp_paths()?;
        {
            let mut connection = open_connection(&db_path)?;
            ensure_migration_table(&connection)?;
            for migration in MIGRATIONS
                .iter()
                .filter(|migration| migration.version <= 37)
            {
                apply_migration(&mut connection, migration)?;
            }
            connection.execute_batch(
                r#"
INSERT INTO scheduler_rollout_preflights (
  preflight_revision, manifest_revision, state, manifest_json, created_at, updated_at
) VALUES (1, 1, 'consumed', '{}', '2026-07-28T00:00:00Z', '2026-07-28T00:00:00Z');
INSERT INTO scheduler_rollout_manifests (
  manifest_revision, preflight_revision, payload_json, installed_at
) VALUES (1, 1, '{}', '2026-07-28T00:00:00Z');
INSERT INTO scheduler_scenario_authorities (
  scenario_class, mode, rollback_target, manifest_revision, preflight_revision, updated_at
) VALUES
  ('operator_interjection', 'authoritative', 'shadow', 1, 1, '2026-07-28T00:00:00Z'),
  ('work_item_autonomous_continuation', 'authoritative', 'shadow', 1, 1, '2026-07-28T00:00:00Z'),
  ('exact_task_rejoin', 'authoritative', 'shadow', 1, 1, '2026-07-28T00:00:00Z'),
  ('exact_wait_resume', 'authoritative', 'shadow', 1, 1, '2026-07-28T00:00:00Z'),
  ('explicitly_bound_operator_input', 'authoritative', 'shadow', 1, 1, '2026-07-28T00:00:00Z'),
  ('settlement', 'authoritative', 'shadow', 1, 1, '2026-07-28T00:00:00Z');
"#,
            )?;
        }

        let mut connection = open_connection(&db_path)?;
        apply_migration(&mut connection, &MIGRATIONS[37])?;
        let preserved: (String, Option<i64>, Option<i64>) = connection.query_row(
            "SELECT mode, manifest_revision, preflight_revision
             FROM scheduler_scenario_authorities
             WHERE scenario_class = 'operator_interjection'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(preserved, ("authoritative".into(), Some(1), Some(1)));
        Ok(())
    }

    #[test]
    fn scheduler_protocol_command_results_keep_first_seen_identities() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let connection = db.connection()?;
        let now = Utc::now().to_rfc3339();
        connection.execute(
            "INSERT INTO scheduler_protocol_command_results (
               agent_id,
               command_kind,
               command_identity,
               canonical_schema_version,
               payload_hash,
               decision,
               conflict_kind,
               conflict_code,
               result_references_json,
               pre_state_fence_json,
               post_state_fence_json,
               outcome_json,
               created_at
             ) VALUES (
               'agent-a',
               'admit_activation',
               'activation-a',
               1,
               'hash-a',
               'rejected',
               'stale_generation',
               'stale_work_generation',
               '[]',
               '{}',
               '{}',
               '{}',
               ?1
             )",
            [&now],
        )?;
        let error = connection
            .execute(
                "INSERT INTO scheduler_protocol_command_results (
                   agent_id,
                   command_kind,
                   command_identity,
                   canonical_schema_version,
                   payload_hash,
                   decision,
                   conflict_kind,
                   conflict_code,
                   result_references_json,
                   pre_state_fence_json,
                   post_state_fence_json,
                   outcome_json,
                   created_at
                 ) VALUES (
                   'agent-a',
                   'admit_activation',
                   'activation-a',
                   1,
                   'hash-b',
                   'admitted',
                   NULL,
                   NULL,
                   '[]',
                   '{}',
                   '{}',
                   '{}',
                   ?1
                 )",
                [&now],
            )
            .unwrap_err();
        assert_eq!(
            error.sqlite_error_code(),
            Some(ErrorCode::ConstraintViolation)
        );

        Ok(())
    }
    #[test]
    fn event_log_epoch_is_stable_across_reopen() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let first = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let first_epoch = first.event_log_epoch()?;
        assert!(first_epoch.starts_with("epoch_"));
        drop(first);

        let reopened = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        assert_eq!(reopened.event_log_epoch()?, first_epoch);
        Ok(())
    }

    #[test]
    fn audit_event_identity_rejects_conflicting_content() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let event = AuditEvent::legacy("fixture", serde_json::json!({ "value": 1 }));
        let first = db.audit_events().append(Some("agent-a"), &event)?;
        let repeated = db.audit_events().append(Some("agent-a"), &event)?;
        assert_eq!(repeated, first);

        let mut conflicting_id = event.clone();
        conflicting_id.data = serde_json::json!({ "value": 2 });
        let error = db
            .audit_events()
            .append(Some("agent-a"), &conflicting_id)
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("conflicting audit event content"));

        let error = db
            .audit_events()
            .append(Some("agent-b"), &event)
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("conflicting audit event agent identity"));

        let mut conflicting_sequence =
            AuditEvent::legacy("fixture", serde_json::json!({ "value": 3 }));
        conflicting_sequence.event_seq = first.event_seq;
        conflicting_sequence.event_log_epoch = first.event_log_epoch.clone();
        let error = db
            .transaction(|tx| {
                crate::runtime_db::evidence::import_audit_event_tx(
                    tx,
                    Some("agent-a"),
                    &conflicting_sequence,
                )
            })
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("conflicting audit event identity"));

        let mut foreign_epoch = AuditEvent::legacy("fixture", serde_json::json!({ "value": 4 }));
        foreign_epoch.id = "event-foreign-epoch".into();
        foreign_epoch.event_seq = first.event_seq + 1;
        foreign_epoch.event_log_epoch = "epoch-from-another-runtime".into();
        let error = db
            .transaction(|tx| {
                crate::runtime_db::evidence::import_audit_event_tx(
                    tx,
                    Some("agent-a"),
                    &foreign_epoch,
                )
            })
            .unwrap_err();
        assert!(error.to_string().contains("does not match runtime epoch"));

        let mut imported = AuditEvent::legacy("fixture", serde_json::json!({ "value": 4 }));
        imported.id = "event-imported".into();
        imported.event_seq = first.event_seq + 1;
        db.transaction(|tx| {
            crate::runtime_db::evidence::import_audit_event_tx(tx, Some("agent-a"), &imported)
        })?;
        let persisted = db
            .audit_events()
            .page_after(Some("agent-a"), first.event_seq, 10)?;
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].event_log_epoch, first.event_log_epoch);

        let mut conflicting_import_id = imported.clone();
        conflicting_import_id.event_seq += 1;
        let error = db
            .transaction(|tx| {
                crate::runtime_db::evidence::import_audit_event_tx(
                    tx,
                    Some("agent-a"),
                    &conflicting_import_id,
                )
            })
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("conflicting audit event sequence"));
        Ok(())
    }

    #[test]
    fn runtime_db_context_episode_anchors_schema_replaces_legacy_episode_table() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let connection = db.connection()?;
        assert!(!table_exists(&connection, "context_episodes")?);
        let mut statement = connection.prepare("PRAGMA table_info(context_episode_anchors)")?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        assert!(!columns.iter().any(|column| column == "summary"));
        assert!(columns.iter().any(|column| column == "payload_json"));
        Ok(())
    }

    #[test]
    fn runtime_db_migration_drops_unreleased_context_episodes_table() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        {
            let connection = open_connection(&db_path)?;
            connection.execute_batch(
                r#"
CREATE TABLE schema_migrations (
  version INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  applied_at TEXT NOT NULL
);
CREATE TABLE storage_domains (
  domain TEXT PRIMARY KEY,
  schema_version INTEGER NOT NULL,
  import_status TEXT NOT NULL,
  canonical_source TEXT NOT NULL,
  source_checkpoint_json TEXT,
  imported_at TEXT,
  updated_at TEXT NOT NULL
);
CREATE TABLE context_episodes (
  episode_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  workspace_id TEXT NOT NULL,
  work_item_id TEXT,
  boundary_reason TEXT NOT NULL,
  start_turn_index INTEGER NOT NULL,
  end_turn_index INTEGER NOT NULL,
  started_at TEXT NOT NULL,
  ended_at TEXT NOT NULL,
  summary TEXT NOT NULL,
  payload_json TEXT NOT NULL
);
CREATE INDEX idx_context_episodes_agent_turn
  ON context_episodes(agent_id, end_turn_index);
CREATE INDEX idx_context_episodes_work_item
  ON context_episodes(work_item_id);
INSERT INTO context_episodes (
  episode_id, agent_id, workspace_id, boundary_reason,
  start_turn_index, end_turn_index, started_at, ended_at, summary, payload_json
) VALUES (
  'episode-old', 'default', 'agent_home', 'hard_turn_cap',
  1, 2, '2026-06-10T00:00:00Z', '2026-06-10T00:01:00Z',
  'legacy summary', '{}'
);
INSERT INTO storage_domains (
  domain, schema_version, import_status, canonical_source, updated_at
) VALUES (
  'context_episodes', 1, 'complete', 'db', '2026-06-10T00:01:00Z'
);
"#,
            )?;
            for migration in &MIGRATIONS[..12] {
                connection.execute(
                    "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
                    (
                        migration.version,
                        migration.name,
                        Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                    ),
                )?;
            }
            mark_migration_applied(&connection, "canonical_work_item_focus")?;
        }

        RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let connection = open_connection(&db_path)?;

        assert!(!table_exists(&connection, "context_episodes")?);
        assert!(table_exists(&connection, "context_episode_anchors")?);
        let old_domain_count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM storage_domains WHERE domain = 'context_episodes'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(old_domain_count, 0);
        Ok(())
    }

    #[test]
    fn runtime_db_migration_is_idempotent() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        RuntimeDb::open_and_migrate(&db_path, &lock_path)?;

        let connection = open_connection(&db_path)?;
        let count: i64 =
            connection.query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })?;
        assert_eq!(count, max_known_migration_version());
        assert_eq!(
            current_schema_version(&connection)?,
            max_known_migration_version()
        );
        Ok(())
    }

    #[test]
    fn scheduler_recovery_open_preserves_the_previous_release_schema() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        {
            let mut connection = open_connection(&db_path)?;
            migrate_through(&mut connection, 46)?;
        }

        let recovery_db = RuntimeDb::open_for_scheduler_recovery(&db_path, &lock_path)?;
        assert_eq!(recovery_db.current_schema_version()?, 46);
        for table in RETIRED_SCHEDULER_TABLES {
            assert!(
                table_exists(&recovery_db.connection()?, table)?,
                "recovery open must not remove retired table {table}"
            );
        }
        drop(recovery_db);

        RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        assert_eq!(
            current_schema_version(&open_connection(&db_path)?)?,
            max_known_migration_version()
        );
        Ok(())
    }

    #[test]
    fn scheduler_recovery_open_rejects_older_schemas() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        {
            let mut connection = open_connection(&db_path)?;
            migrate_through(&mut connection, 45)?;
        }

        let error = RuntimeDb::open_for_scheduler_recovery(&db_path, &lock_path)
            .expect_err("schema 45 is too old for the schema 46 recovery contract");
        assert!(
            error.to_string().contains(&format!(
                "supports runtime db schemas {} through {}, found 45",
                RETIRED_SCHEDULER_SCHEMA_PREDECESSOR,
                max_known_migration_version()
            )),
            "{error:#}"
        );
        Ok(())
    }

    #[test]
    fn migration_47_upgrades_v0_31_1_schema_and_preserves_scheduler_audit_evidence() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        {
            let mut connection = open_connection(&db_path)?;
            migrate_through(&mut connection, 46)?;
            connection.execute_batch(
                r#"
INSERT INTO scheduler_work_demands (
  agent_id, work_item_id, metadata_revision, scheduling_generation, status,
  status_reference_id, capabilities_json, locks_json, locality, cost_class,
  payload_json, updated_at
) VALUES (
  'agent-a', 'work-a', 1, 1, 'terminal',
  NULL, '[]', '[]', 'runtime', 'default',
  '{"retired":"work-demand"}', '2026-08-16T00:00:00Z'
);
INSERT INTO scheduler_waits (
  agent_id, wait_id, owner_kind, owner_id, owner_work_item_id, current_generation,
  payload_json, updated_at
) VALUES (
  'agent-a', 'wait-a', 'work_item', 'work-a', 'work-a', 1,
  '{"retired":"wait"}', '2026-08-16T00:00:00Z'
);
INSERT INTO scheduler_wait_generations (
  agent_id, wait_id, generation, owner_kind, owner_id, owner_work_item_id, lifecycle_state,
  trigger_id, trigger_generation, consuming_activation_id,
  payload_json, created_at, updated_at
) VALUES (
  'agent-a', 'wait-a', 1, 'work_item', 'work-a', 'work-a', 'active',
  NULL, NULL, NULL,
  '{"retired":"wait-generation"}',
  '2026-08-16T00:00:00Z', '2026-08-16T00:00:00Z'
);
INSERT INTO scheduler_agent_dispatch (
  agent_id, dispatch_kind, wait_id, wait_generation,
  dispatch_revision, updated_at
) VALUES (
  'agent-a', 'awaiting', 'wait-a', 1,
  1, '2026-08-16T00:00:00Z'
);
INSERT INTO scheduler_activations (
  agent_id, activation_id, authority_id, owner_kind, owner_id, work_item_id,
  admitted_generation, admission_kind, recovery_for_activation_id, wait_id,
  wait_generation, lifecycle_state, idempotency_key, payload_json,
  created_at, updated_at
) VALUES (
  'agent-a', 'activation-a', 'authority-a', 'work_item', 'work-a', 'work-a',
  1, 'scheduling', NULL, NULL,
  NULL, 'settled', 'activation-a', '{"audit":"activation"}',
  '2026-08-16T00:00:00Z', '2026-08-16T00:01:00Z'
);
INSERT INTO scheduler_activation_settlements (
  agent_id, settlement_id, activation_id, payload_json, created_at
) VALUES (
  'agent-a', 'settlement-a', 'activation-a',
  '{"audit":"settlement"}', '2026-08-16T00:01:00Z'
);
INSERT INTO scheduler_continuation_admissions (
  agent_id, admission_id, settlement_id, completed_work_item_id,
  caller_work_item_id, expected_caller_generation, admitted_caller_generation,
  payload_json, created_at
) VALUES (
  'agent-a', 'admission-a', 'settlement-a', 'work-a',
  'work-caller', 1, 2, '{"audit":"continuation"}',
  '2026-08-16T00:02:00Z'
);
INSERT INTO scheduler_activation_sources (
  agent_id, activation_id, source_kind, source_identity, payload_json, created_at
) VALUES (
  'agent-a', 'activation-a', 'internal_followup', 'source-a',
  '{"audit":"source"}', '2026-08-16T00:00:00Z'
);
INSERT INTO scheduler_activation_inputs (
  agent_id, attachment_id, activation_id, owner_kind, owner_id,
  expected_admitted_generation, expected_dispatch_revision, message_id,
  turn_id, boundary, round, payload_json, created_at
) VALUES (
  'agent-a', 'attachment-a', 'activation-a', 'work_item', 'work-a',
  1, 0, 'message-a', 'turn-a', 'initial', 0,
  '{"audit":"input"}', '2026-08-16T00:00:00Z'
);
INSERT INTO scheduler_protocol_command_results (
  agent_id, command_kind, command_identity, canonical_schema_version,
  payload_hash, decision, conflict_kind, conflict_code, result_references_json,
  pre_state_fence_json, post_state_fence_json, outcome_json, created_at
) VALUES (
  'agent-a', 'settle_activation', 'command-a', 1,
  'hash-a', 'applied', NULL, NULL, '[]',
  '{}', '{}', '{"audit":"command"}', '2026-08-16T00:01:00Z'
);
INSERT INTO scheduler_protocol_command_conflict_attempts (
  partition_kind, partition_key, command_kind, command_identity,
  canonical_schema_version, existing_payload_hash, incoming_payload_hash,
  conflict_kind, conflict_code, created_at
) VALUES (
  'agent', 'agent-a', 'settle_activation', 'command-a',
  1, 'hash-a', 'hash-b', 'identity', 'payload_mismatch',
  '2026-08-16T00:02:00Z'
);
INSERT INTO scheduler_rollout_command_results (
  command_kind, command_identity, canonical_schema_version, payload_hash,
  decision, conflict_kind, conflict_code, result_references_json,
  pre_state_fence_json, post_state_fence_json, outcome_json, created_at
) VALUES (
  'install_manifest', 'rollout-a', 1, 'hash-rollout',
  'applied', NULL, NULL, '[]',
  '{}', '{}', '{"audit":"rollout"}', '2026-08-16T00:03:00Z'
);
"#,
            )?;
        }

        RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        RuntimeDb::open_and_migrate(&db_path, &lock_path)?;

        let connection = open_connection(&db_path)?;
        assert_eq!(
            current_schema_version(&connection)?,
            max_known_migration_version()
        );
        for table in RETIRED_SCHEDULER_TABLES {
            assert!(
                !table_exists(&connection, table)?,
                "retired scheduler table {table} must be removed"
            );
        }
        for (table, payload_column, expected_payload) in [
            (
                "scheduler_activations",
                "payload_json",
                r#"{"audit":"activation"}"#,
            ),
            (
                "scheduler_activation_settlements",
                "payload_json",
                r#"{"audit":"settlement"}"#,
            ),
            (
                "scheduler_continuation_admissions",
                "payload_json",
                r#"{"audit":"continuation"}"#,
            ),
            (
                "scheduler_activation_sources",
                "payload_json",
                r#"{"audit":"source"}"#,
            ),
            (
                "scheduler_activation_inputs",
                "payload_json",
                r#"{"audit":"input"}"#,
            ),
            (
                "scheduler_protocol_command_results",
                "outcome_json",
                r#"{"audit":"command"}"#,
            ),
            (
                "scheduler_rollout_command_results",
                "outcome_json",
                r#"{"audit":"rollout"}"#,
            ),
        ] {
            let payload: String = connection.query_row(
                &format!("SELECT {payload_column} FROM {table}"),
                [],
                |row| row.get(0),
            )?;
            assert_eq!(payload, expected_payload, "changed evidence in {table}");
        }
        let conflict_count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM scheduler_protocol_command_conflict_attempts
             WHERE existing_payload_hash = 'hash-a'
               AND incoming_payload_hash = 'hash-b'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(conflict_count, 1);
        for table in RETAINED_SCHEDULER_AUDIT_TABLES {
            let foreign_key_count: i64 = connection.query_row(
                &format!("SELECT COUNT(*) FROM pragma_foreign_key_list('{table}')"),
                [],
                |row| row.get(0),
            )?;
            assert_eq!(
                foreign_key_count, 0,
                "retained audit table {table} must not depend on retired schema"
            );
        }
        let foreign_key_violation_count: i64 =
            connection.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })?;
        assert_eq!(foreign_key_violation_count, 0);
        Ok(())
    }

    #[test]
    fn migration_47_fails_closed_for_unsettled_canonical_execution_state() -> Result<()> {
        for (active_state_sql, recovery_sql, expected_count) in [
            (
                "INSERT INTO execution_protocol_attempts (
                   agent_id, attempt_id, lifecycle_state, source_identity_json,
                   source_generation, recovery_of_attempt_id, terminal_outcome_id, payload_json
                 ) VALUES (
                   'agent-a', 'attempt-a', 'open', '{}',
                   1, NULL, NULL, '{}'
                 )",
                "UPDATE execution_protocol_attempts
                 SET lifecycle_state = 'interrupted'
                 WHERE agent_id = 'agent-a' AND attempt_id = 'attempt-a'",
                "open_execution_attempts=1",
            ),
            (
                "INSERT INTO execution_protocol_work_items (
                   agent_id, work_item_id, source_revision, generation,
                   lifecycle_state, payload_json
                 ) VALUES (
                   'agent-a', 'work-a', 1, 1, 'in_flight', '{}'
                 )",
                "UPDATE execution_protocol_work_items
                 SET lifecycle_state = 'paused'
                 WHERE agent_id = 'agent-a' AND work_item_id = 'work-a'",
                "in_flight_execution_work_items=1",
            ),
            (
                "INSERT INTO queue_entries (
                   message_id, agent_id, priority, status,
                   created_at, updated_at, payload_json
                 ) VALUES (
                   'message-a', 'agent-a', 'normal', 'dequeued',
                   '2026-08-16T00:00:00Z', '2026-08-16T00:00:00Z', '{}'
                 )",
                "UPDATE queue_entries
                 SET status = 'processed', updated_at = '2026-08-16T00:01:00Z'
                 WHERE message_id = 'message-a'",
                "dequeued_queue_entries=1",
            ),
        ] {
            let (_temp_dir, db_path, lock_path) = temp_paths()?;
            {
                let mut connection = open_connection(&db_path)?;
                migrate_through(&mut connection, 46)?;
                connection.execute(active_state_sql, [])?;
            }

            let error = RuntimeDb::open_and_migrate(&db_path, &lock_path)
                .expect_err("migration must reject unsettled canonical execution state");
            assert!(error.to_string().contains(expected_count), "{error:#}");
            assert!(
                error
                    .to_string()
                    .contains("run `holon debug scheduler-recovery --all-affected`"),
                "{error:#}"
            );
            assert!(
                error.to_string().contains("affected_agents=agent-a"),
                "{error:#}"
            );

            let connection = open_connection(&db_path)?;
            assert_eq!(current_schema_version(&connection)?, 46);
            for table in RETIRED_SCHEDULER_TABLES {
                assert!(
                    table_exists(&connection, table)?,
                    "failed migration must retain retired table {table}"
                );
            }
            connection.execute(recovery_sql, [])?;
            drop(connection);

            RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
            let connection = open_connection(&db_path)?;
            assert_eq!(
                current_schema_version(&connection)?,
                max_known_migration_version()
            );
            for table in RETIRED_SCHEDULER_TABLES {
                assert!(
                    !table_exists(&connection, table)?,
                    "migration must remove retired table {table} after recovery reaches a fixed point"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn schema_47_scheduler_cleanup_fallback_preserves_input_and_reaches_fixed_point() -> Result<()>
    {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        prepare_pre_execution_protocol_claim(
            &db,
            "agent-a",
            "work-a",
            "message-a",
            "activation-a",
        )?;
        let original_message = db
            .messages()
            .recent(Some("agent-a"), usize::MAX)?
            .into_iter()
            .find(|message| message.id == "message-a")
            .expect("operator message exists");
        drop(db);
        downgrade_before_execution_protocol(&db_path)?;
        restore_pre_execution_protocol_work_demand(&db_path, "agent-a", "work-a")?;
        {
            let mut connection = open_connection(&db_path)?;
            migrate_through(&mut connection, 46)?;
            connection.execute_batch(
                "UPDATE execution_protocol_attempts
                 SET lifecycle_state = 'interrupted'
                 WHERE agent_id = 'agent-a' AND attempt_id = 'activation-a';
                 UPDATE execution_protocol_work_items
                 SET lifecycle_state = 'runnable'
                 WHERE agent_id = 'agent-a' AND work_item_id = 'work-a';
                 UPDATE queue_entries
                 SET status = 'quarantined'
                 WHERE agent_id = 'agent-a' AND message_id = 'message-a';",
            )?;
            migrate_through(&mut connection, 47)?;
            connection.execute_batch(
                "UPDATE execution_protocol_attempts
                 SET lifecycle_state = 'open'
                 WHERE agent_id = 'agent-a' AND attempt_id = 'activation-a';
                 UPDATE execution_protocol_work_items
                 SET lifecycle_state = 'in_flight'
                 WHERE agent_id = 'agent-a' AND work_item_id = 'work-a';
                 UPDATE queue_entries
                 SET status = 'dequeued'
                 WHERE agent_id = 'agent-a' AND message_id = 'message-a';
                 DROP TRIGGER audit_events_projection_verification_insert;
                 DROP TRIGGER audit_events_projection_verification_update;
                 DROP TABLE observer_sync_capability_verifications;
                 DROP TABLE agent_identity_reservations;",
            )?;
        }

        let db = RuntimeDb::open_for_scheduler_recovery(&db_path, &lock_path)?;
        assert_eq!(db.current_schema_version()?, 47);
        assert_eq!(db.retired_scheduler_cleanup_inventory()?.blockers.len(), 3);
        let result = db.apply_retired_scheduler_cleanup_fallback("agent-a")?;
        assert_eq!(result.protected_message_ids, vec!["message-a".to_string()]);
        assert!(result.recovery_message_id.is_some());
        assert!(db.retired_scheduler_cleanup_inventory()?.is_fixed_point());

        let messages = db.messages().recent(Some("agent-a"), usize::MAX)?;
        assert_eq!(
            messages.iter().find(|message| message.id == "message-a"),
            Some(&original_message)
        );
        let recovery_message = messages
            .iter()
            .find(|message| Some(message.id.as_str()) == result.recovery_message_id.as_deref())
            .expect("fallback enqueues an agent recovery event");
        assert_eq!(
            recovery_message.kind,
            crate::types::MessageKind::InternalFollowup
        );
        let crate::types::MessageBody::Json { value } = &recovery_message.body else {
            panic!("recovery event must use a typed JSON body");
        };
        assert_eq!(
            value["actions"]
                .as_array()
                .expect("recovery actions are an array")
                .len(),
            result.actions.len()
        );
        assert_eq!(
            value["actions"]
                .as_array()
                .expect("recovery actions are an array")
                .last()
                .and_then(|action| action["kind"].as_str()),
            Some("enqueue_agent_recovery_event")
        );
        assert_eq!(
            db.queue_entries()
                .latest("message-a")?
                .expect("original queue entry exists")
                .status,
            QueueEntryStatus::Quarantined
        );
        assert_eq!(
            db.queue_entries()
                .latest(&recovery_message.id)?
                .expect("recovery queue entry exists")
                .status,
            QueueEntryStatus::Queued
        );
        let state = db
            .transitions()
            .load_execution_protocol_state_if_initialized("agent-a")?
            .expect("execution partition exists");
        assert!(matches!(
            state
                .attempts
                .get("activation-a")
                .expect("attempt remains durable")
                .state,
            crate::domain::execution_protocol::ExecutionAttemptState::Interrupted
        ));
        assert!(matches!(
            state
                .work_items
                .get("work-a")
                .expect("WorkItem remains durable")
                .state,
            WorkItemExecutionState::Runnable {
                recovery_ref: Some(_),
                ..
            }
        ));
        drop(db);

        RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        assert_eq!(
            current_schema_version(&open_connection(&db_path)?)?,
            max_known_migration_version()
        );
        Ok(())
    }

    #[test]
    fn retired_scheduler_cleanup_fallback_refuses_missing_source_message() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        prepare_pre_execution_protocol_claim(
            &db,
            "agent-a",
            "work-a",
            "message-a",
            "activation-a",
        )?;
        drop(db);
        downgrade_before_execution_protocol(&db_path)?;
        restore_pre_execution_protocol_work_demand(&db_path, "agent-a", "work-a")?;
        {
            let mut connection = open_connection(&db_path)?;
            migrate_through(&mut connection, 46)?;
            connection.execute("DELETE FROM messages WHERE message_id = 'message-a'", [])?;
        }

        let db = RuntimeDb::open_for_scheduler_recovery(&db_path, &lock_path)?;
        let before = db.retired_scheduler_cleanup_inventory()?;
        let error = db
            .apply_retired_scheduler_cleanup_fallback("agent-a")
            .expect_err("fallback must protect missing operator input");
        assert!(
            error
                .to_string()
                .contains("cannot preserve source message message-a"),
            "{error:#}"
        );
        assert_eq!(db.retired_scheduler_cleanup_inventory()?, before);
        Ok(())
    }

    #[test]
    fn retired_scheduler_cleanup_fallback_handles_partial_execution_facts() -> Result<()> {
        for (case, damage_sql, expected_blockers) in [
            (
                "missing_attempt",
                "DELETE FROM execution_protocol_attempts
                 WHERE agent_id = 'agent-a' AND attempt_id = 'activation-a'",
                2,
            ),
            (
                "isolated_queue",
                "DELETE FROM execution_protocol_attempts
                 WHERE agent_id = 'agent-a' AND attempt_id = 'activation-a';
                 DELETE FROM execution_protocol_work_items
                 WHERE agent_id = 'agent-a' AND work_item_id = 'work-a'",
                1,
            ),
            (
                "bare_in_flight_work_item",
                "DELETE FROM execution_protocol_attempts
                 WHERE agent_id = 'agent-a' AND attempt_id = 'activation-a';
                 DELETE FROM queue_entries
                 WHERE agent_id = 'agent-a' AND message_id = 'message-a'",
                1,
            ),
        ] {
            let (_temp_dir, db_path, lock_path) = temp_paths()?;
            let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
            prepare_pre_execution_protocol_claim(
                &db,
                "agent-a",
                "work-a",
                "message-a",
                "activation-a",
            )?;
            drop(db);
            downgrade_before_execution_protocol(&db_path)?;
            restore_pre_execution_protocol_work_demand(&db_path, "agent-a", "work-a")?;
            {
                let mut connection = open_connection(&db_path)?;
                migrate_through(&mut connection, 46)?;
                connection.execute_batch(damage_sql)?;
            }

            let db = RuntimeDb::open_for_scheduler_recovery(&db_path, &lock_path)?;
            assert_eq!(
                db.retired_scheduler_cleanup_inventory()?.blockers.len(),
                expected_blockers,
                "{case}"
            );
            let result = db.apply_retired_scheduler_cleanup_fallback("agent-a")?;
            assert!(
                !result.actions.is_empty(),
                "{case} must produce typed recovery actions"
            );
            assert!(
                db.retired_scheduler_cleanup_inventory()?.is_fixed_point(),
                "{case} must reach the migration fixed point"
            );
            assert!(
                result.recovery_message_id.is_some(),
                "{case} must wake the agent for reconciliation"
            );
            drop(db);

            RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
            assert_eq!(
                current_schema_version(&open_connection(&db_path)?)?,
                max_known_migration_version(),
                "{case}"
            );
        }
        Ok(())
    }

    #[test]
    fn retired_scheduler_cleanup_fallback_repairs_nonreciprocal_execution_facts() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        prepare_pre_execution_protocol_claim(
            &db,
            "agent-a",
            "work-a",
            "message-a",
            "activation-a",
        )?;
        drop(db);
        downgrade_before_execution_protocol(&db_path)?;
        restore_pre_execution_protocol_work_demand(&db_path, "agent-a", "work-a")?;
        {
            let mut connection = open_connection(&db_path)?;
            migrate_through(&mut connection, 46)?;
            let payload = connection.query_row(
                "SELECT payload_json
                 FROM execution_protocol_work_items
                 WHERE agent_id = 'agent-a' AND work_item_id = 'work-a'",
                [],
                |row| row.get::<_, String>(0),
            )?;
            let mut record: WorkItemExecutionRecord = serde_json::from_str(&payload)?;
            record.state = WorkItemExecutionState::InFlight {
                generation: record.generation(),
                attempt_id: "missing-attempt".into(),
            };
            connection.execute(
                "UPDATE execution_protocol_work_items
                 SET payload_json = ?1
                 WHERE agent_id = 'agent-a' AND work_item_id = 'work-a'",
                [serde_json::to_string(&record)?],
            )?;
        }

        let db = RuntimeDb::open_for_scheduler_recovery(&db_path, &lock_path)?;
        assert_eq!(db.retired_scheduler_cleanup_inventory()?.blockers.len(), 3);
        let result = db.apply_retired_scheduler_cleanup_fallback("agent-a")?;
        assert!(result.actions.iter().any(|action| {
            action.kind
                == crate::runtime_db::RetiredSchedulerFallbackActionKind::ResumeInFlightExecutionWorkItem
        }));
        assert!(result.actions.iter().any(|action| {
            action.kind
                == crate::runtime_db::RetiredSchedulerFallbackActionKind::InterruptOpenExecutionAttempt
        }));
        assert!(db.retired_scheduler_cleanup_inventory()?.is_fixed_point());
        drop(db);

        RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        assert_eq!(
            current_schema_version(&open_connection(&db_path)?)?,
            max_known_migration_version()
        );
        Ok(())
    }

    #[test]
    fn retired_scheduler_cleanup_fallback_converges_for_multiple_agents() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        for suffix in ["a", "b"] {
            prepare_pre_execution_protocol_claim(
                &db,
                &format!("agent-{suffix}"),
                &format!("work-{suffix}"),
                &format!("message-{suffix}"),
                &format!("activation-{suffix}"),
            )?;
        }
        drop(db);
        downgrade_before_execution_protocol(&db_path)?;
        for suffix in ["a", "b"] {
            restore_pre_execution_protocol_work_demand(
                &db_path,
                &format!("agent-{suffix}"),
                &format!("work-{suffix}"),
            )?;
        }
        {
            let mut connection = open_connection(&db_path)?;
            migrate_through(&mut connection, 46)?;
        }

        let db = RuntimeDb::open_for_scheduler_recovery(&db_path, &lock_path)?;
        let before = db.retired_scheduler_cleanup_inventory()?;
        assert_eq!(
            before.affected_agents(),
            vec!["agent-a".to_string(), "agent-b".to_string()]
        );
        assert_eq!(before.blockers.len(), 6);
        for agent_id in before.affected_agents() {
            let first = db.apply_retired_scheduler_cleanup_fallback(&agent_id)?;
            assert!(!first.actions.is_empty());
        }
        assert!(db.retired_scheduler_cleanup_inventory()?.is_fixed_point());
        for agent_id in ["agent-a", "agent-b"] {
            let message_count = db.messages().recent(Some(agent_id), usize::MAX)?.len();
            let second = db.apply_retired_scheduler_cleanup_fallback(agent_id)?;
            assert!(
                second.actions.is_empty(),
                "repeated fallback must be a fixed point for {agent_id}"
            );
            assert!(second.recovery_message_id.is_none());
            assert_eq!(
                db.messages().recent(Some(agent_id), usize::MAX)?.len(),
                message_count,
                "fixed-point apply must not enqueue another recovery event for {agent_id}"
            );
        }
        drop(db);

        RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        assert_eq!(
            current_schema_version(&open_connection(&db_path)?)?,
            max_known_migration_version()
        );
        Ok(())
    }

    #[test]
    fn retired_scheduler_cleanup_fallbacks_are_atomic_across_agents() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        for suffix in ["a", "b"] {
            prepare_pre_execution_protocol_claim(
                &db,
                &format!("agent-{suffix}"),
                &format!("work-{suffix}"),
                &format!("message-{suffix}"),
                &format!("activation-{suffix}"),
            )?;
        }
        drop(db);
        downgrade_before_execution_protocol(&db_path)?;
        for suffix in ["a", "b"] {
            restore_pre_execution_protocol_work_demand(
                &db_path,
                &format!("agent-{suffix}"),
                &format!("work-{suffix}"),
            )?;
        }
        {
            let mut connection = open_connection(&db_path)?;
            migrate_through(&mut connection, 46)?;
            connection.execute("DELETE FROM messages WHERE message_id = 'message-b'", [])?;
        }

        let db = RuntimeDb::open_for_scheduler_recovery(&db_path, &lock_path)?;
        let before = db.retired_scheduler_cleanup_inventory()?;
        let agent_a_messages = db.messages().recent(Some("agent-a"), usize::MAX)?;
        let error = db
            .apply_retired_scheduler_cleanup_fallbacks(&before.affected_agents())
            .expect_err("one unprotected operator input must abort the global fallback");
        assert!(
            error
                .to_string()
                .contains("cannot preserve source message message-b"),
            "{error:#}"
        );
        assert_eq!(db.retired_scheduler_cleanup_inventory()?, before);
        assert_eq!(
            db.messages().recent(Some("agent-a"), usize::MAX)?,
            agent_a_messages,
            "the first agent must roll back when a later agent cannot be recovered"
        );
        Ok(())
    }

    #[test]
    fn execution_protocol_migration_backfills_active_canonical_claim() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        prepare_pre_execution_protocol_claim(
            &db,
            "agent-a",
            "work-a",
            "message-a",
            "activation-a",
        )?;
        drop(db);
        downgrade_before_execution_protocol(&db_path)?;
        restore_pre_execution_protocol_work_demand(&db_path, "agent-a", "work-a")?;

        let mut connection = open_connection(&db_path)?;
        apply_migration(&mut connection, &MIGRATIONS[40])?;
        let payload_json: String = connection.query_row(
            "SELECT payload_json
             FROM execution_protocol_attempts
             WHERE agent_id = 'agent-a' AND attempt_id = 'activation-a'",
            [],
            |row| row.get(0),
        )?;
        let attempt: ExecutionAttempt = serde_json::from_str(&payload_json)?;
        assert_eq!(
            attempt.state,
            crate::domain::execution_protocol::ExecutionAttemptState::Open
        );
        assert_eq!(attempt.source_message_id.as_deref(), Some("message-a"));
        assert_eq!(
            attempt.binding,
            crate::domain::execution_protocol::ExecutionBinding::WorkItem {
                work_item_id: "work-a".into(),
            }
        );
        assert_eq!(attempt.admitted_fences.source_revision, 1);
        assert_eq!(attempt.admitted_fences.work_item_source_revision, Some(1));
        assert_eq!(attempt.admitted_fences.work_item_generation, Some(1));
        assert_eq!(attempt.admitted_fences.agent_control_revision, 1);
        assert_eq!(attempt.admitted_fences.host_registry_revision, 1);
        assert_eq!(current_schema_version(&connection)?, 41);
        Ok(())
    }

    #[test]
    fn execution_protocol_migration_fails_closed_when_claim_evidence_is_missing() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        prepare_pre_execution_protocol_claim(
            &db,
            "agent-a",
            "work-a",
            "message-a",
            "activation-a",
        )?;
        drop(db);
        downgrade_before_execution_protocol(&db_path)?;
        restore_pre_execution_protocol_work_demand(&db_path, "agent-a", "work-a")?;
        open_connection(&db_path)?
            .execute("DELETE FROM messages WHERE message_id = 'message-a'", [])?;

        let mut connection = open_connection(&db_path)?;
        let error = apply_migration(&mut connection, &MIGRATIONS[40])
            .expect_err("migration must reject an unreconstructable active claim");
        assert!(
            error
                .to_string()
                .contains("requires exactly one message evidence row"),
            "{error:#}"
        );
        assert_eq!(current_schema_version(&connection)?, 40);
        assert!(!table_exists(&connection, "execution_protocol_attempts")?);
        Ok(())
    }

    #[test]
    fn runtime_sequence_migration_repairs_duplicate_historical_values() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let old_event_log_epoch;
        {
            let connection = open_connection(&db_path)?;
            old_event_log_epoch = connection.query_row(
                "SELECT value FROM runtime_metadata WHERE key = 'event_log_epoch'",
                [],
                |row| row.get::<_, String>(0),
            )?;
            connection.execute_batch(
                "DELETE FROM schema_migrations WHERE version = 26;
                 DROP INDEX idx_audit_events_agent_event_seq_unique;
                 DROP INDEX idx_audit_events_host_event_seq_unique;
                 DROP INDEX idx_messages_agent_message_seq_unique;
                 DROP INDEX idx_transcript_entries_agent_transcript_seq_unique;
                 DROP TABLE runtime_sequences;",
            )?;
            let created_at = Utc::now();
            for (id, sequence, text) in [
                ("message-a", 7_i64, "a"),
                ("message-b", 7_i64, "b"),
                ("message-c", 9_i64, "c"),
                ("message-d", 9_i64, "d"),
                ("message-max", 12_i64, "max"),
            ] {
                let mut message = MessageEnvelope::new(
                    "agent-a",
                    crate::types::MessageKind::OperatorPrompt,
                    crate::types::MessageOrigin::Operator {
                        actor_id: None,
                        actor_display_name: None,
                    },
                    crate::types::AuthorityClass::OperatorInstruction,
                    crate::types::Priority::Normal,
                    crate::types::MessageBody::Text { text: text.into() },
                );
                message.id = id.into();
                message.created_at = created_at;
                message.message_seq = Some(sequence as u64);
                let payload_json = serde_json::to_string(&message)?;
                connection.execute(
                    "INSERT INTO messages (
                        evidence_id, agent_id, message_id, message_seq, created_at, kind,
                        content_hash, payload_json
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        message.id,
                        message.agent_id,
                        message.id,
                        sequence,
                        timestamp(message.created_at),
                        "operator_prompt",
                        content_hash(&payload_json),
                        payload_json,
                    ],
                )?;
            }
            for (id, sequence) in [
                ("transcript-a", 3_i64),
                ("transcript-b", 3_i64),
                ("transcript-c", 5_i64),
                ("transcript-d", 5_i64),
                ("transcript-max", 8_i64),
            ] {
                let mut entry = TranscriptEntry::new(
                    "agent-a",
                    TranscriptEntryKind::IncomingMessage,
                    None,
                    None,
                    serde_json::json!({"text": id}),
                );
                entry.id = id.into();
                entry.created_at = created_at;
                entry.transcript_seq = Some(sequence as u64);
                let payload_json = serde_json::to_string(&entry)?;
                connection.execute(
                    "INSERT INTO transcript_entries (
                        evidence_id, agent_id, transcript_seq, created_at, kind,
                        content_hash, payload_json
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        entry.id,
                        entry.agent_id,
                        sequence,
                        timestamp(entry.created_at),
                        "user",
                        content_hash(&payload_json),
                        payload_json,
                    ],
                )?;
            }
            for (id, sequence) in [
                ("audit-a", 11_i64),
                ("audit-b", 11_i64),
                ("audit-c", 12_i64),
                ("audit-d", 12_i64),
                ("audit-max", 15_i64),
            ] {
                let mut event = AuditEvent::legacy("test", serde_json::json!({"id": id}));
                event.id = id.into();
                event.created_at = created_at;
                event.event_seq = sequence as u64;
                connection.execute(
                    "INSERT INTO audit_events (
                        audit_event_id, event_seq, agent_id, kind, created_at, data_json
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        event.id,
                        sequence,
                        "agent-a",
                        event.kind,
                        timestamp(event.created_at),
                        serde_json::to_string(&event)?,
                    ],
                )?;
            }
            let mut unaffected = MessageEnvelope::new(
                "agent-b",
                crate::types::MessageKind::OperatorPrompt,
                crate::types::MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                crate::types::AuthorityClass::OperatorInstruction,
                crate::types::Priority::Normal,
                crate::types::MessageBody::Text {
                    text: "unaffected".into(),
                },
            );
            unaffected.id = "message-unaffected".into();
            unaffected.created_at = created_at;
            unaffected.message_seq = Some(42);
            let payload_json = serde_json::to_string(&unaffected)?;
            connection.execute(
                "INSERT INTO messages (
                    evidence_id, agent_id, message_id, message_seq, created_at, kind,
                    content_hash, payload_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    unaffected.id,
                    unaffected.agent_id,
                    unaffected.id,
                    42_i64,
                    timestamp(unaffected.created_at),
                    "operator_prompt",
                    content_hash(&payload_json),
                    payload_json,
                ],
            )?;
        }

        RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let connection = open_connection(&db_path)?;
        for (table, id_column, id, sequence_column, expected) in [
            ("messages", "evidence_id", "message-a", "message_seq", 1_i64),
            ("messages", "evidence_id", "message-b", "message_seq", 2_i64),
            ("messages", "evidence_id", "message-c", "message_seq", 3_i64),
            ("messages", "evidence_id", "message-d", "message_seq", 4_i64),
            (
                "messages",
                "evidence_id",
                "message-max",
                "message_seq",
                5_i64,
            ),
            (
                "transcript_entries",
                "evidence_id",
                "transcript-a",
                "transcript_seq",
                1_i64,
            ),
            (
                "transcript_entries",
                "evidence_id",
                "transcript-b",
                "transcript_seq",
                2_i64,
            ),
            (
                "transcript_entries",
                "evidence_id",
                "transcript-max",
                "transcript_seq",
                5_i64,
            ),
            (
                "audit_events",
                "audit_event_id",
                "audit-a",
                "event_seq",
                1_i64,
            ),
            (
                "audit_events",
                "audit_event_id",
                "audit-b",
                "event_seq",
                2_i64,
            ),
            (
                "audit_events",
                "audit_event_id",
                "audit-max",
                "event_seq",
                5_i64,
            ),
            (
                "messages",
                "evidence_id",
                "message-unaffected",
                "message_seq",
                42_i64,
            ),
        ] {
            let sql = format!("SELECT {sequence_column} FROM {table} WHERE {id_column} = ?1");
            let actual: i64 = connection.query_row(&sql, [id], |row| row.get(0))?;
            assert_eq!(actual, expected, "{table}.{id}");
        }
        for (table, id, sequence_column, payload_column, expected) in [
            (
                "messages",
                "message-b",
                "message_seq",
                "payload_json",
                2_i64,
            ),
            (
                "transcript_entries",
                "transcript-b",
                "transcript_seq",
                "payload_json",
                2_i64,
            ),
            ("audit_events", "audit-b", "event_seq", "data_json", 2_i64),
        ] {
            let id_column = if table == "audit_events" {
                "audit_event_id"
            } else {
                "evidence_id"
            };
            let sql = format!(
                "SELECT {sequence_column}, {payload_column} FROM {table} WHERE {id_column} = ?1"
            );
            let (sequence, payload_json): (i64, String) =
                connection.query_row(&sql, [id], |row| Ok((row.get(0)?, row.get(1)?)))?;
            let payload: serde_json::Value = serde_json::from_str(&payload_json)?;
            assert_eq!(sequence, expected);
            assert_eq!(payload[sequence_column], expected);
            if table != "audit_events" {
                let stored_hash: String = connection.query_row(
                    &format!("SELECT content_hash FROM {table} WHERE {id_column} = ?1"),
                    [id],
                    |row| row.get(0),
                )?;
                assert_eq!(stored_hash, content_hash(&payload_json));
            }
        }
        for (domain, expected) in [
            ("audit_event", 5_i64),
            ("message", 5_i64),
            ("transcript", 5_i64),
        ] {
            let head: i64 = connection.query_row(
                "SELECT last_value FROM runtime_sequences
                 WHERE domain = ?1 AND scope_key = 'agent:agent-a'",
                [domain],
                |row| row.get(0),
            )?;
            assert_eq!(head, expected, "{domain}");
        }
        let new_event_log_epoch: String = connection.query_row(
            "SELECT value FROM runtime_metadata WHERE key = 'event_log_epoch'",
            [],
            |row| row.get(0),
        )?;
        assert_ne!(new_event_log_epoch, old_event_log_epoch);
        let audit_epochs = connection
            .prepare("SELECT data_json FROM audit_events ORDER BY event_seq")?
            .query_map([], |row| row.get::<_, String>(0))?
            .map(|row| {
                let payload: serde_json::Value = serde_json::from_str(&row?)?;
                Ok(payload["event_log_epoch"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string())
            })
            .collect::<Result<Vec<_>>>()?;
        assert!(audit_epochs
            .iter()
            .all(|epoch| epoch == &new_event_log_epoch));

        drop(connection);
        RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let connection = open_connection(&db_path)?;
        let reopened_epoch: String = connection.query_row(
            "SELECT value FROM runtime_metadata WHERE key = 'event_log_epoch'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(reopened_epoch, new_event_log_epoch);
        let reopened_message_sequences = connection
            .prepare(
                "SELECT message_seq FROM messages
                 WHERE agent_id = 'agent-a'
                 ORDER BY message_seq",
            )?
            .query_map([], |row| row.get::<_, i64>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        assert_eq!(reopened_message_sequences, vec![1, 2, 3, 4, 5]);
        Ok(())
    }

    #[test]
    fn runtime_sequence_migration_initializes_head_from_historical_max() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        {
            let connection = open_connection(&db_path)?;
            connection.execute_batch(
                "DELETE FROM schema_migrations WHERE version = 26;
                 DROP INDEX idx_messages_agent_message_seq_unique;
                 DROP TABLE runtime_sequences;",
            )?;
            let mut historical = MessageEnvelope::new(
                "agent-a",
                crate::types::MessageKind::OperatorPrompt,
                crate::types::MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                crate::types::AuthorityClass::OperatorInstruction,
                crate::types::Priority::Normal,
                crate::types::MessageBody::Text {
                    text: "historical".into(),
                },
            );
            historical.id = "historical-message".into();
            historical.message_seq = Some(7);
            connection.execute(
                "INSERT INTO messages (
                    evidence_id, agent_id, message_id, message_seq, created_at,
                    kind, payload_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    historical.id,
                    historical.agent_id,
                    historical.id,
                    7_i64,
                    timestamp(historical.created_at),
                    "operator_prompt",
                    serde_json::to_string(&historical)?,
                ],
            )?;
        }

        let migrated = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let next = MessageEnvelope::new(
            "agent-a",
            crate::types::MessageKind::OperatorPrompt,
            crate::types::MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            crate::types::AuthorityClass::OperatorInstruction,
            crate::types::Priority::Normal,
            crate::types::MessageBody::Text {
                text: "next".into(),
            },
        );
        let next = migrated.messages().append_with_index_changes(&next, &[])?;
        assert_eq!(next.message_seq, Some(8));
        Ok(())
    }

    #[test]
    fn work_item_focus_migration_backfills_single_legacy_focus() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let mut work = WorkItemRecord::new("agent-a", "legacy focus", WorkItemState::Open);
        work.id = "work-legacy-focus".into();
        db.work_items().insert_new(&work)?;
        db.agent_states().upsert(&AgentState::new("agent-a"))?;
        {
            let connection = open_connection(&db_path)?;
            connection.execute_batch(
                "DELETE FROM schema_migrations WHERE version = 27;
                 DROP INDEX idx_agent_states_current_work_item;
                 DROP TRIGGER trg_agent_states_focus_insert;
                 DROP TRIGGER trg_agent_states_focus_update;
                 DROP TRIGGER trg_work_items_preserve_focused_target;
                 DROP TRIGGER trg_work_items_preserve_focused_delete;",
            )?;
            connection.execute(
                "UPDATE work_items SET current_focus = 1 WHERE work_item_id = ?1",
                [&work.id],
            )?;
        }

        let migrated = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let state = migrated
            .agent_states()
            .latest("agent-a")?
            .expect("agent state");
        assert_eq!(
            state.current_work_item_id.as_deref(),
            Some(work.id.as_str())
        );
        let connection = migrated.connection()?;
        let legacy_focus: i64 = connection.query_row(
            "SELECT current_focus FROM work_items WHERE work_item_id = ?1",
            [&work.id],
            |row| row.get(0),
        )?;
        assert_eq!(legacy_focus, 0);
        Ok(())
    }

    #[test]
    fn work_item_focus_migration_rejects_conflicting_facts() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let mut canonical = WorkItemRecord::new("agent-a", "canonical", WorkItemState::Open);
        canonical.id = "work-canonical".into();
        let mut legacy = WorkItemRecord::new("agent-a", "legacy", WorkItemState::Open);
        legacy.id = "work-legacy".into();
        db.work_items().insert_new(&canonical)?;
        db.work_items().insert_new(&legacy)?;
        let mut state = AgentState::new("agent-a");
        state.current_work_item_id = Some(canonical.id.clone());
        db.agent_states().upsert(&state)?;
        {
            let connection = open_connection(&db_path)?;
            connection.execute_batch(
                "DELETE FROM schema_migrations WHERE version = 27;
                 DROP INDEX idx_agent_states_current_work_item;
                 DROP TRIGGER trg_agent_states_focus_insert;
                 DROP TRIGGER trg_agent_states_focus_update;
                 DROP TRIGGER trg_work_items_preserve_focused_target;
                 DROP TRIGGER trg_work_items_preserve_focused_delete;",
            )?;
            connection.execute(
                "UPDATE work_items SET current_focus = 1 WHERE work_item_id = ?1",
                [&legacy.id],
            )?;
        }

        let error = RuntimeDb::open_and_migrate(&db_path, &lock_path).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("conflicting focus facts"), "{text}");
        assert!(text.contains(&canonical.id), "{text}");
        assert!(text.contains(&legacy.id), "{text}");
        Ok(())
    }

    #[test]
    fn work_item_focus_constraints_reject_invalid_targets_and_completion() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let mut own = WorkItemRecord::new("agent-a", "own", WorkItemState::Open);
        own.id = "work-own".into();
        let mut foreign = WorkItemRecord::new("agent-b", "foreign", WorkItemState::Open);
        foreign.id = "work-foreign".into();
        db.work_items().insert_new(&own)?;
        db.work_items().insert_new(&foreign)?;
        let mut state = AgentState::new("agent-a");
        state.current_work_item_id = Some(foreign.id.clone());
        assert!(db.agent_states().upsert(&state).is_err());
        state.current_work_item_id = Some("work-missing".into());
        assert!(db.agent_states().upsert(&state).is_err());
        state.current_work_item_id = Some(own.id.clone());
        db.agent_states().upsert(&state)?;

        let mut completed = own.clone();
        completed.revision = 2;
        completed.state = WorkItemState::Completed;
        completed.updated_at = Utc::now();
        assert!(db
            .work_items()
            .update_expected(&completed, own.revision)
            .is_err());
        Ok(())
    }

    #[test]
    fn runtime_db_migration_compacts_queue_entries_to_current_view() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        {
            let connection = open_connection(&db_path)?;
            connection.execute_batch(
                r#"
CREATE TABLE schema_migrations (
  version INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  applied_at TEXT NOT NULL
);
CREATE TABLE queue_entries (
  message_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  priority TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  PRIMARY KEY (message_id, status)
);
INSERT INTO queue_entries (
  message_id, agent_id, priority, status, created_at, updated_at, payload_json
) VALUES
  (
    'message-1', 'agent-a', 'normal', 'queued',
    '2026-06-17T00:00:00.000Z', '2026-06-17T00:00:00.000Z',
    '{"message_id":"message-1","agent_id":"agent-a","priority":"normal","status":"queued","created_at":"2026-06-17T00:00:00.000Z","updated_at":"2026-06-17T00:00:00.000Z"}'
  ),
  (
    'message-1', 'agent-a', 'normal', 'processed',
    '2026-06-17T00:00:00.000Z', '2026-06-17T00:01:00.000Z',
    '{"message_id":"message-1","agent_id":"agent-a","priority":"normal","status":"processed","created_at":"2026-06-17T00:00:00.000Z","updated_at":"2026-06-17T00:01:00.000Z"}'
  ),
  (
    'message-2', 'agent-a', 'interject', 'queued',
    '2026-06-17T00:02:00.000Z', '2026-06-17T00:02:00.000Z',
    '{"message_id":"message-2","agent_id":"agent-a","priority":"interject","status":"queued","created_at":"2026-06-17T00:02:00.000Z","updated_at":"2026-06-17T00:02:00.000Z"}'
  );
"#,
            )?;
            for migration in &MIGRATIONS[..17] {
                connection.execute(
                    "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
                    (
                        migration.version,
                        migration.name,
                        Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                    ),
                )?;
            }
            mark_migration_applied(&connection, "canonical_work_item_focus")?;
        }

        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let entries = db.queue_entries().latest_all()?;
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|entry| {
            entry.message_id == "message-1" && entry.status == QueueEntryStatus::Processed
        }));
        assert!(entries.iter().any(|entry| {
            entry.message_id == "message-2" && entry.status == QueueEntryStatus::Queued
        }));

        let connection = open_connection(&db_path)?;
        let duplicate = connection.execute(
            "INSERT INTO queue_entries (
                message_id, agent_id, priority, status, created_at, updated_at, payload_json
             ) VALUES (
                'message-2', 'agent-a', 'interject', 'dequeued',
                '2026-06-17T00:02:00.000Z', '2026-06-17T00:03:00.000Z', '{}'
             )",
            [],
        );
        assert!(duplicate.is_err());

        Ok(())
    }

    #[test]
    fn runtime_db_read_connection_opens_while_external_writer_holds_lock() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let mut external = open_connection(&db_path)?;
        let _external_write = external.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let read = db.connection()?;
        let value: i64 = read.query_row("SELECT 1", [], |row| row.get(0))?;
        assert_eq!(value, 1);
        Ok(())
    }

    #[test]
    fn runtime_db_async_append_retries_temporarily_locked_writer() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let mut external = open_connection(&db_path)?;
        let external_write = external.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let writer = db.clone();
        let (attempt_tx, attempt_rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || -> Result<()> {
            attempt_tx
                .send(())
                .map_err(|_| anyhow!("failed to signal writer attempt"))?;
            writer.append(|tx| {
                insert_audit_event_tx(
                    tx,
                    Some("agent-a"),
                    &AuditEvent::legacy(
                        "runtime_db_locked_retry",
                        serde_json::json!({ "source": "test" }),
                    ),
                )
            })
        });

        attempt_rx
            .recv_timeout(Duration::from_secs(1))
            .context("writer thread did not start")?;
        std::thread::sleep(Duration::from_millis(100));
        drop(external_write);

        handle
            .join()
            .map_err(|_| anyhow!("writer thread panicked"))??;
        wait_until(
            || {
                let events = db.audit_events().recent(Some("agent-a"), 1)?;
                Ok(events.len() == 1 && events[0].kind == "runtime_db_locked_retry")
            },
            "locked async append retry",
        )?;
        Ok(())
    }

    #[test]
    fn runtime_db_clones_serialize_concurrent_writes_through_shared_writer() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let mut handles = Vec::new();

        for index in 0..8 {
            let writer = db.clone();
            handles.push(std::thread::spawn(move || -> Result<()> {
                writer.audit_events().append(
                    Some("agent-a"),
                    &AuditEvent::legacy(
                        format!("runtime_db_concurrent_write_{index}"),
                        serde_json::json!({ "index": index }),
                    ),
                )?;
                Ok(())
            }));
        }

        for handle in handles {
            handle
                .join()
                .map_err(|_| anyhow!("writer thread panicked"))??;
        }

        wait_until(
            || {
                let connection = db.connection()?;
                let count: i64 = connection.query_row(
                    "SELECT COUNT(*) FROM audit_events WHERE agent_id = 'agent-a'",
                    [],
                    |row| row.get(0),
                )?;
                Ok(count == 8)
            },
            "concurrent queued writes",
        )?;
        Ok(())
    }

    #[test]
    fn runtime_db_transactions_are_queued_across_db_instances() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let first = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let second = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;

        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let first_writer = first.clone();
        let first_handle = std::thread::spawn(move || -> Result<()> {
            first_writer.transaction(|tx| {
                entered_tx
                    .send(())
                    .map_err(|_| anyhow!("failed to signal first write"))?;
                release_rx
                    .recv_timeout(Duration::from_secs(2))
                    .context("release signal not received")?;
                insert_audit_event_tx(
                    tx,
                    Some("agent-a"),
                    &AuditEvent::legacy("runtime_db_queue_first", serde_json::json!({})),
                )
            })
        });

        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .context("first write did not enter transaction")?;

        let second_writer = second.clone();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let second_handle = std::thread::spawn(move || -> Result<()> {
            second_writer.transaction(|tx| {
                insert_audit_event_tx(
                    tx,
                    Some("agent-a"),
                    &AuditEvent::legacy("runtime_db_queue_second", serde_json::json!({})),
                )
            })?;
            done_tx
                .send(())
                .map_err(|_| anyhow!("failed to signal second write"))?;
            Ok(())
        });

        assert!(
            done_rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "second write committed before the first queued write completed"
        );
        release_tx
            .send(())
            .map_err(|_| anyhow!("failed to release first write"))?;

        first_handle
            .join()
            .map_err(|_| anyhow!("first writer thread panicked"))??;
        second_handle
            .join()
            .map_err(|_| anyhow!("second writer thread panicked"))??;

        let events = second.audit_events().recent(Some("agent-a"), 2)?;
        let kinds = events
            .iter()
            .map(|event| event.kind.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec!["runtime_db_queue_first", "runtime_db_queue_second"]
        );
        Ok(())
    }

    #[test]
    fn runtime_sequences_are_atomic_across_db_instances_and_scopes() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let first = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let second = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;

        let event_a = first.audit_events().append(
            Some("agent-a"),
            &AuditEvent::legacy("event-a", serde_json::json!({})),
        )?;
        let event_b = second.audit_events().append(
            Some("agent-a"),
            &AuditEvent::legacy("event-b", serde_json::json!({})),
        )?;
        let host_event = second.audit_events().append(
            None,
            &AuditEvent::legacy("host-event", serde_json::json!({})),
        )?;
        assert_eq!((event_a.event_seq, event_b.event_seq), (1, 2));
        assert_eq!(host_event.event_seq, 1);

        let message_a = MessageEnvelope::new(
            "agent-a",
            crate::types::MessageKind::OperatorPrompt,
            crate::types::MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            crate::types::AuthorityClass::OperatorInstruction,
            crate::types::Priority::Normal,
            crate::types::MessageBody::Text { text: "a".into() },
        );
        let message_b = MessageEnvelope::new(
            "agent-a",
            crate::types::MessageKind::OperatorPrompt,
            crate::types::MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            crate::types::AuthorityClass::OperatorInstruction,
            crate::types::Priority::Normal,
            crate::types::MessageBody::Text { text: "b".into() },
        );
        let appended_a = first
            .messages()
            .append_with_index_changes(&message_a, &[])?;
        let appended_b = second
            .messages()
            .append_with_index_changes(&message_b, &[])?;
        assert_eq!(appended_a.message_seq, Some(1));
        assert_eq!(appended_b.message_seq, Some(2));

        let transcript_a = TranscriptEntry::new(
            "agent-a",
            TranscriptEntryKind::AssistantRound,
            Some(1),
            None,
            serde_json::json!({ "text": "a" }),
        );
        let transcript_b = TranscriptEntry::new(
            "agent-a",
            TranscriptEntryKind::AssistantRound,
            Some(2),
            None,
            serde_json::json!({ "text": "b" }),
        );
        assert_eq!(
            first
                .transcript_entries()
                .append(&transcript_a)?
                .transcript_seq,
            Some(1)
        );
        assert_eq!(
            second
                .transcript_entries()
                .append(&transcript_b)?
                .transcript_seq,
            Some(2)
        );
        Ok(())
    }

    #[test]
    fn runtime_db_append_accepts_without_waiting_for_commit() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;

        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let blocker = db.clone();
        let handle = std::thread::spawn(move || -> Result<()> {
            blocker.transaction(|tx| {
                entered_tx
                    .send(())
                    .map_err(|_| anyhow!("failed to signal blocking write"))?;
                release_rx
                    .recv_timeout(Duration::from_secs(2))
                    .context("release signal not received")?;
                insert_audit_event_tx(
                    tx,
                    Some("agent-a"),
                    &AuditEvent::legacy("runtime_db_append_blocker", serde_json::json!({})),
                )
            })
        });
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .context("blocking write did not enter transaction")?;

        db.append(|tx| {
            insert_audit_event_tx(
                tx,
                Some("agent-a"),
                &AuditEvent::legacy("runtime_db_append_async", serde_json::json!({})),
            )
        })?;
        assert_eq!(db.audit_events().recent(Some("agent-a"), 10)?.len(), 0);

        release_tx
            .send(())
            .map_err(|_| anyhow!("failed to release blocking write"))?;
        handle
            .join()
            .map_err(|_| anyhow!("blocking writer thread panicked"))??;

        let started_at = Instant::now();
        loop {
            let events = db.audit_events().recent(Some("agent-a"), 10)?;
            if events.len() == 2 {
                let kinds = events
                    .iter()
                    .map(|event| event.kind.as_str())
                    .collect::<Vec<_>>();
                assert_eq!(
                    kinds,
                    vec!["runtime_db_append_blocker", "runtime_db_append_async"]
                );
                return Ok(());
            }
            if started_at.elapsed() > Duration::from_secs(2) {
                bail!("queued append did not commit");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn runtime_db_migration_drops_unreleased_working_memory_deltas() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        {
            let connection = open_connection(&db_path)?;
            connection.execute_batch(
                r#"
CREATE TABLE working_memory_deltas (
  memory_delta_id TEXT PRIMARY KEY,
  from_revision INTEGER NOT NULL,
  to_revision INTEGER NOT NULL,
  created_at_turn INTEGER NOT NULL,
  reason TEXT NOT NULL,
  created_at TEXT NOT NULL,
  payload_json TEXT NOT NULL
);
"#,
            )?;
            connection.execute(
                "INSERT INTO working_memory_deltas (
                    memory_delta_id, from_revision, to_revision, created_at_turn,
                    reason, created_at, payload_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                (
                    "memory-delta-1-2-7",
                    1_i64,
                    2_i64,
                    7_i64,
                    "task_rejoined",
                    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                    "{}",
                ),
            )?;
            connection.execute(
                "INSERT OR REPLACE INTO storage_domains (
                    domain, schema_version, import_status, canonical_source, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                (
                    "working_memory_deltas",
                    max_known_migration_version(),
                    "complete",
                    "db",
                    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                ),
            )?;
            connection.execute("DELETE FROM schema_migrations WHERE version = 14", [])?;
        }

        RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let connection = open_connection(&db_path)?;
        assert!(!table_exists(&connection, "working_memory_deltas")?);
        assert!(!table_exists(
            &connection,
            "working_memory_deltas_unscoped_legacy"
        )?);
        let domain_count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM storage_domains WHERE domain = ?1",
            ["working_memory_deltas"],
            |row| row.get(0),
        )?;
        assert_eq!(domain_count, 0);
        Ok(())
    }

    #[test]
    fn runtime_db_schema_version_comes_from_schema_migrations() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let connection = open_connection(&db_path)?;
        connection.execute(
            "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
            (
                max_known_migration_version() + 1,
                "future_test",
                Utc::now().to_rfc3339(),
            ),
        )?;

        assert_eq!(
            current_schema_version(&connection)?,
            max_known_migration_version() + 1
        );
        Ok(())
    }

    #[test]
    fn runtime_db_migration_name_mismatch_fails() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        {
            let connection = open_connection(&db_path)?;
            ensure_migration_table(&connection)?;
            connection.execute(
                "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
                (1_i64, "wrong_name", Utc::now().to_rfc3339()),
            )?;
        }

        let error = RuntimeDb::open_and_migrate(&db_path, &lock_path).unwrap_err();
        assert!(error.to_string().contains("name mismatch"));
        Ok(())
    }

    #[test]
    fn runtime_db_accepts_released_message_search_migration_name() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        {
            let connection = open_connection(&db_path)?;
            ensure_migration_table(&connection)?;
            for migration in MIGRATIONS
                .iter()
                .filter(|migration| migration.version <= 14)
            {
                connection.execute(
                    "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
                    (migration.version, migration.name, Utc::now().to_rfc3339()),
                )?;
            }
            connection.execute(
                "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
                (15_i64, "message_search_index", Utc::now().to_rfc3339()),
            )?;
            mark_migration_applied(&connection, "canonical_work_item_focus")?;
        }

        RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let connection = open_connection(&db_path)?;
        assert_eq!(
            current_schema_version(&connection)?,
            max_known_migration_version()
        );
        assert!(!table_exists(&connection, "message_search_index")?);
        Ok(())
    }

    #[test]
    fn runtime_db_migration_rejects_newer_schema_version() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        {
            let connection = open_connection(&db_path)?;
            ensure_migration_table(&connection)?;
            connection.execute(
                "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
                (
                    max_known_migration_version() + 1,
                    "future_test",
                    Utc::now().to_rfc3339(),
                ),
            )?;
        }

        let error = RuntimeDb::open_and_migrate(&db_path, &lock_path).unwrap_err();
        assert!(error
            .to_string()
            .contains("newer than this binary supports"));
        Ok(())
    }

    #[test]
    fn runtime_db_recent_payloads_keep_evidence_id_ascending_after_reverse() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let created_at = Utc::now();
        let mut later_id = BriefRecord::new("agent-a", BriefKind::Result, "later id", None, None);
        later_id.id = "brief-b".into();
        later_id.created_at = created_at;
        let mut earlier_id =
            BriefRecord::new("agent-a", BriefKind::Result, "earlier id", None, None);
        earlier_id.id = "brief-a".into();
        earlier_id.created_at = created_at;

        db.evidence().append_brief(&later_id)?;
        db.evidence().append_brief(&earlier_id)?;

        wait_until(
            || Ok(db.evidence().recent_briefs("agent-a", 2)?.len() == 2),
            "recent brief writes",
        )?;
        let records = db.evidence().recent_briefs("agent-a", 2)?;
        assert_eq!(
            records
                .into_iter()
                .map(|record| record.id)
                .collect::<Vec<_>>(),
            vec!["brief-a", "brief-b"]
        );
        Ok(())
    }

    #[test]
    fn queue_claim_allows_only_one_consumer_for_queued_message() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let now = Utc::now();
        let record = QueueEntryRecord {
            message_id: "message-1".into(),
            agent_id: "agent-a".into(),
            priority: crate::types::Priority::Normal,
            status: QueueEntryStatus::Queued,
            created_at: now,
            updated_at: now,
        };
        db.queue_entries().upsert(&record)?;

        let mut claim = record.clone();
        claim.status = QueueEntryStatus::Dequeued;
        claim.updated_at = now + chrono::Duration::seconds(1);
        assert!(db.queue_entries().try_claim_queued_message(&claim)?);

        let mut duplicate_claim = claim.clone();
        duplicate_claim.updated_at = now + chrono::Duration::seconds(2);
        assert!(!db
            .queue_entries()
            .try_claim_queued_message(&duplicate_claim)?);

        let latest = db.queue_entries().latest_all()?;
        assert_eq!(latest.len(), 1);
        assert_eq!(latest[0].message_id, "message-1");
        assert_eq!(latest[0].status, QueueEntryStatus::Dequeued);
        Ok(())
    }

    #[test]
    fn queue_entries_table_uses_message_id_as_current_state_key() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let connection = db.connection()?;
        let primary_key_columns: Vec<String> = {
            let mut statement = connection.prepare("PRAGMA table_info(queue_entries)")?;
            let rows = statement.query_map([], |row| {
                Ok((row.get::<_, String>(1)?, row.get::<_, i64>(5)?))
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
                .into_iter()
                .filter_map(|(name, pk)| (pk > 0).then_some(name))
                .collect()
        };
        assert_eq!(primary_key_columns, vec!["message_id"]);

        let now = Utc::now();
        let queued = QueueEntryRecord {
            message_id: "message-current".into(),
            agent_id: "agent-a".into(),
            priority: crate::types::Priority::Normal,
            status: QueueEntryStatus::Queued,
            created_at: now,
            updated_at: now,
        };
        let mut processed = queued.clone();
        processed.status = QueueEntryStatus::Processed;
        processed.updated_at = now + chrono::Duration::seconds(1);
        db.queue_entries().upsert(&queued)?;
        db.queue_entries().upsert(&processed)?;

        let rows: i64 =
            connection.query_row("SELECT COUNT(*) FROM queue_entries", [], |row| row.get(0))?;
        assert_eq!(rows, 1);
        assert!(db.queue_entries().queued_for_agent("agent-a")?.is_empty());
        Ok(())
    }

    #[test]
    fn message_repository_orders_null_message_seq_as_legacy_history() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let base = Utc::now();

        let mut sequenced_1 = MessageEnvelope::new(
            "agent-a",
            crate::types::MessageKind::OperatorPrompt,
            crate::types::MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            crate::types::AuthorityClass::OperatorInstruction,
            crate::types::Priority::Normal,
            crate::types::MessageBody::Text { text: "one".into() },
        );
        sequenced_1.id = "msg-seq-1".into();
        sequenced_1.message_seq = Some(1);
        sequenced_1.created_at = base;

        let mut sequenced_2 = sequenced_1.clone();
        sequenced_2.id = "msg-seq-2".into();
        sequenced_2.message_seq = Some(2);
        sequenced_2.created_at = base + chrono::Duration::seconds(1);

        let mut legacy_without_sequence = sequenced_1.clone();
        legacy_without_sequence.id = "msg-legacy".into();
        legacy_without_sequence.message_seq = None;
        legacy_without_sequence.created_at = base + chrono::Duration::seconds(2);

        db.messages().upsert_many(&[
            sequenced_1.clone(),
            sequenced_2.clone(),
            legacy_without_sequence.clone(),
        ])?;

        let all_ids = db
            .messages()
            .all(Some("agent-a"))?
            .into_iter()
            .map(|message| message.id)
            .collect::<Vec<_>>();
        assert_eq!(all_ids, vec!["msg-legacy", "msg-seq-1", "msg-seq-2"]);

        let recent_ids = db
            .messages()
            .recent(Some("agent-a"), 2)?
            .into_iter()
            .map(|message| message.id)
            .collect::<Vec<_>>();
        assert_eq!(recent_ids, vec!["msg-seq-1", "msg-seq-2"]);
        Ok(())
    }

    #[test]
    fn queue_claim_rejects_message_whose_current_status_is_terminal() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let now = Utc::now();
        let queued = QueueEntryRecord {
            message_id: "message-1".into(),
            agent_id: "agent-a".into(),
            priority: crate::types::Priority::Normal,
            status: QueueEntryStatus::Queued,
            created_at: now,
            updated_at: now,
        };
        let mut processed = queued.clone();
        processed.status = QueueEntryStatus::Processed;
        processed.updated_at = now + chrono::Duration::seconds(1);
        db.queue_entries().upsert(&queued)?;
        db.queue_entries().upsert(&processed)?;

        let mut claim = queued;
        claim.status = QueueEntryStatus::Dequeued;
        claim.updated_at = now + chrono::Duration::seconds(2);
        assert!(!db.queue_entries().try_claim_queued_message(&claim)?);

        Ok(())
    }

    #[test]
    fn queue_head_no_progress_budget_is_atomic_and_survives_restart() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let now = Utc::now();
        let queued = QueueEntryRecord {
            message_id: "message-no-progress".into(),
            agent_id: "agent-a".into(),
            priority: crate::types::Priority::Normal,
            status: QueueEntryStatus::Queued,
            created_at: now,
            updated_at: now,
        };
        let mut quarantined = queued.clone();
        quarantined.status = QueueEntryStatus::Quarantined;
        quarantined.updated_at = now + chrono::Duration::seconds(1);
        let mut current_state = AgentState::new("agent-a");
        current_state.pending = 1;
        let mut terminal_state = current_state.clone();
        terminal_state.pending = 0;
        db.queue_entries().upsert(&queued)?;
        db.agent_states().upsert(&current_state)?;

        let command = |fault| QueueHeadNoProgressCommand {
            agent_id: "agent-a".into(),
            expected: queued.clone(),
            quarantined: quarantined.clone(),
            agent_state: AgentStateMutation {
                expected: Some(Box::new(current_state.clone())),
                record: Box::new(terminal_state.clone()),
            },
            reason: "canonical_claim_contended".into(),
            scenario_class: Some("lifecycle_external_nudge".into()),
            max_attempts: 3,
            fault,
        };

        for fault in [
            TransitionFaultPoint::AfterValidation,
            TransitionFaultPoint::AfterCanonicalWrites,
            TransitionFaultPoint::AfterAuditWrites,
            TransitionFaultPoint::BeforeCommit,
        ] {
            let error = db
                .transitions()
                .commit_queue_head_no_progress(&command(Some(fault)))
                .unwrap_err();
            assert!(error
                .to_string()
                .contains("injected runtime transition fault"));
            assert_eq!(
                db.queue_entries().latest(&queued.message_id)?,
                Some(queued.clone())
            );
            let no_progress_rows: i64 = db.connection()?.query_row(
                "SELECT COUNT(*) FROM queue_head_no_progress WHERE message_id = ?1",
                [&queued.message_id],
                |row| row.get(0),
            )?;
            assert_eq!(no_progress_rows, 0);
        }

        assert_eq!(
            db.transitions()
                .commit_queue_head_no_progress(&command(None))?
                .unwrap()
                .outcome,
            QueueHeadNoProgressOutcome::BoundedDefer {
                attempt: 1,
                max_attempts: 3,
            }
        );
        drop(db);

        let reopened = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        assert_eq!(
            reopened
                .transitions()
                .commit_queue_head_no_progress(&command(None))?
                .unwrap()
                .outcome,
            QueueHeadNoProgressOutcome::BoundedDefer {
                attempt: 2,
                max_attempts: 3,
            }
        );
        assert_eq!(
            reopened
                .transitions()
                .commit_queue_head_no_progress(&command(None))?
                .unwrap()
                .outcome,
            QueueHeadNoProgressOutcome::Quarantined {
                attempt: 3,
                max_attempts: 3,
            }
        );
        assert_eq!(
            reopened.queue_entries().latest(&queued.message_id)?,
            Some(quarantined)
        );
        assert_eq!(
            reopened.agent_states().latest("agent-a")?,
            Some(terminal_state)
        );
        let persisted: (u32, u32, String) = reopened.connection()?.query_row(
            "SELECT attempts, max_attempts, status
             FROM queue_head_no_progress WHERE message_id = ?1",
            [&queued.message_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(persisted, (3, 3, "quarantined".into()));
        Ok(())
    }

    #[test]
    fn queue_terminal_state_rejects_late_updates_and_allows_identical_retries() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let now = Utc::now();
        let terminal = QueueEntryRecord {
            message_id: "message-terminal".into(),
            agent_id: "agent-a".into(),
            priority: crate::types::Priority::Normal,
            status: QueueEntryStatus::Processed,
            created_at: now,
            updated_at: now + chrono::Duration::seconds(1),
        };
        db.queue_entries().upsert(&terminal)?;

        let mut identical_retry = terminal.clone();
        identical_retry.updated_at = now + chrono::Duration::seconds(2);
        db.queue_entries().upsert(&identical_retry)?;
        assert_eq!(db.queue_entries().latest_all()?, vec![terminal.clone()]);

        let mut late_active = terminal.clone();
        late_active.status = QueueEntryStatus::Interrupted;
        late_active.updated_at = now + chrono::Duration::seconds(3);
        let error = db.queue_entries().upsert(&late_active).unwrap_err();
        let conflict = error
            .downcast_ref::<RuntimeStateTransitionConflict>()
            .expect("late queue update should return a typed conflict");
        assert_eq!(conflict.domain(), "queue entry");
        assert_eq!(conflict.record_id(), terminal.message_id);
        assert_eq!(conflict.existing_status(), "processed");
        assert_eq!(conflict.incoming_status(), "interrupted");

        let mut conflicting_terminal = terminal.clone();
        conflicting_terminal.status = QueueEntryStatus::Dropped;
        conflicting_terminal.updated_at = now + chrono::Duration::seconds(4);
        assert!(db
            .queue_entries()
            .upsert(&conflicting_terminal)
            .unwrap_err()
            .downcast_ref::<RuntimeStateTransitionConflict>()
            .is_some());
        assert_eq!(db.queue_entries().latest_all()?, vec![terminal]);
        Ok(())
    }

    #[test]
    fn wait_terminal_state_rejects_late_updates_and_allows_identical_retries() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let now = Utc::now();
        let terminal = WaitConditionRecord {
            id: "wait-terminal".into(),
            agent_id: "agent-a".into(),
            work_item_id: Some("work-1".into()),
            status: WaitConditionStatus::Resolved,
            kind: WaitConditionKind::Task,
            source: Some("task".into()),
            subject_ref: Some("task-1".into()),
            waiting_for: "task result".into(),
            wake_sources: Vec::new(),
            continuation: None,
            created_at: now,
            updated_at: now + chrono::Duration::seconds(1),
            expires_at: None,
            resolved_at: Some(now + chrono::Duration::seconds(1)),
            cancelled_at: None,
            turn_id: Some("turn-1".into()),
            trigger_message_id: None,
            triggered_at: None,
        };
        db.wait_conditions().upsert(&terminal)?;
        let persisted_terminal = db.wait_conditions().latest_all()?;

        let mut identical_retry = terminal.clone();
        identical_retry.updated_at = now + chrono::Duration::seconds(2);
        db.wait_conditions().upsert(&identical_retry)?;
        assert_eq!(db.wait_conditions().latest_all()?, persisted_terminal);

        let mut late_active = terminal.clone();
        late_active.status = WaitConditionStatus::Active;
        late_active.updated_at = now + chrono::Duration::seconds(3);
        late_active.resolved_at = None;
        let error = db.wait_conditions().upsert(&late_active).unwrap_err();
        let conflict = error
            .downcast_ref::<RuntimeStateTransitionConflict>()
            .expect("late wait update should return a typed conflict");
        assert_eq!(conflict.domain(), "wait condition");
        assert_eq!(conflict.record_id(), terminal.id);
        assert_eq!(conflict.existing_status(), "resolved");
        assert_eq!(conflict.incoming_status(), "active");

        let mut conflicting_terminal = terminal.clone();
        conflicting_terminal.status = WaitConditionStatus::Cancelled;
        conflicting_terminal.updated_at = now + chrono::Duration::seconds(4);
        conflicting_terminal.resolved_at = None;
        conflicting_terminal.cancelled_at = Some(now + chrono::Duration::seconds(4));
        assert!(db
            .wait_conditions()
            .upsert(&conflicting_terminal)
            .unwrap_err()
            .downcast_ref::<RuntimeStateTransitionConflict>()
            .is_some());
        assert_eq!(db.wait_conditions().latest_all()?, persisted_terminal);
        Ok(())
    }

    #[test]
    fn wait_owner_uniqueness_migration_cancels_older_unresolved_rows() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        {
            let connection = db.connection()?;
            connection.execute_batch(
                "DROP INDEX wait_conditions_unresolved_agent_owner;
                 DROP INDEX wait_conditions_unresolved_work_item_owner;
                 DELETE FROM schema_migrations WHERE version = 44;",
            )?;
        }
        let now = Utc::now();
        let older = WaitConditionRecord {
            id: "wait-owner-older".into(),
            agent_id: "agent-a".into(),
            work_item_id: Some("work-owner".into()),
            status: WaitConditionStatus::Active,
            kind: WaitConditionKind::Operator,
            source: Some("test".into()),
            subject_ref: None,
            waiting_for: "older wait".into(),
            wake_sources: Vec::new(),
            continuation: None,
            created_at: now,
            updated_at: now,
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
            turn_id: None,
            trigger_message_id: None,
            triggered_at: None,
        };
        let mut newer = older.clone();
        newer.id = "wait-owner-newer".into();
        newer.status = WaitConditionStatus::Triggered;
        newer.waiting_for = "newer wait".into();
        newer.created_at = now + chrono::Duration::seconds(1);
        newer.updated_at = newer.created_at;
        newer.trigger_message_id = Some("message-owner-newer".into());
        newer.triggered_at = Some(newer.created_at);
        db.wait_conditions().upsert(&older)?;
        db.wait_conditions().upsert(&newer)?;

        let migration = MIGRATIONS
            .iter()
            .find(|migration| migration.version == 44)
            .expect("migration 44");
        let mut connection = db.connection()?;
        apply_migration(&mut connection, migration)?;

        let waits = db.wait_conditions().latest_all()?;
        assert_eq!(
            waits
                .iter()
                .find(|wait| wait.id == older.id)
                .map(|wait| &wait.status),
            Some(&WaitConditionStatus::Cancelled)
        );
        assert_eq!(
            waits
                .iter()
                .find(|wait| wait.id == newer.id)
                .map(|wait| &wait.status),
            Some(&WaitConditionStatus::Triggered)
        );
        let mut duplicate = older;
        duplicate.id = "wait-owner-duplicate".into();
        duplicate.created_at = now + chrono::Duration::seconds(2);
        duplicate.updated_at = duplicate.created_at;
        assert!(db.wait_conditions().upsert(&duplicate).is_err());
        Ok(())
    }

    #[test]
    fn wait_protocol_cutover_cancels_history_and_releases_exact_work_item_authority() -> Result<()>
    {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        {
            let connection = db.connection()?;
            connection.execute("DELETE FROM schema_migrations WHERE version = 45", [])?;
        }

        let now = Utc::now();
        let mut owned = WorkItemRecord::new("agent-a", "owned wait", WorkItemState::Open);
        owned.id = "work-cutover-owned".into();
        owned.blocked_by = Some("owned blocker".into());
        owned.updated_at = now;
        db.work_items().insert_new(&owned)?;

        let mut original_changed =
            WorkItemRecord::new("agent-a", "changed wait", WorkItemState::Open);
        original_changed.id = "work-cutover-changed".into();
        original_changed.blocked_by = Some("older blocker".into());
        original_changed.updated_at = now;
        db.work_items().insert_new(&original_changed)?;
        let mut changed = original_changed.clone();
        changed.id = "work-cutover-changed".into();
        changed.revision = 2;
        changed.blocked_by = Some("newer blocker".into());
        changed.updated_at = now + chrono::Duration::seconds(1);
        db.work_items()
            .update_expected(&changed, original_changed.revision)?;

        let owned_wait = WaitConditionRecord {
            id: "wait-cutover-owned".into(),
            agent_id: "agent-a".into(),
            work_item_id: Some(owned.id.clone()),
            status: WaitConditionStatus::Active,
            kind: WaitConditionKind::Operator,
            source: Some("WaitFor".into()),
            subject_ref: None,
            waiting_for: "owned blocker".into(),
            wake_sources: Vec::new(),
            continuation: None,
            created_at: now,
            updated_at: now,
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
            turn_id: Some("turn-cutover-owned".into()),
            trigger_message_id: None,
            triggered_at: None,
        };
        let changed_wait = WaitConditionRecord {
            id: "wait-cutover-changed".into(),
            agent_id: "agent-a".into(),
            work_item_id: Some(changed.id.clone()),
            waiting_for: "older blocker".into(),
            turn_id: Some("turn-cutover-changed".into()),
            ..owned_wait.clone()
        };
        let agent_wait = WaitConditionRecord {
            id: "wait-cutover-agent".into(),
            agent_id: "agent-b".into(),
            work_item_id: None,
            waiting_for: "lifecycle wait".into(),
            turn_id: Some("turn-cutover-agent".into()),
            ..owned_wait.clone()
        };
        db.wait_conditions().upsert(&owned_wait)?;
        db.wait_conditions().upsert(&changed_wait)?;
        db.wait_conditions().upsert(&agent_wait)?;

        let owned_execution = WorkItemExecutionRecord {
            source_revision: owned.revision,
            state: WorkItemExecutionState::Waiting {
                generation: 4,
                wait: WaitReference {
                    wait_id: owned_wait.id.clone(),
                },
            },
        };
        let changed_execution = WorkItemExecutionRecord {
            source_revision: 2,
            state: WorkItemExecutionState::Waiting {
                generation: 7,
                wait: WaitReference {
                    wait_id: "wait-cutover-stale-history".into(),
                },
            },
        };
        {
            let connection = db.connection()?;
            let legacy_attempt = serde_json::json!({
                "attempt_id": "activation:message:message-cutover-trigger",
                "agent_id": "agent-a",
                "source_message_id": "message-cutover-trigger",
                "source": {
                    "identity": {
                        "kind": "triggered_wait",
                        "wait_id": "wait-cutover-history",
                        "wait_generation": 9,
                        "trigger_id": "external-trigger-history",
                        "trigger_generation": 12
                    },
                    "generation": 12
                },
                "binding": {
                    "kind": "work_item",
                    "work_item_id": owned.id
                },
                "provenance": {
                    "origin": "system",
                    "trust": "runtime_instruction",
                    "priority": "next",
                    "correlation_id": null,
                    "causation_id": null
                },
                "admitted_fences": {
                    "source_revision": 12,
                    "work_item_source_revision": owned.revision,
                    "work_item_generation": 4,
                    "rejoin": null,
                    "agent_control_revision": 1,
                    "host_registry_revision": 1
                },
                "state": "settled",
                "run_id": null,
                "turn_id": "turn-cutover-history",
                "recovery_of_attempt_id": null,
                "terminal_outcome_id": null,
                "admitted_at": "2026-08-01T00:00:00Z",
                "terminal_at": "2026-08-01T00:01:00Z"
            });
            connection.execute(
                "INSERT INTO execution_protocol_attempts (
                   agent_id, attempt_id, lifecycle_state,
                   source_identity_json, source_generation,
                   recovery_of_attempt_id, terminal_outcome_id, payload_json
                 ) VALUES (?1, ?2, 'settled', ?3, 12, NULL, NULL, ?4)",
                params![
                    "agent-a",
                    "activation:message:message-cutover-trigger",
                    serde_json::to_string(&legacy_attempt["source"]["identity"])?,
                    serde_json::to_string(&legacy_attempt)?,
                ],
            )?;
            connection.execute(
                "INSERT INTO execution_protocol_work_items (
                   agent_id, work_item_id, source_revision, generation,
                   lifecycle_state, payload_json
                 ) VALUES (?1, ?2, ?3, ?4, 'waiting', ?5)",
                params![
                    "agent-a",
                    owned.id,
                    owned_execution.source_revision as i64,
                    owned_execution.generation() as i64,
                    serde_json::to_string(&owned_execution)?,
                ],
            )?;
            connection.execute(
                "INSERT INTO execution_protocol_work_items (
                   agent_id, work_item_id, source_revision, generation,
                   lifecycle_state, payload_json
                 ) VALUES (?1, ?2, ?3, ?4, 'waiting', ?5)",
                params![
                    "agent-a",
                    changed.id,
                    changed_execution.source_revision as i64,
                    changed_execution.generation() as i64,
                    serde_json::to_string(&changed_execution)?,
                ],
            )?;
        }

        let migration = MIGRATIONS
            .iter()
            .find(|migration| migration.version == 45)
            .expect("migration 45");
        let mut connection = db.connection()?;
        apply_migration(&mut connection, migration)?;

        let waits = db.wait_conditions().latest_all()?;
        for wait_id in [&owned_wait.id, &changed_wait.id, &agent_wait.id] {
            let wait = waits
                .iter()
                .find(|wait| wait.id == *wait_id)
                .expect("cutover wait");
            assert_eq!(wait.status, WaitConditionStatus::Cancelled);
            assert_eq!(
                wait.continuation
                    .as_ref()
                    .and_then(|value| value.get("cancel_reason"))
                    .and_then(serde_json::Value::as_str),
                Some("protocol_cutover")
            );
        }

        let persisted_owned = db.work_items().latest(&owned.id)?.expect("owned WorkItem");
        assert_eq!(persisted_owned.blocked_by, None);
        assert_eq!(persisted_owned.revision, owned.revision + 1);
        let persisted_changed = db
            .work_items()
            .latest(&changed.id)?
            .expect("changed WorkItem");
        assert_eq!(
            persisted_changed.blocked_by.as_deref(),
            Some("newer blocker")
        );
        assert_eq!(persisted_changed.revision, changed.revision);

        let load_execution = |work_item_id: &str| -> Result<WorkItemExecutionRecord> {
            let payload = connection.query_row(
                "SELECT payload_json
                 FROM execution_protocol_work_items
                 WHERE agent_id = 'agent-a' AND work_item_id = ?1",
                [work_item_id],
                |row| row.get::<_, String>(0),
            )?;
            Ok(serde_json::from_str(&payload)?)
        };
        let owned_execution = load_execution(&owned.id)?;
        assert_eq!(owned_execution.source_revision, persisted_owned.revision);
        assert_eq!(
            owned_execution.state,
            WorkItemExecutionState::Runnable {
                generation: 5,
                recovery_ref: Some("protocol_cutover".into()),
            }
        );
        let changed_execution = load_execution(&changed.id)?;
        assert_eq!(
            changed_execution.source_revision,
            persisted_changed.revision
        );
        assert_eq!(
            changed_execution.state,
            WorkItemExecutionState::Runnable {
                generation: 8,
                recovery_ref: Some("protocol_cutover".into()),
            }
        );
        let normalized_attempt_payload = connection.query_row(
            "SELECT payload_json
             FROM execution_protocol_attempts
             WHERE agent_id = 'agent-a'
               AND attempt_id = 'activation:message:message-cutover-trigger'",
            [],
            |row| row.get::<_, String>(0),
        )?;
        let normalized_attempt: ExecutionAttempt =
            serde_json::from_str(&normalized_attempt_payload)?;
        assert_eq!(
            normalized_attempt.source.identity,
            ExecutionSourceIdentity::TriggeredWait {
                wait_id: "wait-cutover-history".into(),
                trigger_message_id: "message-cutover-trigger".into(),
            }
        );
        assert_eq!(
            current_schema_version(&connection)?,
            max_known_migration_version()
        );
        Ok(())
    }

    #[test]
    fn queue_and_wait_terminal_state_survive_restart_and_second_db_handle() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let first = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let second = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let now = Utc::now();
        let queue_terminal = QueueEntryRecord {
            message_id: "message-restart".into(),
            agent_id: "agent-a".into(),
            priority: crate::types::Priority::Normal,
            status: QueueEntryStatus::Aborted,
            created_at: now,
            updated_at: now,
        };
        let wait_terminal = WaitConditionRecord {
            id: "wait-restart".into(),
            agent_id: "agent-a".into(),
            work_item_id: None,
            status: WaitConditionStatus::Expired,
            kind: WaitConditionKind::Timer,
            source: Some("timer".into()),
            subject_ref: Some("timer-1".into()),
            waiting_for: "timer".into(),
            wake_sources: Vec::new(),
            continuation: None,
            created_at: now,
            updated_at: now,
            expires_at: Some(now),
            resolved_at: None,
            cancelled_at: None,
            turn_id: None,
            trigger_message_id: None,
            triggered_at: None,
        };
        first.queue_entries().upsert(&queue_terminal)?;
        first.wait_conditions().upsert(&wait_terminal)?;
        let persisted_queue_terminal = first.queue_entries().latest_all()?;
        let persisted_wait_terminal = first.wait_conditions().latest_all()?;

        let mut queue_late = queue_terminal.clone();
        queue_late.status = QueueEntryStatus::Queued;
        queue_late.updated_at = now + chrono::Duration::seconds(1);
        let mut wait_late = wait_terminal.clone();
        wait_late.status = WaitConditionStatus::Active;
        wait_late.updated_at = now + chrono::Duration::seconds(1);
        assert!(second.queue_entries().upsert(&queue_late).is_err());
        assert!(second.wait_conditions().upsert(&wait_late).is_err());

        drop(first);
        drop(second);
        let reopened = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        assert_eq!(
            reopened.queue_entries().latest_all()?,
            persisted_queue_terminal
        );
        assert_eq!(
            reopened.wait_conditions().latest_all()?,
            persisted_wait_terminal
        );
        Ok(())
    }

    #[test]
    fn queue_and_wait_legacy_import_reject_terminal_regressions() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let now = Utc::now();
        let queue_terminal = QueueEntryRecord {
            message_id: "message-import".into(),
            agent_id: "agent-a".into(),
            priority: crate::types::Priority::Normal,
            status: QueueEntryStatus::Processed,
            created_at: now,
            updated_at: now,
        };
        let mut queue_late = queue_terminal.clone();
        queue_late.status = QueueEntryStatus::Queued;
        queue_late.updated_at = now + chrono::Duration::seconds(1);
        assert!(db
            .queue_entries()
            .import_legacy(vec![queue_terminal.clone(), queue_late])
            .unwrap_err()
            .downcast_ref::<RuntimeStateTransitionConflict>()
            .is_some());
        assert!(db.queue_entries().latest_all()?.is_empty());

        let mut queue_retry = queue_terminal.clone();
        queue_retry.updated_at = now + chrono::Duration::seconds(2);
        db.queue_entries()
            .import_legacy(vec![queue_terminal.clone(), queue_retry])?;
        assert_eq!(db.queue_entries().latest_all()?, vec![queue_terminal]);

        let wait_terminal = WaitConditionRecord {
            id: "wait-import".into(),
            agent_id: "agent-a".into(),
            work_item_id: None,
            status: WaitConditionStatus::Cancelled,
            kind: WaitConditionKind::Operator,
            source: Some("operator".into()),
            subject_ref: None,
            waiting_for: "operator input".into(),
            wake_sources: Vec::new(),
            continuation: None,
            created_at: now,
            updated_at: now,
            expires_at: None,
            resolved_at: None,
            cancelled_at: Some(now),
            turn_id: None,
            trigger_message_id: None,
            triggered_at: None,
        };
        let mut wait_late = wait_terminal.clone();
        wait_late.status = WaitConditionStatus::Active;
        wait_late.updated_at = now + chrono::Duration::seconds(1);
        wait_late.cancelled_at = None;
        assert!(db
            .wait_conditions()
            .import_legacy(vec![wait_terminal.clone(), wait_late])
            .unwrap_err()
            .downcast_ref::<RuntimeStateTransitionConflict>()
            .is_some());
        assert!(db.wait_conditions().latest_all()?.is_empty());

        let mut wait_retry = wait_terminal.clone();
        wait_retry.updated_at = now + chrono::Duration::seconds(2);
        db.wait_conditions()
            .import_legacy(vec![wait_terminal, wait_retry])?;
        let imported_waits = db.wait_conditions().latest_all()?;
        assert_eq!(imported_waits.len(), 1);
        assert_eq!(imported_waits[0].status, WaitConditionStatus::Cancelled);
        Ok(())
    }

    #[test]
    fn queued_for_agent_reads_current_queue_entries() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let now = Utc::now();
        let stale_queued = QueueEntryRecord {
            message_id: "stale-message".into(),
            agent_id: "agent-a".into(),
            priority: crate::types::Priority::Normal,
            status: QueueEntryStatus::Queued,
            created_at: now,
            updated_at: now,
        };
        let mut stale_processed = stale_queued.clone();
        stale_processed.status = QueueEntryStatus::Processed;
        stale_processed.updated_at = now + chrono::Duration::seconds(1);

        let fresh_queued = QueueEntryRecord {
            message_id: "fresh-message".into(),
            agent_id: "agent-a".into(),
            priority: crate::types::Priority::Interject,
            status: QueueEntryStatus::Queued,
            created_at: now + chrono::Duration::seconds(2),
            updated_at: now + chrono::Duration::seconds(2),
        };

        db.queue_entries().upsert(&stale_queued)?;
        db.queue_entries().upsert(&stale_processed)?;
        db.queue_entries().upsert(&fresh_queued)?;

        assert!(db.queue_entries().has_queued_for_agent("agent-a")?);
        let queued = db.queue_entries().queued_for_agent("agent-a")?;
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].message_id, "fresh-message");

        let mut fresh_dequeued = fresh_queued.clone();
        fresh_dequeued.status = QueueEntryStatus::Dequeued;
        fresh_dequeued.updated_at = now + chrono::Duration::seconds(3);
        db.queue_entries().upsert(&fresh_dequeued)?;

        assert!(!db.queue_entries().has_queued_for_agent("agent-a")?);
        assert!(db.queue_entries().queued_for_agent("agent-a")?.is_empty());

        Ok(())
    }

    #[test]
    fn queued_for_agent_includes_interrupted_entries() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let now = Utc::now();

        let queued_entry = QueueEntryRecord {
            message_id: "msg-queued".into(),
            agent_id: "agent-a".into(),
            priority: crate::types::Priority::Normal,
            status: QueueEntryStatus::Queued,
            created_at: now,
            updated_at: now,
        };
        let interrupted_entry = QueueEntryRecord {
            message_id: "msg-interrupted".into(),
            agent_id: "agent-a".into(),
            priority: crate::types::Priority::Normal,
            status: QueueEntryStatus::Interrupted,
            created_at: now + chrono::Duration::seconds(1),
            updated_at: now + chrono::Duration::seconds(1),
        };
        let aborted_entry = QueueEntryRecord {
            message_id: "msg-aborted".into(),
            agent_id: "agent-a".into(),
            priority: crate::types::Priority::Normal,
            status: QueueEntryStatus::Aborted,
            created_at: now + chrono::Duration::seconds(2),
            updated_at: now + chrono::Duration::seconds(2),
        };

        db.queue_entries().upsert(&queued_entry)?;
        db.queue_entries().upsert(&interrupted_entry)?;
        db.queue_entries().upsert(&aborted_entry)?;

        let entries = db.queue_entries().queued_for_agent("agent-a")?;
        let message_ids: Vec<_> = entries.iter().map(|e| e.message_id.as_str()).collect();
        assert!(
            message_ids.contains(&"msg-queued"),
            "Queued entry should be included"
        );
        assert!(
            message_ids.contains(&"msg-interrupted"),
            "Interrupted entry should be included for recovery replay"
        );
        assert!(
            !message_ids.contains(&"msg-aborted"),
            "Aborted entry should NOT be included"
        );

        Ok(())
    }

    #[test]
    fn abort_pending_for_agent_aborts_queued_and_interrupted_entries() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let now = Utc::now();

        let queued_entry = QueueEntryRecord {
            message_id: "msg-queued-abort".into(),
            agent_id: "agent-abort".into(),
            priority: crate::types::Priority::Normal,
            status: QueueEntryStatus::Queued,
            created_at: now,
            updated_at: now,
        };
        let interrupted_entry = QueueEntryRecord {
            message_id: "msg-interrupted-abort".into(),
            agent_id: "agent-abort".into(),
            priority: crate::types::Priority::Normal,
            status: QueueEntryStatus::Interrupted,
            created_at: now,
            updated_at: now,
        };
        let processed_entry = QueueEntryRecord {
            message_id: "msg-processed-keep".into(),
            agent_id: "agent-abort".into(),
            priority: crate::types::Priority::Normal,
            status: QueueEntryStatus::Processed,
            created_at: now,
            updated_at: now,
        };

        db.queue_entries().upsert(&queued_entry)?;
        db.queue_entries().upsert(&interrupted_entry)?;
        db.queue_entries().upsert(&processed_entry)?;

        let count = db.queue_entries().abort_pending_for_agent("agent-abort")?;
        assert_eq!(count, 2, "should abort queued and interrupted entries");

        assert_eq!(
            db.queue_entries()
                .latest("msg-queued-abort")?
                .unwrap()
                .status,
            QueueEntryStatus::Aborted
        );
        assert_eq!(
            db.queue_entries()
                .latest("msg-interrupted-abort")?
                .unwrap()
                .status,
            QueueEntryStatus::Aborted
        );
        // Processed entries should be untouched.
        assert_eq!(
            db.queue_entries()
                .latest("msg-processed-keep")?
                .unwrap()
                .status,
            QueueEntryStatus::Processed
        );

        Ok(())
    }

    #[test]
    fn recovery_candidate_agent_ids_include_dequeued_and_interrupted_entries() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let now = Utc::now();
        for agent_id in [
            "agent-queued",
            "agent-dequeued",
            "agent-interrupted",
            "agent-processed",
        ] {
            db.agent_identities().upsert(&agent_identity(agent_id, 0))?;
        }
        let mut deleted_identity = agent_identity("agent-deleted", 0);
        deleted_identity.status = AgentRegistryStatus::Deleted;
        deleted_identity.deleted_at = Some(now);
        db.agent_identities().upsert(&deleted_identity)?;
        for (message_id, agent_id, status) in [
            ("msg-queued", "agent-queued", QueueEntryStatus::Queued),
            ("msg-dequeued", "agent-dequeued", QueueEntryStatus::Dequeued),
            (
                "msg-interrupted",
                "agent-interrupted",
                QueueEntryStatus::Interrupted,
            ),
            (
                "msg-processed",
                "agent-processed",
                QueueEntryStatus::Processed,
            ),
            (
                "msg-deleted",
                "agent-deleted",
                QueueEntryStatus::Interrupted,
            ),
        ] {
            db.queue_entries().upsert(&QueueEntryRecord {
                message_id: message_id.into(),
                agent_id: agent_id.into(),
                priority: crate::types::Priority::Normal,
                status,
                created_at: now,
                updated_at: now,
            })?;
        }

        assert_eq!(
            db.queue_entries().recovery_candidate_agent_ids()?,
            vec!["agent-dequeued", "agent-interrupted"]
        );
        assert!(db
            .queue_entries()
            .has_interrupted_for_agent("agent-interrupted")?);
        assert!(!db
            .queue_entries()
            .has_interrupted_for_agent("agent-dequeued")?);
        Ok(())
    }

    #[test]
    fn try_claim_succeeds_for_interrupted_entry() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let now = Utc::now();

        let record = QueueEntryRecord {
            message_id: "msg-interrupted".into(),
            agent_id: "agent-a".into(),
            priority: crate::types::Priority::Normal,
            status: QueueEntryStatus::Interrupted,
            created_at: now,
            updated_at: now,
        };
        db.queue_entries().upsert(&record)?;

        // An Interrupted entry must be claimable, otherwise recovery would
        // silently drop it. See PR #2052 review feedback.
        assert!(db.queue_entries().has_queued_for_agent("agent-a")?);
        let mut claim = record.clone();
        claim.status = QueueEntryStatus::Dequeued;
        claim.updated_at = now + chrono::Duration::seconds(1);
        assert!(
            db.queue_entries().try_claim_queued_message(&claim)?,
            "Interrupted entry should be claimable for replay"
        );
        assert_eq!(
            db.queue_entries().latest_all()?[0].status,
            QueueEntryStatus::Dequeued
        );
        Ok(())
    }

    #[test]
    fn runtime_db_foreign_keys_are_enabled_per_connection() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let connection = db.connection()?;
        let enabled: i64 = connection.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
        assert_eq!(enabled, 1);
        Ok(())
    }

    #[test]
    fn agent_state_repository_upserts_latest_turn_state() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let mut current_work = WorkItemRecord::new("agent-a", "current focus", WorkItemState::Open);
        current_work.id = "work-current".into();
        let mut stale_work = WorkItemRecord::new("agent-a", "stale focus", WorkItemState::Open);
        stale_work.id = "work-stale".into();
        db.work_items().insert_new(&current_work)?;
        db.work_items().insert_new(&stale_work)?;
        let mut current = AgentState::new("agent-a");
        current.status = AgentStatus::AwakeIdle;
        current.turn_index = 3;
        current.current_work_item_id = Some("work-current".into());
        db.agent_states().import_legacy(Some(current.clone()))?;

        let mut stale = current.clone();
        stale.status = AgentStatus::Stopped;
        stale.turn_index = 2;
        stale.current_work_item_id = Some("work-stale".into());
        db.agent_states().upsert(&stale)?;

        let persisted = db.agent_states().latest("agent-a")?.expect("agent state");
        assert_eq!(persisted.status, AgentStatus::AwakeIdle);
        assert_eq!(persisted.turn_index, 3);
        assert_eq!(
            persisted.current_work_item_id.as_deref(),
            Some("work-current")
        );
        Ok(())
    }

    #[test]
    fn workspace_entry_import_is_idempotent_and_keeps_latest_update() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let older = workspace_entry("ws-a", 1);
        let mut newer = workspace_entry("ws-a", 5);
        newer.workspace_alias = Some("alias-newer".into());

        db.workspace_entries()
            .import_legacy(vec![older.clone(), newer.clone()])?;
        db.workspace_entries().import_legacy(vec![older, newer])?;

        let entries = db.workspace_entries().latest_all()?;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].workspace_id, "ws-a");
        assert_eq!(entries[0].workspace_alias.as_deref(), Some("alias-newer"));
        let rows: i64 =
            db.connection()?
                .query_row("SELECT COUNT(*) FROM workspace_entries", [], |row| {
                    row.get(0)
                })?;
        assert_eq!(rows, 1);
        Ok(())
    }

    #[test]
    fn workspace_occupancy_import_is_idempotent_and_keeps_released_record() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let active = workspace_occupancy("occ-a", None);
        let released = workspace_occupancy("occ-a", Some(10));

        db.workspace_occupancies()
            .import_legacy(vec![active.clone(), released.clone()])?;
        db.workspace_occupancies()
            .import_legacy(vec![active, released])?;

        let occupancies = db.workspace_occupancies().latest_all()?;
        assert_eq!(occupancies.len(), 1);
        assert_eq!(occupancies[0].occupancy_id, "occ-a");
        assert!(occupancies[0].released_at.is_some());
        let rows: i64 = db.connection()?.query_row(
            "SELECT COUNT(*) FROM workspace_occupancies",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(rows, 1);
        Ok(())
    }

    #[test]
    fn agent_identity_repository_imports_latest_and_reads_by_agent() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let older = agent_identity("agent-a", 1);
        let mut newer = agent_identity("agent-a", 5);
        newer.status = AgentRegistryStatus::Deleted;
        newer.deleted_at = Some(newer.updated_at);

        db.agent_identities()
            .import_legacy(vec![older.clone(), newer.clone()])?;
        db.agent_identities().import_legacy(vec![older, newer])?;

        let identity = db
            .agent_identities()
            .latest("agent-a")?
            .expect("agent identity");
        assert_eq!(identity.status, AgentRegistryStatus::Deleted);
        assert!(identity.deleted_at.is_some());
        let identities = db.agent_identities().latest_all()?;
        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].agent_id, "agent-a");
        Ok(())
    }

    #[test]
    fn agent_deletion_begin_is_idempotent_and_survives_reopen() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let identity = agent_identity("agent-delete", 1);
        db.agent_identities().upsert(&identity)?;

        let (deleting, first_job, created) =
            db.agent_deletions()
                .begin("agent-delete", identity.revision, "operator:test", true)?;
        assert!(created);
        assert_eq!(deleting.status, AgentRegistryStatus::Deleting);
        assert_eq!(deleting.revision, identity.revision + 1);
        assert!(first_job.cascade_private_children);

        let (same_identity, same_job, created) = db.agent_deletions().begin(
            "agent-delete",
            deleting.revision,
            "operator:retry",
            false,
        )?;
        assert!(!created);
        assert_eq!(same_identity, deleting);
        assert_eq!(same_job, first_job);

        drop(db);
        let reopened = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        assert_eq!(
            reopened
                .agent_deletions()
                .latest_for_agent("agent-delete")?,
            Some(first_job)
        );
        assert_eq!(
            reopened
                .agent_identities()
                .latest("agent-delete")?
                .expect("deleting identity")
                .status,
            AgentRegistryStatus::Deleting
        );
        Ok(())
    }

    #[test]
    fn runtime_db_transaction_helper_commits_and_rolls_back() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;

        db.transaction(|tx| {
            tx.execute(
                "INSERT INTO storage_domains (
                    domain, schema_version, import_status, canonical_source, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                ("test", 1_i64, "pending", "jsonl", Utc::now().to_rfc3339()),
            )?;
            Ok(())
        })?;
        let connection = db.connection()?;
        let count: i64 =
            connection.query_row("SELECT COUNT(*) FROM storage_domains", [], |row| row.get(0))?;
        assert_eq!(count, 1);

        let error = db
            .transaction(|tx| {
                tx.execute(
                    "INSERT INTO storage_domains (
                        domain, schema_version, import_status, canonical_source, updated_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5)",
                    (
                        "rollback",
                        1_i64,
                        "pending",
                        "jsonl",
                        Utc::now().to_rfc3339(),
                    ),
                )?;
                Err::<(), anyhow::Error>(anyhow!("force rollback"))
            })
            .unwrap_err();
        assert_eq!(error.to_string(), "force rollback");

        let connection = db.connection()?;
        let count: i64 =
            connection.query_row("SELECT COUNT(*) FROM storage_domains", [], |row| row.get(0))?;
        assert_eq!(count, 1);
        Ok(())
    }

    #[test]
    fn storage_domain_import_failure_is_visible_and_retryable() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;

        let error = db
            .evidence()
            .import_legacy(
                vec![serde_json::json!({ "turn_index": 1 })],
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("importing legacy storage domain evidence"));
        let failed = db
            .storage_domain("evidence")?
            .expect("failed storage domain row");
        assert_eq!(failed.import_status, "failed");
        assert_eq!(failed.canonical_source, "jsonl");
        assert!(failed
            .source_checkpoint_json
            .as_deref()
            .is_some_and(|checkpoint| checkpoint.contains("restart runtime to retry")));

        db.run_storage_domain_import("evidence", "jsonl", "db", |tx| {
            let checkpoint: Option<String> = tx.query_row(
                "SELECT source_checkpoint_json FROM storage_domains WHERE domain = 'evidence'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(checkpoint, failed.source_checkpoint_json);
            Ok(serde_json::json!({ "imported_records": 0 }))
        })?;
        let complete = db
            .storage_domain("evidence")?
            .expect("complete storage domain row");
        assert_eq!(complete.import_status, "complete");
        assert_eq!(complete.canonical_source, "db");
        Ok(())
    }

    #[test]
    fn audit_event_import_failure_is_retryable_and_idempotent() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let mut invalid = AuditEvent::legacy("legacy_audit", serde_json::json!({ "n": 1 }));
        invalid.id = "audit-1".into();
        invalid.event_seq = u64::MAX;

        let error = db
            .audit_events()
            .import_legacy(Some("agent-a"), vec![invalid])
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("importing legacy storage domain audit_events"));
        let failed = db
            .storage_domain("audit_events")?
            .expect("failed storage domain row");
        assert_eq!(failed.import_status, "failed");

        let mut valid = AuditEvent::legacy("legacy_audit", serde_json::json!({ "n": 1 }));
        valid.id = "audit-1".into();
        valid.event_seq = 7;
        db.audit_events()
            .import_legacy(Some("agent-a"), vec![valid.clone()])?;
        db.audit_events()
            .import_legacy(Some("agent-a"), vec![valid])?;

        let complete = db
            .storage_domain("audit_events")?
            .expect("complete storage domain row");
        assert_eq!(complete.import_status, "complete");
        assert_eq!(complete.canonical_source, "db");
        let imported = db.audit_events().recent(Some("agent-a"), 10)?;
        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0].id, "audit-1");
        assert_eq!(imported[0].event_seq, 7);
        Ok(())
    }

    #[test]
    fn cutover_diagnostics_detect_missing_failed_and_mixed_sources() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;

        let missing = db.diagnose_cutover(RuntimeDb::expected_storage_domains())?;
        assert!(missing
            .iter()
            .any(|diagnostic| diagnostic.contains("storage domain work_items is missing")));

        db.transaction(|tx| {
            upsert_storage_domain(tx, "work_items", "complete", "jsonl", None)?;
            upsert_storage_domain(
                tx,
                "tasks",
                "failed",
                "jsonl",
                Some(serde_json::json!({ "error": "forced failure" })),
            )?;
            upsert_storage_domain(tx, "external_triggers", "complete", "db", None)?;
            upsert_storage_domain(tx, "evidence", "complete", "db", None)?;
            upsert_storage_domain(tx, "audit_events", "complete", "db", None)?;
            Ok(())
        })?;

        let diagnostics = db.diagnose_cutover(RuntimeDb::expected_storage_domains())?;
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.contains("storage domain work_items has canonical_source=jsonl")
        }));
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.contains("storage domain tasks import failed")
                && diagnostic.contains("forced failure")
        }));
        assert!(db
            .validate_expected_storage_domains(RuntimeDb::expected_storage_domains())
            .is_err());
        Ok(())
    }

    #[test]
    fn turn_record_repository_imports_legacy_evidence_without_turns_jsonl() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let mut message = MessageEnvelope::new(
            "agent-a",
            crate::types::MessageKind::OperatorPrompt,
            crate::types::MessageOrigin::Operator {
                actor_id: Some("operator:test".into()),
                actor_display_name: None,
            },
            crate::types::AuthorityClass::OperatorInstruction,
            crate::types::Priority::Normal,
            crate::types::MessageBody::Text {
                text: "derive a turn record".into(),
            },
        );
        message.id = "msg-1".into();
        message.message_seq = Some(7);
        message.turn_id = Some("turn-a".into());
        let mut brief = BriefRecord::new(
            "agent-a",
            crate::types::BriefKind::Result,
            "derived result",
            Some("msg-1".into()),
            None,
        );
        brief.id = "brief-1".into();
        brief.turn_id = Some("turn-a".into());
        brief.turn_index = Some(7);
        let tool = ToolExecutionRecord {
            id: "tool-1".into(),
            agent_id: "agent-a".into(),
            work_item_id: Some("work-1".into()),
            turn_index: 7,
            turn_id: Some("turn-a".into()),
            tool_name: "ExecCommand".into(),
            created_at: Utc::now(),
            completed_at: Some(Utc::now()),
            duration_ms: 1,
            authority_class: crate::types::AuthorityClass::RuntimeInstruction,
            status: crate::types::ToolExecutionStatus::Success,
            input: serde_json::json!({ "cmd": "true" }),
            output: serde_json::json!({ "exit": 0 }),
            summary: "Run command: true".into(),
            invocation_surface: None,
        };

        db.turn_records().import_legacy(
            vec![serde_json::to_value(&message)?],
            vec![tool],
            vec![brief],
            Vec::new(),
            Vec::new(),
        )?;

        let records = db.turn_records().recent_for_agent("agent-a", 10)?;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].turn_id, "turn-a");
        assert_eq!(records[0].turn_index, 7);
        assert_eq!(records[0].input_message_ids, vec!["msg-1"]);
        assert_eq!(records[0].produced_brief_ids, vec!["brief-1"]);
        assert_eq!(records[0].tool_execution_ids, vec!["tool-1"]);
        assert_eq!(records[0].current_work_item_id.as_deref(), Some("work-1"));
        assert_eq!(
            db.turn_records()
                .by_id(Some("agent-a"), "turn-a")?
                .expect("turn should be addressable directly")
                .turn_index,
            7
        );
        assert!(db
            .turn_records()
            .by_id(Some("agent-b"), "turn-a")?
            .is_none());
        let domain = db
            .storage_domain("turn_records")?
            .expect("turn_records domain");
        assert_eq!(domain.canonical_source, "db");
        assert!(domain
            .source_checkpoint_json
            .as_deref()
            .is_some_and(|checkpoint| checkpoint.contains("turns.jsonl")));
        Ok(())
    }

    #[test]
    fn turn_record_legacy_import_preserves_existing_identity() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let mut canonical = crate::types::TurnRecord::new("agent-a", "turn-a", 11);
        canonical.current_work_item_id = Some("work-current".into());
        canonical.created_at = Utc::now() - chrono::Duration::seconds(5);
        db.turn_records().upsert(&canonical)?;

        let mut delivery = crate::types::DeliverySummaryRecord::new(
            "agent-a",
            "work-legacy",
            "legacy delivery",
            Some(7),
            None,
        );
        delivery.id = "delivery-legacy".into();
        delivery.turn_id = Some("turn-a".into());

        db.turn_records().import_legacy(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![delivery],
            Vec::new(),
        )?;

        let imported = db
            .turn_records()
            .by_id(Some("agent-a"), "turn-a")?
            .expect("existing turn should remain");
        assert_eq!(imported.turn_index, 11);
        assert_eq!(
            imported.current_work_item_id.as_deref(),
            Some("work-current")
        );
        assert_eq!(imported.created_at, canonical.created_at);
        assert_eq!(imported.delivery_summary_ids, vec!["delivery-legacy"]);
        assert_eq!(imported.completed_work_item_ids, vec!["work-legacy"]);
        Ok(())
    }

    #[test]
    fn runtime_db_temp_helper_uses_isolated_state_dir() -> Result<()> {
        let temp_db = test_support::TempRuntimeDb::new()?;
        assert!(temp_db.db.path().ends_with("state/runtime.sqlite"));
        assert!(temp_db.db.lock_path().ends_with("state/runtime.lock"));
        assert_eq!(
            temp_db.db.current_schema_version()?,
            max_known_migration_version()
        );
        Ok(())
    }

    #[test]
    fn external_trigger_import_normalizes_to_one_default_active_per_agent() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let older =
            external_trigger_record("trigger-older", "agent-a", ExternalTriggerStatus::Active, 0);
        let newer = external_trigger_record(
            "trigger-newer",
            "agent-a",
            ExternalTriggerStatus::Active,
            10,
        );

        db.external_triggers()
            .import_legacy(vec![older.clone(), newer.clone()])?;
        db.external_triggers()
            .import_legacy(vec![older.clone(), newer.clone()])?;

        let active = db
            .external_triggers()
            .active_default_for_agent("agent-a")?
            .expect("active default trigger");
        assert_eq!(active.external_trigger_id, "trigger-newer");
        assert_eq!(active.scope, ExternalTriggerScope::Agent);
        assert_eq!(active.delivery_mode, CallbackDeliveryMode::WakeHint);

        let all = db.external_triggers().latest_for_agent("agent-a")?;
        assert_eq!(all.len(), 2);
        assert_eq!(
            all.into_iter()
                .filter(|record| record.status == ExternalTriggerStatus::Active)
                .count(),
            1
        );
        Ok(())
    }

    #[test]
    fn external_trigger_latest_for_agent_limit_uses_bounded_recent_results() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        db.external_triggers().import_legacy(Vec::new())?;

        for index in 0..4 {
            db.external_triggers().upsert(&external_trigger_record(
                &format!("trigger-{index}"),
                "agent-a",
                ExternalTriggerStatus::Revoked,
                index,
            ))?;
        }
        db.external_triggers().upsert(&external_trigger_record(
            "trigger-other-agent",
            "agent-b",
            ExternalTriggerStatus::Revoked,
            10,
        ))?;

        let recent = db
            .external_triggers()
            .latest_for_agent_limit("agent-a", 2)?;
        assert_eq!(
            recent
                .into_iter()
                .map(|record| record.external_trigger_id)
                .collect::<Vec<_>>(),
            vec!["trigger-3", "trigger-2"]
        );
        assert!(db
            .external_triggers()
            .latest_for_agent_limit("agent-a", 0)?
            .is_empty());
        Ok(())
    }

    #[test]
    fn external_trigger_upsert_tracks_delivery_and_token_lookup() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        db.external_triggers().import_legacy(Vec::new())?;
        let mut trigger = external_trigger_record(
            "trigger-active",
            "agent-a",
            ExternalTriggerStatus::Active,
            0,
        );
        trigger.delivery_mode = CallbackDeliveryMode::WakeHint;
        db.external_triggers().upsert(&trigger)?;

        trigger.delivery_count = 2;
        trigger.last_delivered_at = Some(trigger.created_at + chrono::Duration::seconds(30));
        db.external_triggers().upsert(&trigger)?;

        let by_token = db
            .external_triggers()
            .active_by_token_hash("hash-trigger-active")?
            .expect("active trigger by token");
        assert_eq!(by_token.delivery_count, 2);
        assert_eq!(by_token.last_delivered_at, trigger.last_delivered_at);
        Ok(())
    }

    #[test]
    fn external_trigger_upsert_does_not_revert_newer_revocation() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        db.external_triggers().import_legacy(Vec::new())?;
        let active = external_trigger_record(
            "trigger-active",
            "agent-a",
            ExternalTriggerStatus::Active,
            0,
        );
        db.external_triggers().upsert(&active)?;

        let mut revoked = active.clone();
        revoked.status = ExternalTriggerStatus::Revoked;
        revoked.revoked_at = Some(active.created_at + chrono::Duration::seconds(30));
        db.external_triggers().upsert(&revoked)?;
        db.external_triggers().upsert(&active)?;

        let latest = db
            .external_triggers()
            .latest("trigger-active")?
            .expect("latest trigger");
        assert_eq!(latest.status, ExternalTriggerStatus::Revoked);
        assert_eq!(latest.revoked_at, revoked.revoked_at);
        Ok(())
    }

    #[test]
    fn work_item_import_is_idempotent_and_preserves_latest_revision() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let mut older = WorkItemRecord::new("agent-a", "older objective", WorkItemState::Open);
        older.id = "work-test".into();
        older.revision = 1;
        older.updated_at = older.created_at;
        let mut newer = older.clone();
        newer.objective = "newer objective".into();
        newer.revision = 3;
        newer.updated_at = older.updated_at + chrono::Duration::seconds(10);

        db.work_items()
            .import_legacy(vec![older.clone(), newer.clone()])?;
        db.work_items()
            .import_legacy(vec![older.clone(), newer.clone()])?;

        let imported = db
            .work_items()
            .latest("work-test")?
            .expect("work item imported");
        assert_eq!(imported.revision, 3);
        assert_eq!(imported.objective, "newer objective");
        let connection = db.connection()?;
        let rows: i64 =
            connection.query_row("SELECT COUNT(*) FROM work_items", [], |row| row.get(0))?;
        assert_eq!(rows, 1);
        let current_focus: i64 = connection.query_row(
            "SELECT current_focus FROM work_items WHERE work_item_id = 'work-test'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(current_focus, 0);
        Ok(())
    }

    #[test]
    fn work_item_upsert_rejects_revision_rollback() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        db.work_items().import_legacy(Vec::new())?;
        let mut current = WorkItemRecord::new("agent-a", "current", WorkItemState::Open);
        current.id = "work-revision".into();
        db.work_items().insert_new(&current)?;
        current.revision = 2;
        current.updated_at += chrono::Duration::seconds(1);
        db.work_items().update_expected(&current, 1)?;

        let mut stale = current.clone();
        stale.objective = "stale".into();
        stale.revision = 1;
        stale.updated_at = current.updated_at + chrono::Duration::seconds(10);
        let error = db.work_items().update_expected(&stale, 0).unwrap_err();
        let conflict = error
            .downcast_ref::<RuntimeStateTransitionConflict>()
            .expect("stale update should return typed conflict");
        assert_eq!(conflict.code(), "revision_conflict");
        assert_eq!(conflict.expected_revision(), Some(0));
        assert_eq!(conflict.actual_revision(), Some(2));
        assert!(conflict.retryable());

        let persisted = db
            .work_items()
            .latest("work-revision")?
            .expect("work item persisted");
        assert_eq!(persisted.revision, 2);
        assert_eq!(persisted.objective, "current");
        Ok(())
    }

    #[test]
    fn work_item_expected_update_is_idempotent_and_rejects_same_revision_payload_change(
    ) -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let mut initial = WorkItemRecord::new("agent-a", "initial", WorkItemState::Open);
        initial.id = "work-cas".into();
        db.work_items().insert_new(&initial)?;

        let mut updated = initial.clone();
        updated.revision = 2;
        updated.objective = "updated".into();
        updated.updated_at += chrono::Duration::seconds(1);
        assert!(db.work_items().update_expected(&updated, 1)?);
        assert!(!db.work_items().update_expected(&updated, 1)?);

        let mut conflicting = updated.clone();
        conflicting.objective = "conflicting".into();
        let error = db
            .work_items()
            .update_expected(&conflicting, 1)
            .unwrap_err();
        let conflict = error
            .downcast_ref::<RuntimeStateTransitionConflict>()
            .expect("same revision payload change should return typed conflict");
        assert_eq!(conflict.domain(), "work_item");
        assert_eq!(conflict.code(), "same_revision_payload_conflict");
        assert_eq!(conflict.expected_revision(), Some(1));
        assert_eq!(conflict.actual_revision(), Some(2));
        assert!(!conflict.retryable());
        Ok(())
    }

    #[test]
    fn work_item_expected_update_allows_only_one_writer_across_db_instances() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let first = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let second = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let mut initial = WorkItemRecord::new("agent-a", "initial", WorkItemState::Open);
        initial.id = "work-concurrent-cas".into();
        first.work_items().insert_new(&initial)?;

        let mut left = initial.clone();
        left.revision = 2;
        left.objective = "left".into();
        let mut right = initial.clone();
        right.revision = 2;
        right.objective = "right".into();

        assert!(first.work_items().update_expected(&left, 1)?);
        let error = second.work_items().update_expected(&right, 1).unwrap_err();
        let conflict = error
            .downcast_ref::<RuntimeStateTransitionConflict>()
            .expect("second writer should return typed conflict");
        assert_eq!(conflict.code(), "same_revision_payload_conflict");
        assert_eq!(
            second
                .work_items()
                .latest("work-concurrent-cas")?
                .expect("persisted work item")
                .objective,
            "left"
        );
        Ok(())
    }

    #[test]
    fn work_item_listing_is_partitioned_by_agent() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        db.work_items().import_legacy(Vec::new())?;
        let mut first = WorkItemRecord::new("agent-a", "first", WorkItemState::Open);
        first.id = "work-first".into();
        let mut second = WorkItemRecord::new("agent-b", "second", WorkItemState::Open);
        second.id = "work-second".into();
        db.work_items().insert_new(&first)?;
        db.work_items().insert_new(&second)?;

        let agent_items = db.work_items().latest_for_agent("agent-a", 20)?;
        assert_eq!(agent_items.len(), 1);
        assert_eq!(agent_items[0].id, "work-first");
        Ok(())
    }

    #[test]
    fn task_import_is_idempotent_and_preserves_latest_lifecycle_state() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let queued = task_record("task-import", "agent-a", TaskStatus::Queued, 0);
        let completed = task_record("task-import", "agent-a", TaskStatus::Completed, 10);

        db.tasks()
            .import_legacy(vec![queued.clone(), completed.clone()])?;
        db.tasks().import_legacy(vec![queued, completed])?;

        let imported = db.tasks().latest("task-import")?.expect("task imported");
        assert_eq!(imported.status, TaskStatus::Completed);
        assert_eq!(
            imported
                .detail
                .as_ref()
                .and_then(|detail| detail.get("output_path"))
                .and_then(serde_json::Value::as_str),
            Some("/tmp/task-import.log")
        );
        let connection = db.connection()?;
        let rows: i64 = connection.query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))?;
        assert_eq!(rows, 1);
        let terminal_rows: i64 = connection.query_row(
            "SELECT COUNT(*) FROM tasks WHERE status = 'completed' AND completed_at IS NOT NULL",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(terminal_rows, 1);
        Ok(())
    }

    #[test]
    fn task_import_merges_legacy_metadata_when_latest_update_is_sparse() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let queued = task_record("task-sparse", "agent-a", TaskStatus::Queued, 0);
        let mut completed = task_record("task-sparse", "agent-a", TaskStatus::Completed, 10);
        completed.summary = None;
        completed.detail = None;
        completed.recovery = None;

        db.tasks().import_legacy(vec![queued, completed])?;

        let imported = db.tasks().latest("task-sparse")?.expect("task imported");
        assert_eq!(imported.status, TaskStatus::Completed);
        assert_eq!(imported.summary.as_deref(), Some("task-sparse"));
        assert_eq!(
            imported
                .detail
                .as_ref()
                .and_then(|detail| detail.get("output_path"))
                .and_then(serde_json::Value::as_str),
            Some("/tmp/task-sparse.log")
        );
        Ok(())
    }

    #[test]
    fn task_terminal_state_is_first_writer_wins_across_terminal_matrix() -> Result<()> {
        let statuses = [
            TaskStatus::Completed,
            TaskStatus::Failed,
            TaskStatus::Cancelled,
            TaskStatus::Interrupted,
        ];

        for existing_status in &statuses {
            for incoming_status in &statuses {
                let (_temp_dir, db_path, lock_path) = temp_paths()?;
                let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
                let mut existing =
                    task_record("task-terminal", "agent-a", existing_status.clone(), 1);
                existing.parent_message_id = Some("message-1".into());
                existing.work_item_id = Some("work-1".into());
                existing
                    .detail
                    .as_mut()
                    .and_then(|detail| detail.as_object_mut())
                    .unwrap()
                    .insert("parent_turn_id".into(), serde_json::json!("turn-1"));
                db.tasks().upsert(&existing)?;

                let mut incoming = existing.clone();
                incoming.status = incoming_status.clone();
                incoming.updated_at += chrono::Duration::seconds(1);
                if existing_status == incoming_status {
                    db.tasks().upsert(&incoming)?;
                } else {
                    let error = db.tasks().upsert(&incoming).unwrap_err();
                    let conflict = error
                        .downcast_ref::<RuntimeStateTransitionConflict>()
                        .expect("conflicting terminal task should return a typed conflict");
                    assert_eq!(conflict.domain(), "task");
                    assert_eq!(conflict.record_id(), "task-terminal");
                    assert_eq!(conflict.existing_status(), enum_string(existing_status)?);
                    assert_eq!(conflict.incoming_status(), enum_string(incoming_status)?);
                }
                assert_eq!(
                    db.tasks().latest("task-terminal")?.expect("persisted task"),
                    slim_task_record_for_payload(&existing)
                );
            }
        }
        Ok(())
    }

    #[test]
    fn task_terminal_state_rejects_payload_changes_but_ignores_previews() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let mut terminal = task_record("task-payload", "agent-a", TaskStatus::Completed, 1);
        terminal.parent_message_id = Some("message-1".into());
        terminal.work_item_id = Some("work-1".into());
        db.tasks().upsert(&terminal)?;

        let mut preview_retry = terminal.clone();
        preview_retry.updated_at += chrono::Duration::seconds(1);
        preview_retry
            .detail
            .as_mut()
            .and_then(|detail| detail.as_object_mut())
            .unwrap()
            .insert(
                "output_summary".into(),
                serde_json::json!("a different preview"),
            );
        db.tasks().upsert(&preview_retry)?;

        let mut conflicting_result = terminal.clone();
        conflicting_result.updated_at += chrono::Duration::seconds(2);
        conflicting_result
            .detail
            .as_mut()
            .and_then(|detail| detail.as_object_mut())
            .unwrap()
            .insert("exit_status".into(), serde_json::json!(9));
        assert!(db
            .tasks()
            .upsert(&conflicting_result)
            .unwrap_err()
            .downcast_ref::<RuntimeStateTransitionConflict>()
            .is_some());

        let mut late_active = terminal.clone();
        late_active.status = TaskStatus::Running;
        late_active.updated_at += chrono::Duration::seconds(3);
        assert!(db
            .tasks()
            .upsert(&late_active)
            .unwrap_err()
            .downcast_ref::<RuntimeStateTransitionConflict>()
            .is_some());
        assert_eq!(
            db.tasks().latest("task-payload")?.expect("persisted task"),
            slim_task_record_for_payload(&terminal)
        );
        Ok(())
    }

    #[test]
    fn task_terminal_state_survives_restart_and_second_db_handle() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let first = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let second = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let terminal = task_record("task-restart", "agent-a", TaskStatus::Cancelled, 1);
        first.tasks().upsert(&terminal)?;

        let mut conflicting = terminal.clone();
        conflicting.status = TaskStatus::Interrupted;
        conflicting.updated_at += chrono::Duration::seconds(1);
        assert!(second
            .tasks()
            .upsert(&conflicting)
            .unwrap_err()
            .downcast_ref::<RuntimeStateTransitionConflict>()
            .is_some());

        drop(first);
        drop(second);
        let reopened = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        assert_eq!(
            reopened
                .tasks()
                .latest("task-restart")?
                .expect("persisted task"),
            slim_task_record_for_payload(&terminal)
        );
        Ok(())
    }

    #[test]
    fn task_parent_agent_column_is_only_set_for_child_agent_tasks() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let command = task_record("task-command", "agent-a", TaskStatus::Running, 0);
        let mut child = task_record("task-child", "agent-a", TaskStatus::Running, 1);
        child.detail = Some(serde_json::json!({
            "child_agent_id": "child-a",
            "input_target": "child_followup"
        }));

        db.tasks().upsert(&command)?;
        db.tasks().upsert(&child)?;

        let connection = db.connection()?;
        let command_parent: Option<String> = connection.query_row(
            "SELECT parent_agent_id FROM tasks WHERE task_id = 'task-command'",
            [],
            |row| row.get(0),
        )?;
        let child_parent: Option<String> = connection.query_row(
            "SELECT parent_agent_id FROM tasks WHERE task_id = 'task-child'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(command_parent, None);
        assert_eq!(child_parent.as_deref(), Some("agent-a"));
        Ok(())
    }

    #[test]
    fn task_payload_json_slimguards_large_preview_fields() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let mut task = task_record("task-large", "agent-a", TaskStatus::Running, 0);
        task.detail = Some(serde_json::json!({
            "output_path": "/tmp/task-large.log",
            "initial_output": "i".repeat(TASK_PAYLOAD_STRING_LIMIT + 10),
            "output_summary": "s".repeat(TASK_PAYLOAD_STRING_LIMIT + 10),
            "lines": (0..(TASK_PAYLOAD_ARRAY_LIMIT + 10)).collect::<Vec<_>>()
        }));

        db.tasks().upsert(&task)?;

        let connection = db.connection()?;
        let (payload_json, result_summary): (String, Option<String>) = connection.query_row(
            "SELECT payload_json, result_summary FROM tasks WHERE task_id = 'task-large'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let payload: serde_json::Value = serde_json::from_str(&payload_json)?;
        let detail = &payload["detail"];
        assert!(detail.get("initial_output").is_none());
        assert_eq!(
            detail["output_summary"].as_str().expect("summary").len(),
            TASK_PAYLOAD_STRING_LIMIT
        );
        assert_eq!(
            detail["lines"].as_array().expect("lines").len(),
            TASK_PAYLOAD_ARRAY_LIMIT
        );
        assert_eq!(
            result_summary.expect("result summary").len(),
            TASK_PAYLOAD_STRING_LIMIT
        );
        Ok(())
    }

    #[test]
    fn task_active_listing_is_partitioned_by_agent_and_excludes_terminal() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        db.tasks().import_legacy(Vec::new())?;
        db.tasks().upsert(&task_record(
            "agent-a-running",
            "agent-a",
            TaskStatus::Running,
            1,
        ))?;
        db.tasks().upsert(&task_record(
            "agent-a-completed",
            "agent-a",
            TaskStatus::Completed,
            2,
        ))?;
        db.tasks().upsert(&task_record(
            "agent-b-running",
            "agent-b",
            TaskStatus::Running,
            3,
        ))?;

        let active = db.tasks().active_for_agent("agent-a", 20)?;
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, "agent-a-running");
        let all_agent_a = db.tasks().latest_for_agent("agent-a", 20)?;
        assert_eq!(all_agent_a.len(), 2);
        Ok(())
    }

    #[test]
    fn runtime_db_lock_rejects_second_nonblocking_holder() -> Result<()> {
        if let Ok(lock_path) = std::env::var("HOLON_RUNTIME_DB_LOCK_CHILD_PATH") {
            RuntimeDbLock::try_lock(lock_path).expect_err("second process should not get lock");
            return Ok(());
        }

        let temp_dir = tempdir()?;
        let lock_path = temp_dir.path().join("state/runtime.lock");
        let first = RuntimeDbLock::lock(&lock_path)?;
        let output = Command::new(std::env::current_exe()?)
            .arg("--exact")
            .arg("runtime_db::tests::tests::runtime_db_lock_rejects_second_nonblocking_holder")
            .arg("--nocapture")
            .env("HOLON_RUNTIME_DB_LOCK_CHILD_PATH", &lock_path)
            .output()?;
        assert!(
            output.status.success(),
            "child lock assertion failed: stdout={}, stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        drop(first);

        let second = RuntimeDbLock::try_lock(&lock_path)?;
        assert_eq!(second.path(), lock_path.as_path());
        Ok(())
    }
    #[test]
    fn backfill_wait_condition_payload_columns_adds_columns_and_fills_data() -> Result<()> {
        let (_temp_dir, db_path, _lock_path) = temp_paths()?;
        std::fs::create_dir_all(db_path.parent().unwrap())?;

        let conn = rusqlite::Connection::open(&db_path)?;
        conn.execute_batch(
            "CREATE TABLE wait_conditions (
                wait_condition_id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                work_item_id TEXT,
                status TEXT NOT NULL,
                kind TEXT NOT NULL,
                source TEXT,
                subject_ref TEXT,
                waiting_for TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                expires_at TEXT,
                resolved_at TEXT,
                cancelled_at TEXT,
                last_turn_id TEXT,
                payload_json TEXT NOT NULL
            );",
        )?;

        let now = chrono::Utc::now();
        let payload = serde_json::json!({
            "id": "wc-1",
            "agent_id": "agent-a",
            "status": "active",
            "kind": "external",
            "source": "test",
            "subject_ref": "github:owner/repo#1",
            "waiting_for": "external",
            "wake_sources": [{"kind": "external_ingress", "external_trigger_id": "trigger-123"}],
            "continuation": {"action": "check_pr"},
            "created_at": now.to_rfc3339(),
            "updated_at": now.to_rfc3339()
        });
        let payload_json = serde_json::to_string(&payload)?;

        conn.execute(
            "INSERT INTO wait_conditions (wait_condition_id, agent_id, status, kind, waiting_for, created_at, updated_at, payload_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params!["wc-1", "agent-a", "active", "external", "external", timestamp(now), timestamp(now), payload_json],
        )?;

        super::backfill_wait_condition_payload_columns(&conn)?;

        let wake_sources: String = conn.query_row(
            "SELECT wake_sources_json FROM wait_conditions WHERE wait_condition_id = 'wc-1'",
            [],
            |row| row.get(0),
        )?;
        assert!(wake_sources.contains("external_ingress"));

        let continuation: String = conn.query_row(
            "SELECT continuation_json FROM wait_conditions WHERE wait_condition_id = 'wc-1'",
            [],
            |row| row.get(0),
        )?;
        assert!(continuation.contains("check_pr"));

        Ok(())
    }

    #[test]
    fn backfill_wait_condition_payload_columns_skips_existing_values() -> Result<()> {
        let (_temp_dir, db_path, _lock_path) = temp_paths()?;
        std::fs::create_dir_all(db_path.parent().unwrap())?;

        let conn = rusqlite::Connection::open(&db_path)?;
        conn.execute_batch(
            "CREATE TABLE wait_conditions (
                wait_condition_id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                work_item_id TEXT,
                status TEXT NOT NULL,
                kind TEXT NOT NULL,
                source TEXT,
                subject_ref TEXT,
                waiting_for TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                expires_at TEXT,
                resolved_at TEXT,
                cancelled_at TEXT,
                last_turn_id TEXT,
                payload_json TEXT NOT NULL,
                wake_sources_json TEXT NOT NULL DEFAULT '[]',
                continuation_json TEXT
            );",
        )?;

        let now = chrono::Utc::now();
        conn.execute(
            "INSERT INTO wait_conditions (wait_condition_id, agent_id, status, kind, waiting_for, created_at, updated_at, payload_json, wake_sources_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params!["wc-2", "agent-a", "active", "external", "external", timestamp(now), timestamp(now), "{}", "[\"existing\"]"],
        )?;

        super::backfill_wait_condition_payload_columns(&conn)?;

        let wake_sources: String = conn.query_row(
            "SELECT wake_sources_json FROM wait_conditions WHERE wait_condition_id = 'wc-2'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(wake_sources, "[\"existing\"]");

        Ok(())
    }

    #[test]
    fn backfill_wait_condition_payload_columns_handles_missing_table() -> Result<()> {
        let (_temp_dir, db_path, _lock_path) = temp_paths()?;
        std::fs::create_dir_all(db_path.parent().unwrap())?;

        let conn = rusqlite::Connection::open(&db_path)?;
        super::backfill_wait_condition_payload_columns(&conn)?;
        Ok(())
    }

    #[test]
    fn backfill_work_item_recheck_columns_adds_columns_and_fills_data() -> Result<()> {
        let (_temp_dir, db_path, _lock_path) = temp_paths()?;
        std::fs::create_dir_all(db_path.parent().unwrap())?;

        let conn = rusqlite::Connection::open(&db_path)?;
        conn.execute_batch(
            "CREATE TABLE work_items (
                work_item_id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                state TEXT NOT NULL,
                objective TEXT NOT NULL,
                plan_status TEXT,
                readiness TEXT,
                revision INTEGER NOT NULL,
                current_focus INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                completed_at TEXT,
                plan_artifact_path TEXT,
                last_turn_id TEXT,
                last_message_id TEXT,
                causation_id TEXT,
                correlation_id TEXT,
                payload_json TEXT NOT NULL
            );",
        )?;

        let now = chrono::Utc::now();
        let recheck_time = now + chrono::Duration::hours(1);
        let payload = serde_json::json!({
            "id": "wi-1",
            "agent_id": "agent-a",
            "workspace_id": "ws-test",
            "revision": 1,
            "objective": "Test work item",
            "state": "open",
            "plan_status": "draft",
            "blocked_by": "external:github:owner/repo#1",
            "recheck_at": recheck_time.to_rfc3339(),
            "created_at": now.to_rfc3339(),
            "updated_at": now.to_rfc3339()
        });
        let payload_json = serde_json::to_string(&payload)?;

        conn.execute(
            "INSERT INTO work_items (work_item_id, agent_id, state, objective, revision, current_focus, created_at, updated_at, payload_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params!["wi-1", "agent-a", "open", "Test work item", 1, 0, timestamp(now), timestamp(now), payload_json],
        )?;

        super::backfill_work_item_recheck_columns(&conn)?;

        let blocked_by: String = conn.query_row(
            "SELECT blocked_by FROM work_items WHERE work_item_id = 'wi-1'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(blocked_by, "external:github:owner/repo#1");

        let recheck_at: String = conn.query_row(
            "SELECT recheck_at FROM work_items WHERE work_item_id = 'wi-1'",
            [],
            |row| row.get(0),
        )?;
        assert!(!recheck_at.is_empty());

        Ok(())
    }

    #[test]
    fn backfill_work_item_recheck_columns_skips_when_no_values() -> Result<()> {
        let (_temp_dir, db_path, _lock_path) = temp_paths()?;
        std::fs::create_dir_all(db_path.parent().unwrap())?;

        let conn = rusqlite::Connection::open(&db_path)?;
        conn.execute_batch(
            "CREATE TABLE work_items (
                work_item_id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                state TEXT NOT NULL,
                objective TEXT NOT NULL,
                plan_status TEXT,
                readiness TEXT,
                revision INTEGER NOT NULL,
                current_focus INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                completed_at TEXT,
                plan_artifact_path TEXT,
                last_turn_id TEXT,
                last_message_id TEXT,
                causation_id TEXT,
                correlation_id TEXT,
                payload_json TEXT NOT NULL
            );",
        )?;

        let now = chrono::Utc::now();
        let payload = serde_json::json!({
            "id": "wi-2",
            "agent_id": "agent-a",
            "workspace_id": "ws-test",
            "revision": 1,
            "objective": "Test work item",
            "state": "open",
            "plan_status": "draft",
            "created_at": now.to_rfc3339(),
            "updated_at": now.to_rfc3339()
        });
        let payload_json = serde_json::to_string(&payload)?;

        conn.execute(
            "INSERT INTO work_items (work_item_id, agent_id, state, objective, revision, current_focus, created_at, updated_at, payload_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params!["wi-2", "agent-a", "open", "Test work item", 1, 0, timestamp(now), timestamp(now), payload_json],
        )?;

        super::backfill_work_item_recheck_columns(&conn)?;

        let blocked_by: Option<String> = conn.query_row(
            "SELECT blocked_by FROM work_items WHERE work_item_id = 'wi-2'",
            [],
            |row| row.get(0),
        )?;
        assert!(blocked_by.is_none());

        Ok(())
    }

    #[test]
    fn backfill_work_item_recheck_columns_handles_missing_table() -> Result<()> {
        let (_temp_dir, db_path, _lock_path) = temp_paths()?;
        std::fs::create_dir_all(db_path.parent().unwrap())?;

        let conn = rusqlite::Connection::open(&db_path)?;
        super::backfill_work_item_recheck_columns(&conn)?;
        Ok(())
    }

    #[test]
    fn execution_root_entry_upsert_and_get() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        std::fs::create_dir_all(db_path.parent().unwrap())?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let repo = db.execution_root_entries();

        let entry = ExecutionRootEntry {
            execution_root_id: "git_worktree_root:ws_abc:/tmp/wt".into(),
            workspace_id: "ws_abc".into(),
            filesystem_path: PathBuf::from("/tmp/wt"),
            root_kind: crate::system::WorkspaceProjectionKind::GitWorktreeRoot,
            worktree: None,
            created_at: Utc::now(),
            removed_at: None,
        };
        repo.upsert(&entry)?;

        let fetched = repo.get("git_worktree_root:ws_abc:/tmp/wt")?.unwrap();
        assert_eq!(fetched.workspace_id, "ws_abc");
        assert_eq!(fetched.filesystem_path, PathBuf::from("/tmp/wt"));
        assert!(fetched.removed_at.is_none());
        Ok(())
    }

    #[test]
    fn execution_root_entry_mark_removed() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        std::fs::create_dir_all(db_path.parent().unwrap())?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let repo = db.execution_root_entries();

        let entry = ExecutionRootEntry {
            execution_root_id: "git_worktree_root:ws_xyz:/tmp/wt2".into(),
            workspace_id: "ws_xyz".into(),
            filesystem_path: PathBuf::from("/tmp/wt2"),
            root_kind: crate::system::WorkspaceProjectionKind::GitWorktreeRoot,
            worktree: None,
            created_at: Utc::now(),
            removed_at: None,
        };
        repo.upsert(&entry)?;
        assert!(repo.mark_removed("git_worktree_root:ws_xyz:/tmp/wt2")?);
        let fetched = repo.get("git_worktree_root:ws_xyz:/tmp/wt2")?.unwrap();
        assert!(fetched.removed_at.is_some());

        // Double mark is a no-op.
        assert!(!repo.mark_removed("git_worktree_root:ws_xyz:/tmp/wt2")?);
        Ok(())
    }

    #[test]
    fn execution_root_entry_active_for_workspace() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        std::fs::create_dir_all(db_path.parent().unwrap())?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let repo = db.execution_root_entries();

        // Two roots for the same workspace
        for path in ["/tmp/wt_a", "/tmp/wt_b"] {
            repo.upsert(&ExecutionRootEntry {
                execution_root_id: format!("git_worktree_root:ws_multi:{}", path),
                workspace_id: "ws_multi".into(),
                filesystem_path: PathBuf::from(path),
                root_kind: crate::system::WorkspaceProjectionKind::GitWorktreeRoot,
                worktree: None,
                created_at: Utc::now(),
                removed_at: None,
            })?;
        }

        // One root for a different workspace
        repo.upsert(&ExecutionRootEntry {
            execution_root_id: "git_worktree_root:ws_other:/tmp/wt_c".into(),
            workspace_id: "ws_other".into(),
            filesystem_path: PathBuf::from("/tmp/wt_c"),
            root_kind: crate::system::WorkspaceProjectionKind::GitWorktreeRoot,
            worktree: None,
            created_at: Utc::now(),
            removed_at: None,
        })?;

        // Mark one of ws_multi's roots as removed
        repo.mark_removed("git_worktree_root:ws_multi:/tmp/wt_a")?;

        let active = repo.active_for_workspace("ws_multi")?;
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].filesystem_path, PathBuf::from("/tmp/wt_b"));

        let other = repo.active_for_workspace("ws_other")?;
        assert_eq!(other.len(), 1);
        Ok(())
    }

    #[test]
    fn execution_root_entry_get_not_found() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        std::fs::create_dir_all(db_path.parent().unwrap())?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let repo = db.execution_root_entries();

        let result = repo.get("nonexistent")?;
        assert!(result.is_none());
        Ok(())
    }

    #[test]
    fn observer_sync_fresh_database_mints_and_preserves_runtime_identity() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        {
            let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
            let runtime_id = db.runtime_id()?;
            let epoch = db.event_log_epoch()?;
            assert!(runtime_id.starts_with("runtime_"));
            assert!(epoch.starts_with("epoch_"));
            assert_ne!(runtime_id, epoch);
            assert_eq!(db.visibility_policy_generation()?, 0);
            let foundations = db.observer_sync_foundations()?;
            assert!(foundations.runtime_identity_stable);
            assert!(foundations.agent_identity_reserved);
        }
        let reopened = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        assert!(reopened.runtime_id()?.starts_with("runtime_"));
        assert!(reopened.event_log_epoch()?.starts_with("epoch_"));
        let foundations = reopened.observer_sync_foundations()?;
        assert!(foundations.runtime_identity_stable);
        assert!(foundations.agent_identity_reserved);
        assert!(foundations.event_projection_effect_complete);
        Ok(())
    }

    #[test]
    fn observer_sync_event_projection_effect_accepts_legacy_and_typed_events() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        {
            let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
            for kind in [
                "agent_state_changed",
                "brief_created",
                "scheduler_diagnostic",
                "future_legacy_kind",
            ] {
                let legacy = crate::types::AuditEvent::legacy(
                    kind,
                    serde_json::json!({ "agent_id": "default" }),
                );
                db.audit_events().append(Some("default"), &legacy)?;
            }
            let descriptor = crate::runtime_event::RuntimeEventKind::WorkItemWritten.descriptor();
            let typed = crate::types::AuditEvent {
                id: "evt_typed_fixture".into(),
                event_seq: 0,
                event_log_epoch: String::new(),
                created_at: chrono::Utc::now(),
                kind: descriptor.wire_name.into(),
                contract_version: crate::runtime_event::RUNTIME_EVENT_CONTRACT_VERSION,
                payload_schema: descriptor.payload_schema.into(),
                payload_schema_version: descriptor.payload_schema_version,
                data: serde_json::from_str(descriptor.fixture_json)?,
            };
            db.audit_events().append(Some("default"), &typed)?;
            let proof: (i64, i64, i64) = db.connection()?.query_row(
                "SELECT verified, event_generation, verified_event_generation
                 FROM observer_sync_capability_verifications
                 WHERE capability = 'event_projection_effect_complete'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            assert_eq!(proof.0, 1);
            assert_eq!(proof.1, 5);
            assert_eq!(proof.2, proof.1);
            assert!(
                db.observer_sync_foundations()?
                    .event_projection_effect_complete
            );
        }
        let reopened = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let foundations = reopened.observer_sync_foundations()?;
        assert!(foundations.event_projection_effect_complete);
        Ok(())
    }

    #[test]
    fn observer_sync_event_projection_effect_rejects_unsupported_typed_events() -> Result<()> {
        for (kind, payload_schema, payload_schema_version) in [
            ("future_kind", "holon.runtime_event.future", 1),
            ("brief_created", "holon.runtime_event.wrong", 1),
            (
                "scheduler_diagnostic",
                crate::runtime_event::RuntimeEventKind::SchedulerDiagnostic
                    .descriptor()
                    .payload_schema,
                crate::runtime_event::RuntimeEventKind::SchedulerDiagnostic
                    .descriptor()
                    .payload_schema_version
                    + 1,
            ),
        ] {
            let (_temp_dir, db_path, lock_path) = temp_paths()?;
            {
                let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
                let mut event =
                    crate::types::AuditEvent::legacy(kind, serde_json::json!({ "opaque": true }));
                event.contract_version = crate::runtime_event::RUNTIME_EVENT_CONTRACT_VERSION;
                event.payload_schema = payload_schema.into();
                event.payload_schema_version = payload_schema_version;
                db.audit_events().append(Some("default"), &event)?;
                assert!(
                    !db.observer_sync_foundations()?
                        .event_projection_effect_complete,
                    "unsupported trusted append must disable the capability immediately"
                );
            }
            let reopened = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
            let foundations = reopened.observer_sync_foundations()?;
            assert!(
                !foundations.event_projection_effect_complete,
                "{kind}@{payload_schema}v{payload_schema_version} must stay unsupported"
            );
        }
        Ok(())
    }

    #[test]
    fn observer_sync_event_projection_effect_reuses_current_proof_on_reopen() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        {
            let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
            db.connection()?.execute(
                "UPDATE observer_sync_capability_verifications
                 SET detail = 'proof-reuse-sentinel'
                 WHERE capability = 'event_projection_effect_complete'",
                [],
            )?;
        }

        let reopened = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let proof: (i64, String, i64, i64, i64) = reopened.connection()?.query_row(
            "SELECT verified, detail, verification_version,
                    event_generation, verified_event_generation
             FROM observer_sync_capability_verifications
             WHERE capability = 'event_projection_effect_complete'",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?;
        assert_eq!(proof.0, 1);
        assert_eq!(proof.1, "proof-reuse-sentinel");
        assert_eq!(proof.2, EVENT_PROJECTION_EFFECT_VERIFIER_VERSION);
        assert_eq!(proof.3, proof.4);
        Ok(())
    }

    #[test]
    fn observer_sync_foundations_degrade_when_event_proof_metadata_is_unreadable() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        db.connection()?.execute(
            "DELETE FROM runtime_metadata WHERE key = 'event_log_epoch'",
            [],
        )?;

        let foundations = db.observer_sync_foundations()?;
        assert!(foundations.runtime_identity_stable);
        assert!(foundations.agent_identity_reserved);
        assert!(!foundations.event_projection_effect_complete);
        Ok(())
    }

    #[test]
    fn observer_sync_event_projection_effect_reverifies_stale_version_or_epoch() -> Result<()> {
        for stale_column in ["verification_version", "event_log_epoch"] {
            let (_temp_dir, db_path, lock_path) = temp_paths()?;
            {
                let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
                let sql = format!(
                    "UPDATE observer_sync_capability_verifications
                     SET {stale_column} = ?1, detail = 'stale-proof-sentinel'
                     WHERE capability = 'event_projection_effect_complete'"
                );
                let stale_value = if stale_column == "verification_version" {
                    "0"
                } else {
                    "epoch_stale"
                };
                db.connection()?.execute(&sql, [stale_value])?;
            }

            let reopened = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
            let (version, epoch, detail): (i64, String, String) =
                reopened.connection()?.query_row(
                    "SELECT verification_version, event_log_epoch, detail
                     FROM observer_sync_capability_verifications
                     WHERE capability = 'event_projection_effect_complete'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?;
            assert_eq!(version, EVENT_PROJECTION_EFFECT_VERIFIER_VERSION);
            assert_eq!(epoch, reopened.event_log_epoch()?);
            assert!(detail.contains("full_inventory"));
        }
        Ok(())
    }

    #[test]
    fn observer_sync_event_projection_effect_direct_write_invalidates_proof() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        {
            let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
            let epoch = db.event_log_epoch()?;
            let event = crate::types::AuditEvent {
                id: "evt_direct_unsupported".into(),
                event_seq: 1,
                event_log_epoch: epoch,
                created_at: chrono::Utc::now(),
                kind: "future_typed_kind".into(),
                contract_version: crate::runtime_event::RUNTIME_EVENT_CONTRACT_VERSION,
                payload_schema: "holon.runtime_event.future".into(),
                payload_schema_version: 1,
                data: serde_json::json!({ "opaque": true }),
            };
            db.connection()?.execute(
                "INSERT INTO audit_events (
                   audit_event_id, event_seq, agent_id, kind, created_at, data_json
                 ) VALUES (?1, ?2, 'default', ?3, ?4, ?5)",
                rusqlite::params![
                    event.id,
                    event.event_seq,
                    event.kind,
                    timestamp(event.created_at),
                    serde_json::to_string(&event)?,
                ],
            )?;
            assert!(
                !db.observer_sync_foundations()?
                    .event_projection_effect_complete,
                "the insert trigger must make an untrusted write fail closed"
            );
        }

        let reopened = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let proof: (i64, i64, i64, String) = reopened.connection()?.query_row(
            "SELECT verified, event_generation, verified_event_generation, detail
             FROM observer_sync_capability_verifications
             WHERE capability = 'event_projection_effect_complete'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        assert_eq!(proof.0, 0);
        assert_eq!(proof.1, proof.2);
        assert!(proof.3.contains("full_inventory"));
        Ok(())
    }

    #[test]
    fn agent_event_recovery_window_reads_one_committed_view() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let empty = db.agent_event_recovery_window(Some("default"))?;
        assert_eq!(empty.event_head_seq, 0);
        assert_eq!(empty.oldest_retained_seq, 0);
        assert!(empty.event_log_epoch.starts_with("epoch_"));
        for index in 1..=3 {
            let event = crate::types::AuditEvent::legacy(
                format!("legacy_{index}"),
                serde_json::json!({ "index": index }),
            );
            db.audit_events().append(Some("default"), &event)?;
        }
        let window = db.agent_event_recovery_window(Some("default"))?;
        assert_eq!(window.event_head_seq, 3);
        assert_eq!(window.oldest_retained_seq, 0);
        assert_eq!(window.event_log_epoch, db.event_log_epoch()?);
        for index in 1..=2 {
            let event = crate::types::AuditEvent::legacy(
                format!("runtime_legacy_{index}"),
                serde_json::json!({ "index": index }),
            );
            db.audit_events().append(None, &event)?;
        }
        // The scoped window must not absorb runtime-level rows, and the
        // unscoped window must see them instead of silently matching
        // nothing behind a `agent_id = NULL` comparison.
        let scoped = db.agent_event_recovery_window(Some("default"))?;
        assert_eq!(scoped.event_head_seq, 3);
        assert_eq!(scoped.oldest_retained_seq, 0);
        let unscoped = db.agent_event_recovery_window(None)?;
        assert_eq!(unscoped.event_head_seq, 2);
        assert_eq!(unscoped.oldest_retained_seq, 0);
        assert_eq!(unscoped.event_log_epoch, db.event_log_epoch()?);
        Ok(())
    }

    #[test]
    fn observer_sync_windows_use_durable_head_and_floor_after_full_prefix_deletion() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        db.agent_identities().upsert(&agent_identity("member", 0))?;
        for index in 1..=3 {
            db.audit_events().append(
                Some("member"),
                &crate::types::AuditEvent::legacy(
                    format!("legacy_{index}"),
                    serde_json::json!({ "index": index }),
                ),
            )?;
        }
        db.transaction(|tx| {
            tx.execute("DELETE FROM audit_events WHERE agent_id = 'member'", [])?;
            tx.execute(
                "INSERT INTO audit_event_retention_watermarks (
                   scope_key, oldest_retained_seq
                 ) VALUES ('agent:member', 4)",
                [],
            )?;
            Ok(())
        })?;

        let recovery = db.agent_event_recovery_window(Some("member"))?;
        assert_eq!(recovery.event_head_seq, 3);
        assert_eq!(recovery.oldest_retained_seq, 4);
        let roster = db.agent_roster_snapshot_rows()?;
        let roster_row = roster
            .rows
            .iter()
            .find(|row| row.agent_id == "member")
            .expect("member roster row");
        assert_eq!(roster_row.event_head_seq, 3);
        assert_eq!(roster_row.oldest_retained_seq, 4);
        let projection = db
            .agent_projection_snapshot_rows("member")?
            .row
            .expect("member projection row");
        assert_eq!(projection.event_head_seq, 3);
        assert_eq!(projection.oldest_retained_seq, 4);
        Ok(())
    }

    fn insert_roster_brief(
        db: &RuntimeDb,
        evidence_id: &str,
        agent_id: &str,
        created_at: &str,
        created_event_seq: Option<i64>,
        preview: Option<&str>,
    ) -> Result<()> {
        let payload = serde_json::json!({
            "id": evidence_id,
            "agent_id": agent_id,
            "kind": "result",
            "created_at": created_at,
            "text": "brief text",
        });
        db.connection()?.execute(
            "INSERT INTO briefs (
                evidence_id, agent_id, created_at, kind, preview, payload_json, created_event_seq
             ) VALUES (?1, ?2, ?3, 'result', ?4, ?5, ?6)",
            rusqlite::params![
                evidence_id,
                agent_id,
                created_at,
                preview,
                payload.to_string(),
                created_event_seq
            ],
        )?;
        Ok(())
    }

    #[test]
    fn agent_roster_snapshot_rows_read_one_committed_view() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;

        // Membership: one public active member, one private child, one
        // deleted public agent. Only the member may appear.
        db.agent_identities()
            .upsert(&agent_identity("member-public", 0))?;
        let mut private = agent_identity("child-private", 0);
        private.visibility = AgentVisibility::Private;
        db.agent_identities().upsert(&private)?;
        let mut deleted = agent_identity("member-deleted", 0);
        deleted.status = AgentRegistryStatus::Deleted;
        db.agent_identities().upsert(&deleted)?;

        for index in 1..=3 {
            let event = crate::types::AuditEvent::legacy(
                format!("roster_event_{index}"),
                serde_json::json!({ "index": index }),
            );
            db.audit_events().append(Some("member-public"), &event)?;
        }
        db.audit_events().append(
            Some("child-private"),
            &crate::types::AuditEvent::legacy("roster_private_event", serde_json::json!({})),
        )?;

        insert_roster_brief(
            &db,
            "brief-older",
            "member-public",
            "2026-01-01T00:00:01.000Z",
            Some(1),
            Some("older"),
        )?;
        insert_roster_brief(
            &db,
            "brief-newer",
            "member-public",
            "2026-01-02T00:00:01.000Z",
            Some(2),
            Some("newer"),
        )?;
        insert_roster_brief(
            &db,
            "brief-private",
            "child-private",
            "2026-01-03T00:00:01.000Z",
            None,
            Some("private"),
        )?;

        db.agent_states()
            .upsert(&crate::types::AgentState::new("member-public"))?;

        let snapshot = db.agent_roster_snapshot_rows()?;
        assert_eq!(snapshot.runtime_id, db.runtime_id()?);
        assert_eq!(snapshot.event_log_epoch, db.event_log_epoch()?);
        assert_eq!(snapshot.visibility_policy_generation, 0);
        let ids: Vec<&str> = snapshot
            .rows
            .iter()
            .map(|row| row.agent_id.as_str())
            .collect();
        assert_eq!(ids, vec!["member-public"]);

        let row = &snapshot.rows[0];
        assert_eq!(row.event_head_seq, 3);
        assert_eq!(row.oldest_retained_seq, 0);
        let brief = row.latest_brief.as_ref().expect("latest brief anchor");
        assert_eq!(brief.brief_id, "brief-newer");
        assert_eq!(brief.created_event_seq, Some(2));
        assert_eq!(brief.preview.as_deref(), Some("newer"));
        assert!(row.agent_state_json.is_some());

        // A registered member with no committed state, events, or briefs
        // still appears with zero anchors instead of vanishing.
        db.agent_identities()
            .upsert(&agent_identity("member-empty", 1))?;
        let snapshot = db.agent_roster_snapshot_rows()?;
        assert_eq!(snapshot.rows.len(), 2);
        let empty = snapshot
            .rows
            .iter()
            .find(|row| row.agent_id == "member-empty")
            .expect("empty member row");
        assert_eq!(empty.event_head_seq, 0);
        assert_eq!(empty.oldest_retained_seq, 0);
        assert_eq!(empty.latest_brief, None);
        assert_eq!(empty.agent_state_json, None);
        Ok(())
    }

    #[test]
    fn agent_roster_snapshot_rows_break_latest_brief_ties_deterministically() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        db.agent_identities().upsert(&agent_identity("member", 0))?;
        let tied = "2026-01-01T00:00:00.000Z";
        insert_roster_brief(&db, "brief-a", "member", tied, None, Some("a"))?;
        insert_roster_brief(&db, "brief-z", "member", tied, None, Some("z"))?;
        insert_roster_brief(
            &db,
            "brief-older",
            "member",
            "2025-12-31T00:00:00.000Z",
            None,
            Some("older"),
        )?;
        let snapshot = db.agent_roster_snapshot_rows()?;
        assert_eq!(
            snapshot.rows[0].latest_brief.as_ref().unwrap().brief_id,
            "brief-z"
        );
        Ok(())
    }

    #[test]
    fn agent_roster_latest_brief_query_is_driven_by_public_membership() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        db.agent_identities().upsert(&agent_identity("member", 0))?;

        let connection = db.connection()?;
        let query_plan = {
            let mut statement = connection.prepare(&format!(
                "EXPLAIN QUERY PLAN {AGENT_ROSTER_LATEST_BRIEFS_SQL}"
            ))?;
            let details = statement
                .query_map([], |row| row.get::<_, String>(3))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            details
        };

        assert!(
            query_plan.iter().any(|detail| {
                detail.contains("SEARCH b USING INDEX sqlite_autoindex_briefs_1 (evidence_id=?)")
            }),
            "roster latest-Brief lookup must probe one Brief by primary key per public member: {query_plan:?}"
        );
        assert!(
            query_plan
                .iter()
                .all(|detail| !detail.contains("SEARCH b USING INDEX idx_briefs_agent_turn")),
            "roster latest-Brief lookup must not visit every historical Brief: {query_plan:?}"
        );
        Ok(())
    }

    #[test]
    fn roster_snapshot_verification_degrades_on_unreadable_committed_state() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        {
            let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
            assert!(db.observer_sync_foundations()?.roster_snapshot_verified);
            db.agent_identities().upsert(&agent_identity("member", 0))?;
            db.agent_states()
                .upsert(&crate::types::AgentState::new("member"))?;
            db.connection()?.execute(
                "UPDATE agent_states SET payload_json = '{not json' WHERE agent_id = 'member'",
                [],
            )?;
        }
        let reopened = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let foundations = reopened.observer_sync_foundations()?;
        assert!(!foundations.roster_snapshot_verified);
        assert!(foundations.runtime_identity_stable);
        Ok(())
    }

    fn insert_projection_message(
        db: &RuntimeDb,
        evidence_id: &str,
        agent_id: &str,
        created_at: &str,
    ) -> Result<()> {
        let payload = serde_json::json!({
            "id": evidence_id,
            "agent_id": agent_id,
            "kind": "operator_prompt",
            "created_at": created_at,
        });
        db.connection()?.execute(
            "INSERT INTO messages (
                evidence_id, agent_id, created_at, kind, payload_json
             ) VALUES (?1, ?2, ?3, 'operator_prompt', ?4)",
            rusqlite::params![evidence_id, agent_id, created_at, payload.to_string()],
        )?;
        Ok(())
    }

    fn insert_projection_transcript_entry(
        db: &RuntimeDb,
        evidence_id: &str,
        agent_id: &str,
        created_at: &str,
    ) -> Result<()> {
        let payload = serde_json::json!({
            "id": evidence_id,
            "agent_id": agent_id,
            "kind": "assistant",
            "created_at": created_at,
        });
        db.connection()?.execute(
            "INSERT INTO transcript_entries (
                evidence_id, agent_id, created_at, kind, payload_json
             ) VALUES (?1, ?2, ?3, 'assistant', ?4)",
            rusqlite::params![evidence_id, agent_id, created_at, payload.to_string()],
        )?;
        Ok(())
    }

    fn insert_projection_work_item(
        db: &RuntimeDb,
        work_item_id: &str,
        agent_id: &str,
        state: &str,
        plan_status: Option<&str>,
        revision: i64,
        current_focus: bool,
        updated_at: &str,
    ) -> Result<()> {
        // The payload stays a decodable WorkItemRecord even when a column
        // is deliberately corrupt, so open-time migrations succeed and the
        // projection verification is what observes the corruption.
        let mut record = crate::types::WorkItemRecord::new(
            agent_id,
            "projection snapshot test",
            crate::types::WorkItemState::Open,
        );
        record.id = work_item_id.to_string();
        record.revision = revision.max(1) as u64;
        record.plan_status = plan_status
            .and_then(|status| match status {
                "draft" => Some(crate::types::WorkItemPlanStatus::Draft),
                "ready" => Some(crate::types::WorkItemPlanStatus::Ready),
                "needs_input" => Some(crate::types::WorkItemPlanStatus::NeedsInput),
                _ => None,
            })
            .unwrap_or(crate::types::WorkItemPlanStatus::Draft);
        let payload = serde_json::to_value(&record)?;
        db.connection()?.execute(
            "INSERT INTO work_items (
                work_item_id, agent_id, state, objective, plan_status, revision,
                current_focus, created_at, updated_at, payload_json
             ) VALUES (?1, ?2, ?3, 'objective', ?4, ?5, ?6, ?7, ?7, ?8)",
            rusqlite::params![
                work_item_id,
                agent_id,
                state,
                plan_status,
                revision,
                i64::from(current_focus),
                updated_at,
                payload.to_string()
            ],
        )?;
        Ok(())
    }

    #[test]
    fn agent_projection_snapshot_rows_read_one_committed_view() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        db.agent_identities().upsert(&agent_identity("member", 0))?;
        db.agent_states()
            .upsert(&crate::types::AgentState::new("member"))?;
        for index in 1..=3 {
            let event = crate::types::AuditEvent::legacy(
                format!("projection_event_{index}"),
                serde_json::json!({ "index": index }),
            );
            db.audit_events().append(Some("member"), &event)?;
        }
        insert_projection_work_item(
            &db,
            "work-stale",
            "member",
            "open",
            Some("draft"),
            4,
            false,
            "2026-01-01T00:00:00.000Z",
        )?;
        insert_projection_work_item(
            &db,
            "work-focused",
            "member",
            "open",
            Some("ready"),
            2,
            true,
            "2026-01-02T00:00:00.000Z",
        )?;
        insert_projection_work_item(
            &db,
            "work-other-agent",
            "stranger",
            "open",
            None,
            9,
            false,
            "2026-01-03T00:00:00.000Z",
        )?;
        insert_projection_message(&db, "msg-older", "member", "2026-01-01T00:00:00.000Z")?;
        insert_projection_message(&db, "msg-newer", "member", "2026-01-02T00:00:00.000Z")?;
        insert_projection_message(&db, "msg-stranger", "stranger", "2026-01-04T00:00:00.000Z")?;
        insert_projection_transcript_entry(&db, "te-only", "member", "2026-01-01T12:00:00.000Z")?;
        insert_roster_brief(
            &db,
            "brief-member",
            "member",
            "2026-01-02T00:00:00.000Z",
            Some(3),
            Some("member preview"),
        )?;

        let snapshot = db.agent_projection_snapshot_rows("member")?;
        assert_eq!(snapshot.runtime_id, db.runtime_id()?);
        assert_eq!(snapshot.event_log_epoch, db.event_log_epoch()?);
        let row = snapshot.row.expect("member anchors");
        assert_eq!(row.agent_id, "member");
        assert_eq!(row.event_head_seq, 3);
        assert_eq!(row.oldest_retained_seq, 0);
        let work_item = row.current_work_item.expect("current work item anchor");
        assert_eq!(work_item.work_item_id, "work-focused");
        assert_eq!(work_item.state, "open");
        assert_eq!(work_item.plan_status.as_deref(), Some("ready"));
        assert_eq!(work_item.revision, 2);
        assert_eq!(row.latest_message_id.as_deref(), Some("msg-newer"));
        assert_eq!(row.latest_transcript_entry_id.as_deref(), Some("te-only"));
        assert_eq!(
            row.latest_brief.as_ref().expect("latest brief").brief_id,
            "brief-member"
        );
        Ok(())
    }

    #[test]
    fn agent_projection_snapshot_rows_absent_for_inaccessible_identities() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        db.agent_identities().upsert(&agent_identity("member", 0))?;
        let mut private = agent_identity("child-private", 0);
        private.visibility = AgentVisibility::Private;
        db.agent_identities().upsert(&private)?;
        let mut deleted = agent_identity("member-deleted", 0);
        deleted.status = AgentRegistryStatus::Deleted;
        db.agent_identities().upsert(&deleted)?;

        for agent_id in ["child-private", "member-deleted", "never-existed"] {
            let snapshot = db.agent_projection_snapshot_rows(agent_id)?;
            assert!(
                snapshot.row.is_none(),
                "{agent_id} must not assemble projection anchors"
            );
        }
        // Metadata stays readable only for real members.
        assert!(db.agent_projection_snapshot_rows("member")?.row.is_some());
        Ok(())
    }

    #[test]
    fn agent_projection_snapshot_boundary_equals_committed_head() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        db.agent_identities().upsert(&agent_identity("member", 0))?;
        db.audit_events().append(
            Some("member"),
            &crate::types::AuditEvent::legacy("head_before", serde_json::json!({})),
        )?;
        let before = db.agent_projection_snapshot_rows("member")?;
        let head_before = before.row.as_ref().expect("member").event_head_seq;
        assert_eq!(head_before, 1);
        db.audit_events().append(
            Some("member"),
            &crate::types::AuditEvent::legacy("head_after", serde_json::json!({})),
        )?;
        let after = db.agent_projection_snapshot_rows("member")?;
        let row = after.row.expect("member");
        assert_eq!(row.event_head_seq, head_before + 1);
        // Every later event stays replayable through the raw event page.
        let count: i64 = db.connection()?.query_row(
            "SELECT COUNT(*) FROM audit_events WHERE agent_id = 'member' AND event_seq > ?1",
            [head_before as i64],
            |row| row.get(0),
        )?;
        assert_eq!(count, 1);
        Ok(())
    }

    #[test]
    fn agent_projection_snapshot_falls_back_to_latest_open_work_item() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        db.agent_identities().upsert(&agent_identity("member", 0))?;
        insert_projection_work_item(
            &db,
            "work-completed",
            "member",
            "completed",
            Some("ready"),
            5,
            false,
            "2026-01-05T00:00:00.000Z",
        )?;
        insert_projection_work_item(
            &db,
            "work-open-older",
            "member",
            "open",
            None,
            1,
            false,
            "2026-01-01T00:00:00.000Z",
        )?;
        insert_projection_work_item(
            &db,
            "work-open-newer",
            "member",
            "open",
            Some("draft"),
            2,
            false,
            "2026-01-02T00:00:00.000Z",
        )?;
        let snapshot = db.agent_projection_snapshot_rows("member")?;
        let row = snapshot.row.expect("member anchors");
        assert_eq!(
            row.current_work_item.expect("fallback anchor").work_item_id,
            "work-open-newer"
        );
        Ok(())
    }

    #[test]
    fn projection_snapshot_verification_degrades_on_unreadable_work_item_anchor() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        {
            let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
            assert!(db.observer_sync_foundations()?.projection_snapshot_verified);
            db.agent_identities().upsert(&agent_identity("member", 0))?;
            insert_projection_work_item(
                &db,
                "work-corrupt",
                "member",
                "not-a-state",
                Some("ready"),
                1,
                true,
                "2026-01-01T00:00:00.000Z",
            )?;
        }
        let reopened = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let foundations = reopened.observer_sync_foundations()?;
        assert!(!foundations.projection_snapshot_verified);
        // The sibling capabilities keep their own verdicts.
        assert!(foundations.roster_snapshot_verified);
        assert!(foundations.runtime_identity_stable);
        Ok(())
    }

    #[test]
    fn projection_snapshot_verification_passes_for_healthy_database() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        db.agent_identities().upsert(&agent_identity("member", 0))?;
        db.agent_states()
            .upsert(&crate::types::AgentState::new("member"))?;
        db.audit_events().append(
            Some("member"),
            &crate::types::AuditEvent::legacy("healthy", serde_json::json!({})),
        )?;
        insert_projection_work_item(
            &db,
            "work-healthy",
            "member",
            "open",
            Some("needs_input"),
            3,
            true,
            "2026-01-01T00:00:00.000Z",
        )?;
        assert!(db.observer_sync_foundations()?.projection_snapshot_verified);
        Ok(())
    }

    #[test]
    fn observer_sync_fresh_databases_mint_distinct_identity() -> Result<()> {
        let (_first_dir, first_path, first_lock) = temp_paths()?;
        let (_second_dir, second_path, second_lock) = temp_paths()?;
        let first = RuntimeDb::open_and_migrate(&first_path, &first_lock)?;
        let second = RuntimeDb::open_and_migrate(&second_path, &second_lock)?;
        assert_ne!(first.runtime_id()?, second.runtime_id()?);
        assert_ne!(first.event_log_epoch()?, second.event_log_epoch()?);
        Ok(())
    }

    #[test]
    fn observer_sync_agent_identity_verification_accepts_persisted_agent_state_shape() -> Result<()>
    {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        {
            let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
            db.agent_identities()
                .upsert(&agent_identity("agent-state-shape", 0))?;
            db.agent_states()
                .upsert(&AgentState::new("agent-state-shape"))?;
        }

        let reopened = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        assert!(
            reopened
                .observer_sync_foundations()?
                .agent_identity_reserved
        );
        Ok(())
    }

    #[test]
    fn observer_sync_retired_agent_ids_are_never_reusable() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let mut identity = agent_identity("agent-doomed", 0);
        db.agent_identities().upsert(&identity)?;

        identity.status = AgentRegistryStatus::Deleted;
        identity.deleted_at = Some(Utc::now());
        identity.updated_at = Utc::now();
        db.agent_identities().upsert(&identity)?;

        let reservation: Option<(String, String)> = db
            .connection()?
            .query_row(
                "SELECT reservation_state, source FROM agent_identity_reservations
                 WHERE agent_id = 'agent-doomed'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        assert_eq!(
            reservation,
            Some(("retired".to_string(), "agent_registry".to_string()))
        );

        let mut revived = agent_identity("agent-doomed", 10);
        let error = db
            .agent_identities()
            .upsert(&revived)
            .expect_err("retired agent id must not be reusable");
        assert!(error.to_string().contains("never reused"));
        revived.status = AgentRegistryStatus::Deleting;
        assert!(db.agent_identities().upsert(&revived).is_err());

        // Re-asserting the tombstone stays idempotent.
        let mut tombstone = agent_identity("agent-doomed", 20);
        tombstone.status = AgentRegistryStatus::Deleted;
        tombstone.deleted_at = Some(Utc::now());
        db.agent_identities().upsert(&tombstone)?;
        Ok(())
    }

    fn reservation_state(
        db: &RuntimeDb,
        agent_id: &str,
    ) -> Result<Option<(String, String, Option<String>)>> {
        db.connection()?
            .query_row(
                "SELECT reservation_state, source, retired_at
                 FROM agent_identity_reservations WHERE agent_id = ?1",
                [agent_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(Into::into)
    }

    #[test]
    fn observer_sync_migration_backfills_reservations_from_history() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        {
            let mut connection = open_connection(&db_path)?;
            migrate_through(&mut connection, 47)?;
            let now = "2026-01-01T00:00:00.000Z";
            connection.execute(
                "INSERT INTO agent_identities (
                    agent_id, kind, visibility, ownership, profile_preset, status,
                    parent_agent_id, lineage_parent_agent_id, delegated_from_task_id,
                    created_at, updated_at, archived_at, payload_json
                 ) VALUES
                    ('agent-keep', 'named', 'public', 'self_owned', 'public_named', 'active',
                     NULL, NULL, NULL, ?1, ?1, NULL, '{}'),
                    ('agent-gone', 'named', 'public', 'self_owned', 'public_named', 'deleted',
                     NULL, NULL, NULL, ?1, ?1, ?1, '{}'),
                    ('agent-del-only', 'named', 'public', 'self_owned', 'public_named', 'deleted',
                     NULL, NULL, NULL, ?1, ?1, NULL, '{}')",
                [now],
            )?;
            connection.execute(
                "INSERT INTO agent_states (agent_id, status, updated_at, payload_json) VALUES
                    ('agent-state-only', 'idle', ?1, '{\"id\":\"agent-state-only\"}')",
                [now],
            )?;
            connection.execute(
                "INSERT INTO audit_events (audit_event_id, agent_id, kind, created_at, data_json)
                 VALUES ('event-audit', 'agent-audit-only', 'fixture', ?1, '{}')",
                [now],
            )?;
            connection.execute(
                "INSERT INTO agent_deletion_jobs (
                    deletion_id, agent_id, status, phase, created_at, updated_at,
                    completed_at, payload_json
                 ) VALUES ('del-job', 'agent-del-only', 'completed', 'finalize', ?1, ?1, ?1, '{}')",
                [now],
            )?;
            connection.execute(
                "INSERT INTO agents (
                    agent_id, status, visibility, ownership, profile_preset,
                    created_at, updated_at, payload_json
                 ) VALUES ('agent-legacy-only', 'active', 'public', NULL, NULL, ?1, ?1, '{}')",
                [now],
            )?;
        }

        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        assert_eq!(
            reservation_state(&db, "agent-keep")?,
            Some(("active".to_string(), "agent_registry".to_string(), None))
        );
        assert_eq!(
            reservation_state(&db, "agent-gone")?,
            Some((
                "retired".to_string(),
                "agent_registry".to_string(),
                Some("2026-01-01T00:00:00.000Z".to_string())
            ))
        );
        assert_eq!(
            reservation_state(&db, "agent-state-only")?,
            Some((
                "retired".to_string(),
                "backfill:agent-state".to_string(),
                None
            ))
        );
        assert_eq!(
            reservation_state(&db, "agent-audit-only")?,
            Some(("retired".to_string(), "backfill:audit".to_string(), None))
        );
        assert_eq!(
            reservation_state(&db, "agent-del-only")?,
            Some((
                "retired".to_string(),
                "agent_registry".to_string(),
                Some("2026-01-01T00:00:00.000Z".to_string())
            ))
        );
        assert_eq!(
            reservation_state(&db, "agent-legacy-only")?,
            Some((
                "retired".to_string(),
                "backfill:legacy-agents".to_string(),
                None
            ))
        );
        let foundations = db.observer_sync_foundations()?;
        assert!(foundations.runtime_identity_stable);
        assert!(foundations.agent_identity_reserved);
        Ok(())
    }

    #[test]
    fn observer_sync_backfill_ambiguity_blocks_agent_identity_capability() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        {
            let mut connection = open_connection(&db_path)?;
            migrate_through(&mut connection, 47)?;
            let now = "2026-01-01T00:00:00.000Z";
            connection.execute(
                "INSERT INTO agent_states (agent_id, status, updated_at, payload_json) VALUES
                    ('agent-broken', 'idle', ?1, '{\"id\":\"agent-other\"}')",
                [now],
            )?;
            connection.execute(
                "INSERT INTO agent_identities (
                    agent_id, kind, visibility, ownership, profile_preset, status,
                    parent_agent_id, lineage_parent_agent_id, delegated_from_task_id,
                    created_at, updated_at, archived_at, payload_json
                 ) VALUES ('agent-reused', 'named', 'public', 'self_owned', 'public_named',
                     'active', NULL, NULL, NULL, ?1, ?1, NULL, '{}')",
                [now],
            )?;
            connection.execute(
                "INSERT INTO agent_deletion_jobs (
                    deletion_id, agent_id, status, phase, created_at, updated_at,
                    completed_at, payload_json
                 ) VALUES ('del-reused', 'agent-reused', 'completed', 'finalize', ?1, ?1, ?1, '{}')",
                [now],
            )?;
        }

        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let foundations = db.observer_sync_foundations()?;
        assert!(foundations.runtime_identity_stable);
        assert!(!foundations.agent_identity_reserved);
        let detail: String = db.connection()?.query_row(
            "SELECT detail FROM observer_sync_capability_verifications
             WHERE capability = 'agent_identity_reserved'",
            [],
            |row| row.get(0),
        )?;
        assert!(detail.contains("agent_state_identity_mismatch"));
        assert!(detail.contains("completed_deletion_with_available_registry"));
        Ok(())
    }

    #[test]
    fn observer_sync_reservation_drift_blocks_capability() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        {
            let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
            db.agent_identities()
                .upsert(&agent_identity("agent-drift", 0))?;
            let foundations = db.observer_sync_foundations()?;
            assert!(foundations.agent_identity_reserved);
            db.connection()?
                .execute("DELETE FROM agent_identity_reservations", [])?;
        }
        let reopened = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let foundations = reopened.observer_sync_foundations()?;
        assert!(foundations.runtime_identity_stable);
        assert!(!foundations.agent_identity_reserved);
        Ok(())
    }

    #[test]
    fn turn_owner_is_indexed_and_reconstructed_after_reopen() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let owner = TurnOwner::Conversation {
            interaction_id: "interaction1_test".into(),
        };
        {
            let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
            let mut turn = TurnRecord::new("agent-owner", "turn-owner", 7);
            turn.owner = Some(owner.clone());
            db.turn_records().upsert(&turn)?;
            assert_eq!(
                db.turn_records()
                    .recent_for_owner("agent-owner", &owner, 10)?,
                vec![turn]
            );
        }

        let reopened = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let turn = reopened
            .turn_records()
            .by_id(Some("agent-owner"), "turn-owner")?
            .expect("turn should survive reopen");
        assert_eq!(turn.effective_owner(), owner);
        Ok(())
    }

    #[test]
    fn legacy_turn_owner_fallback_is_conservative() {
        let mut work_item_turn = TurnRecord::new("agent-owner", "turn-work", 1);
        work_item_turn.current_work_item_id = Some("work-1".into());
        assert_eq!(
            work_item_turn.effective_owner(),
            TurnOwner::WorkItem {
                work_item_id: "work-1".into(),
            }
        );

        let lifecycle_turn = TurnRecord::new("agent-owner", "turn-lifecycle", 2);
        assert_eq!(
            lifecycle_turn.effective_owner(),
            TurnOwner::AgentLifecycle {
                agent_id: "agent-owner".into(),
            }
        );
    }
}
