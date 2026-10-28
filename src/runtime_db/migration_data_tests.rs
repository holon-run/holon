use crate::{
    runtime_db::{
        evidence::{append_message_tx, upsert_agent_identity_tx, upsert_agent_state_tx},
        migrations::{apply_migration, ensure_migration_table, MIGRATIONS},
        repositories,
        task_result_settlement::{upsert_pending_tx, TaskResultSettlementRecord},
        RuntimeDb,
    },
    types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentState,
        AgentVisibility, AuthorityClass, CallbackDeliveryMode, ExternalTriggerRecord,
        ExternalTriggerScope, ExternalTriggerStatus, MessageBody, MessageEnvelope, MessageKind,
        MessageOrigin, Priority, QueueEntryRecord, QueueEntryStatus, TaskKind, TaskRecord,
        TaskStatus, TimerRecord, TimerStatus, WorkItemDelegationRecord,
    },
};
use anyhow::{Context, Result};
use rusqlite::{types::Value, Connection, Transaction};

const CANONICAL_TABLES: &[&str] = &[
    "messages",
    "tasks",
    "task_result_settlements",
    "queue_entries",
    "timers",
    "timer_wakes",
    "external_triggers",
    "work_item_delegations",
];

fn rows(connection: &Connection, sql: &str) -> Result<Vec<Vec<Value>>> {
    let mut query = connection.prepare(sql)?;
    let count = query.column_count();
    let values = query
        .query_map([], |row| (0..count).map(|index| row.get(index)).collect())?
        .collect::<rusqlite::Result<_>>()?;
    Ok(values)
}

fn canonical(connection: &Connection) -> Result<Vec<Vec<Vec<Value>>>> {
    CANONICAL_TABLES
        .iter()
        .map(|table| {
            // Main adds recovery columns at 83; compare the unchanged original
            // business projection across that additive schema boundary.
            let projection = if *table == "task_result_settlements" {
                "result_identity,agent_id,task_id,message_id,work_item_id,
                 rejoin_generation,parent_turn_id,state,activation_id,disposition,
                 created_at,updated_at,admitted_at,settled_at,deferred_reason,
                 deferred_at,next_recheck_at,payload_json"
            } else {
                "*"
            };
            rows(
                connection,
                &format!("SELECT {projection} FROM {table} ORDER BY rowid"),
            )
        })
        .collect()
}

fn fixed_facts(connection: &Connection) -> Result<Vec<Vec<Vec<Value>>>> {
    [
        "SELECT family,record_id,availability,content_digest,projection_digest
         FROM task_domain_eligibility ORDER BY family,record_id",
        "SELECT family,record_id,fingerprint_json,mandatory
         FROM input_evidence_eligibility ORDER BY family,record_id",
        "SELECT family,record_id,content_digest,projection_digest,status,sort_at,created_at
         FROM scheduling_fixed_members ORDER BY family,record_id",
        "SELECT * FROM task_status_source_inventory ORDER BY singleton",
        "SELECT * FROM task_status_fixed_delegations ORDER BY child_agent_id",
    ]
    .into_iter()
    .map(|sql| rows(connection, sql))
    .collect()
}

fn seed(tx: &Transaction<'_>) -> Result<()> {
    let at = "2026-10-01T00:00:00.123456789Z".parse()?;
    let mut identity = AgentIdentityRecord::new(
        "migration-agent",
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    identity.created_at = at;
    identity.updated_at = at;
    upsert_agent_identity_tx(tx, &identity)?;
    let mut state = AgentState::new("migration-agent");
    state.pending = 2;
    state.total_message_count = 2;
    upsert_agent_state_tx(tx, &state)?;
    tx.execute(
        "INSERT INTO storage_domains(domain,schema_version,import_status,canonical_source,updated_at)
         VALUES('context_episode_anchors',1,'complete','db',?1)",
        [repositories::timestamp(at)],
    )?;
    for (id, status) in [
        ("running-task", TaskStatus::Running),
        ("completed-task", TaskStatus::Completed),
    ] {
        let task = TaskRecord {
            id: id.into(),
            agent_id: "migration-agent".into(),
            kind: TaskKind::CommandTask,
            status,
            created_at: at,
            updated_at: at,
            parent_message_id: None,
            work_item_id: None,
            summary: Some(id.into()),
            detail: Some(serde_json::json!({
                "rejoin_obligation_id": id,
                "rejoin_generation": 1,
                "parent_turn_id": "original-parent-turn",
            })),
            recovery: None,
        };
        repositories::upsert_task_tx(tx, &task)?;
        if task.status == TaskStatus::Completed {
            let mut message = MessageEnvelope::new(
                "migration-agent",
                MessageKind::TaskResult,
                MessageOrigin::Task { task_id: id.into() },
                AuthorityClass::RuntimeInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "original result".into(),
                },
            );
            message.id = "result-message".into();
            message.task_id = Some(id.into());
            message.created_at = at;
            let (message, _) = append_message_tx(tx, &message)?;
            let result = TaskResultSettlementRecord::pending(&task, &message, at)?
                .context("terminal command result has a settlement")?;
            upsert_pending_tx(tx, &result)?;
            enqueue(tx, &message)?;
        }
    }
    let timer = TimerRecord {
        calendar: None,
        id: "original-timer".into(),
        agent_id: "migration-agent".into(),
        created_at: at,
        duration_ms: 1000,
        interval_ms: Some(1000),
        repeat: true,
        status: TimerStatus::Active,
        summary: Some("original repeating timer".into()),
        next_fire_at: Some(at + chrono::Duration::seconds(1)),
        last_fired_at: Some(at),
        fire_count: 5,
    };
    repositories::upsert_timer_tx(tx, &timer)?;
    let mut wake = MessageEnvelope::new(
        "migration-agent",
        MessageKind::TimerTick,
        MessageOrigin::Timer {
            timer_id: timer.id.clone(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "original wake".into(),
        },
    );
    wake.id = "wake-message".into();
    wake.created_at = at;
    wake.metadata = Some(serde_json::json!({"timer_fire_count": 3}));
    let (wake, _) = append_message_tx(tx, &wake)?;
    enqueue(tx, &wake)?;
    tx.execute(
        "INSERT INTO timer_wakes(timer_id,message_id,fire_count,status,created_at,updated_at)
         VALUES(?1,?2,3,'pending',?3,?3)",
        rusqlite::params![timer.id, wake.id, repositories::timestamp(at)],
    )?;
    repositories::upsert_external_trigger_tx(
        tx,
        &ExternalTriggerRecord {
            external_trigger_id: "original-trigger".into(),
            target_agent_id: "migration-agent".into(),
            scope: ExternalTriggerScope::Agent,
            delivery_mode: CallbackDeliveryMode::WakeHint,
            token: Some("fixture-not-a-credential".into()),
            token_hash: "fixture-hash".into(),
            status: ExternalTriggerStatus::Active,
            created_at: at,
            revoked_at: None,
            last_delivered_at: None,
            delivery_count: 0,
        },
    )?;
    let mut delegation = WorkItemDelegationRecord::new(
        "migration-agent",
        "parent-work",
        "child-agent",
        "child-work",
    );
    delegation.delegation_id = "original-delegation".into();
    delegation.created_at = at;
    delegation.updated_at = at;
    repositories::upsert_work_item_delegation_tx(tx, &delegation)?;
    Ok(())
}

fn enqueue(tx: &Transaction<'_>, message: &MessageEnvelope) -> Result<()> {
    repositories::upsert_queue_entry_tx(
        tx,
        &QueueEntryRecord {
            message_id: message.id.clone(),
            agent_id: message.agent_id.clone(),
            priority: message.priority.clone(),
            status: QueueEntryStatus::Queued,
            created_at: message.created_at,
            updated_at: message.created_at,
        },
    )?;
    Ok(())
}

#[test]
fn migration_data_matrix_preserves_task_input_status_and_scheduling_across_lineages() -> Result<()>
{
    let mut expected_fixed = None;
    let cases = (80..=102).map(|version| (version, None)).chain(
        [81, 83, 89]
            .into_iter()
            .flat_map(|start| (91..=102).map(move |version| (version, Some(start)))),
    );
    for (prefix, legacy_start) in cases {
        let home = tempfile::tempdir()?;
        let path = home.path().join("runtime.sqlite");
        let lock = home.path().join("runtime.lock");
        let mut connection = Connection::open(&path)?;
        ensure_migration_table(&connection)?;
        for migration in MIGRATIONS
            .iter()
            .filter(|migration| migration.version <= 80)
        {
            apply_migration(&mut connection, migration)?;
        }
        let tx = connection.transaction()?;
        seed(&tx)
            .with_context(|| format!("seed prefix={prefix}, legacy_start={legacy_start:?}"))?;
        tx.commit()?;
        let original = canonical(&connection)?;
        for migration in MIGRATIONS.iter().filter(|migration| {
            migration.version > 80
                && migration.version <= prefix
                && legacy_start
                    .is_none_or(|start| migration.version < start || migration.version >= 91)
        }) {
            apply_migration(&mut connection, migration)?;
        }
        if let Some(start) = legacy_start {
            // Reproduce the old lineage's exact ledger, not its SQL twice.
            for version in 91..=prefix {
                connection.execute(
                    "UPDATE schema_migrations SET version=version-?2 WHERE version=?1",
                    [version, 91 - start],
                )?;
            }
        }
        let old_history = super::history(&connection)?;
        drop(connection);
        let db = RuntimeDb::open_and_migrate(&path, &lock)
            .with_context(|| format!("prefix={prefix}, legacy_start={legacy_start:?}"))?;
        let connection = db.connection()?;
        assert_eq!(
            canonical(&connection)?,
            original,
            "prefix={prefix}, legacy_start={legacy_start:?}"
        );
        assert_eq!(
            rows(
                &connection,
                "SELECT recovery_epoch,recovery_attempts,last_recovery_root_attempt_id,
                 last_recovery_admitted_at,recovery_paused_at,recovery_diagnostic_at
                 FROM task_result_settlements ORDER BY rowid",
            )?,
            vec![vec![
                Value::Integer(0),
                Value::Integer(0),
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null,
            ]],
            "main recovery defaults must survive feature qualification"
        );
        for (version, name, timestamp) in old_history {
            let actual_version = if let Some(start) = legacy_start.filter(|start| version >= *start)
            {
                version + (91 - start)
            } else {
                version
            };
            let actual: (String, String) = connection.query_row(
                "SELECT name,applied_at FROM schema_migrations WHERE version=?1",
                [actual_version],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            assert_eq!(actual, (name, timestamp));
        }
        let fixed = fixed_facts(&connection)?;
        assert!(!fixed[0].is_empty(), "Task cut must be populated");
        assert!(!fixed[1].is_empty(), "Input cut must be populated");
        assert_eq!(
            fixed[2].len(),
            3,
            "timer, wake and trigger must all survive"
        );
        assert_eq!(fixed[4].len(), 1, "TaskStatus delegation must survive");
        if let Some(expected) = &expected_fixed {
            assert_eq!(
                &fixed, expected,
                "prefix={prefix}, legacy_start={legacy_start:?}"
            );
        } else {
            expected_fixed = Some(fixed.clone());
        }
        let history = super::history(&connection)?;
        drop(connection);
        drop(db);
        let reopened = RuntimeDb::open_and_migrate(&path, &lock)?;
        let connection = reopened.connection()?;
        assert_eq!(canonical(&connection)?, original);
        assert_eq!(
            fixed_facts(&connection)?,
            fixed,
            "reopen must not recapture"
        );
        assert_eq!(super::history(&connection)?, history);
    }
    Ok(())
}

#[test]
fn fresh_migration_has_no_invented_task_input_or_scheduling_members() -> Result<()> {
    let home = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    let connection = db.connection()?;
    assert!(canonical(&connection)?.iter().all(Vec::is_empty));
    let fixed = fixed_facts(&connection)?;
    for index in [0, 1, 2, 4] {
        assert!(fixed[index].is_empty());
    }
    let count: usize = connection.query_row(
        "SELECT COUNT(*) FROM schema_migrations WHERE version BETWEEN 81 AND 102",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(count, 22);
    Ok(())
}
