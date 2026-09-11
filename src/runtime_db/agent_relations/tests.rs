use super::*;
use crate::types::AgentVisibility;
use chrono::{TimeZone, Utc};
use tempfile::tempdir;

fn identity(
    agent_id: &str,
    kind: AgentKind,
    visibility: AgentVisibility,
    ownership: AgentOwnership,
    preset: AgentProfilePreset,
    parent_agent_id: Option<&str>,
    task_id: Option<&str>,
) -> AgentIdentityRecord {
    let created_at = Utc.with_ymd_and_hms(2026, 9, 6, 0, 0, 0).unwrap();
    let mut identity = AgentIdentityRecord::new(
        agent_id,
        kind,
        visibility,
        ownership,
        preset,
        parent_agent_id.map(str::to_string),
        task_id.map(str::to_string),
    );
    identity.created_at = created_at;
    identity.updated_at = created_at;
    identity
}

fn task_evidence(
    task_id: &str,
    owner_agent_id: &str,
    child_agent_id: &str,
) -> LegacySupervisionTaskEvidence {
    LegacySupervisionTaskEvidence {
        task_id: task_id.into(),
        owner_agent_id: owner_agent_id.into(),
        child_agent_id: Some(child_agent_id.into()),
        is_child_agent_task: true,
        delegated_from_work_item_id: Some("work-parent".into()),
    }
}

fn supervision_task(task_id: &str, owner_agent_id: &str, child_agent_id: &str) -> TaskRecord {
    let created_at = Utc.with_ymd_and_hms(2026, 9, 6, 0, 0, 1).unwrap();
    TaskRecord {
        id: task_id.into(),
        agent_id: owner_agent_id.into(),
        kind: TaskKind::ChildAgentTask,
        status: crate::types::TaskStatus::Running,
        created_at,
        updated_at: created_at,
        parent_message_id: None,
        work_item_id: Some("work-parent".into()),
        summary: None,
        detail: Some(serde_json::json!({
            "child_agent_id": child_agent_id,
            "created_new_subagent": true,
        })),
        recovery: None,
    }
}

#[test]
fn public_named_legacy_identity_maps_to_resolved_independent_projection() {
    let identity = identity(
        "worker",
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    let projection =
        project_agent_canonical_relations(&identity, AgentCanonicalRecordSet::default(), None);

    assert_eq!(projection.resolution, AgentCanonicalResolution::Resolved);
    assert_eq!(projection.lineage, None);
    assert_eq!(projection.supervision, None);
    assert_eq!(
        projection.durability.as_ref().unwrap().durability,
        AgentCanonicalDurability::Persistent
    );
    assert_eq!(
        projection.lifecycle_attachment.as_ref().unwrap().attachment,
        AgentLifecycleAttachment::Independent
    );
    assert_eq!(
        projection.sources.capability_policy,
        Some(AgentCanonicalValueSource::Legacy)
    );
    assert_eq!(
        projection.message_policy.as_ref().unwrap().default_effect,
        AgentPolicyEffect::Deny
    );
}

#[test]
fn private_child_legacy_identity_uses_parent_and_task_evidence() {
    let identity = identity(
        "child",
        AgentKind::Child,
        AgentVisibility::Private,
        AgentOwnership::ParentSupervised,
        AgentProfilePreset::PrivateChild,
        Some("parent"),
        Some("task-child"),
    );
    let task = task_evidence("task-child", "parent", "child");
    let projection = project_agent_canonical_relations(
        &identity,
        AgentCanonicalRecordSet::default(),
        Some(&task),
    );

    assert_eq!(projection.resolution, AgentCanonicalResolution::Resolved);
    assert_eq!(
        projection.lineage.as_ref().unwrap().parent_agent_id,
        "parent"
    );
    let supervision = projection.supervision.as_ref().unwrap();
    assert_eq!(supervision.supervisor_agent_id, "parent");
    assert_eq!(
        supervision.delegated_from_work_item_id.as_deref(),
        Some("work-parent")
    );
    assert_eq!(
        projection.durability.as_ref().unwrap().durability,
        AgentCanonicalDurability::Ephemeral
    );
    assert_eq!(
        projection.lifecycle_attachment.as_ref().unwrap().attachment,
        AgentLifecycleAttachment::SupervisionAttached
    );
}

#[test]
fn parent_supervised_legacy_identity_without_task_is_not_guessed() {
    let identity = identity(
        "child",
        AgentKind::Child,
        AgentVisibility::Private,
        AgentOwnership::ParentSupervised,
        AgentProfilePreset::PrivateChild,
        Some("parent"),
        Some("missing-task"),
    );
    let projection =
        project_agent_canonical_relations(&identity, AgentCanonicalRecordSet::default(), None);

    assert_eq!(
        projection.resolution,
        AgentCanonicalResolution::MissingEvidence
    );
    assert_eq!(projection.supervision, None);
    assert_eq!(projection.durability, None);
    assert_eq!(projection.lifecycle_attachment, None);
}

#[test]
fn conflicting_legacy_parent_fields_are_reported() {
    let mut identity = identity(
        "child",
        AgentKind::Child,
        AgentVisibility::Private,
        AgentOwnership::ParentSupervised,
        AgentProfilePreset::PrivateChild,
        Some("parent-a"),
        Some("task-child"),
    );
    identity.lineage_parent_agent_id = Some("parent-b".into());
    let task = task_evidence("task-child", "parent-a", "child");
    let projection = project_agent_canonical_relations(
        &identity,
        AgentCanonicalRecordSet::default(),
        Some(&task),
    );

    assert_eq!(
        projection.resolution,
        AgentCanonicalResolution::Contradictory
    );
    assert_eq!(projection.lineage, None);
    assert!(projection.issues.iter().any(|issue| {
        issue.axis == AgentCanonicalRelationAxis::Lineage
            && issue.resolution == AgentCanonicalResolution::Contradictory
    }));
}

#[test]
fn canonical_axis_wins_over_legacy_default() {
    let mut identity = identity(
        "worker",
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    identity.durability = Some(crate::types::AgentDurability::Persistent);
    let canonical_durability = AgentDurabilityRecord {
        agent_id: "worker".into(),
        durability: AgentCanonicalDurability::Ephemeral,
        revision: 4,
        created_at: identity.created_at,
    };
    let projection = project_agent_canonical_relations(
        &identity,
        AgentCanonicalRecordSet {
            durability: Some(canonical_durability.clone()),
            ..Default::default()
        },
        None,
    );

    assert_eq!(projection.durability, Some(canonical_durability));
    assert_eq!(
        projection.sources.durability,
        Some(AgentCanonicalValueSource::Canonical)
    );
    assert_eq!(
        projection.resolution,
        AgentCanonicalResolution::Contradictory
    );
}

#[test]
fn canonical_projection_round_trips_and_has_json_schema() {
    let identity = identity(
        "worker",
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    let projection =
        project_agent_canonical_relations(&identity, AgentCanonicalRecordSet::default(), None);
    let json = serde_json::to_value(&projection).unwrap();
    assert_eq!(
        serde_json::from_value::<AgentCanonicalRelationsProjection>(json).unwrap(),
        projection
    );
    let schema =
        serde_json::to_value(schemars::schema_for!(AgentCanonicalRelationsProjection)).unwrap();
    assert!(schema.get("$defs").is_some() || schema.get("definitions").is_some());
}

#[test]
fn repository_round_trips_mixed_canonical_and_legacy_axes() -> Result<()> {
    let temp_dir = tempdir()?;
    let db_path = temp_dir.path().join("state/runtime.sqlite");
    let lock_path = temp_dir.path().join("state/runtime.lock");
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
    let parent = identity(
        "parent",
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    let child = identity(
        "child",
        AgentKind::Child,
        AgentVisibility::Private,
        AgentOwnership::ParentSupervised,
        AgentProfilePreset::PrivateChild,
        Some("parent"),
        Some("task-child"),
    );
    db.agent_identities().upsert(&parent)?;
    db.agent_identities().upsert(&child)?;

    let repository = db.agent_canonical_relations();
    repository.upsert_lineage(&AgentLineageRecord {
        child_agent_id: "child".into(),
        parent_agent_id: "parent".into(),
        creation_cause: AgentLineageCreationCause::Migration,
        revision: 1,
        created_at: child.created_at,
    })?;
    repository.upsert_supervision(&AgentSupervisionRecord {
        supervision_id: "supervision-child".into(),
        supervisor_agent_id: "parent".into(),
        child_agent_id: "child".into(),
        delegated_from_work_item_id: Some("work-parent".into()),
        delegated_from_task_id: Some("task-child".into()),
        state: AgentSupervisionState::Active,
        revision: 1,
        created_at: child.created_at,
        updated_at: child.updated_at,
    })?;
    repository.upsert_durability(&AgentDurabilityRecord {
        agent_id: "child".into(),
        durability: AgentCanonicalDurability::Ephemeral,
        revision: 1,
        created_at: child.created_at,
    })?;
    repository.upsert_lifecycle_attachment(&AgentLifecycleAttachmentRecord {
        agent_id: "child".into(),
        attachment: AgentLifecycleAttachment::SupervisionAttached,
        revision: 1,
        created_at: child.created_at,
    })?;
    repository.upsert_capability_policy(&AgentCapabilityPolicyRecord {
        agent_id: "child".into(),
        revision: 1,
        rules: vec![AgentCapabilityPolicyRule {
            family: AgentCapabilityFamily::CoreAgent,
            effect: AgentPolicyEffect::Allow,
        }],
        created_at: child.created_at,
    })?;
    repository.upsert_message_policy(&AgentMessagePolicyRecord {
        agent_id: "child".into(),
        revision: 1,
        default_effect: AgentPolicyEffect::Deny,
        rules: vec![AgentMessagePolicyRule {
            principal_kind: AgentMessagePrincipalKind::SupervisingParent,
            principal_id: Some("parent".into()),
            route: Some("supervision_follow_up".into()),
            effect: AgentPolicyEffect::Allow,
        }],
        created_at: child.created_at,
    })?;

    let projection = repository.latest("child")?.unwrap();
    assert_eq!(projection.resolution, AgentCanonicalResolution::Resolved);
    assert_eq!(
        projection.sources.lineage,
        Some(AgentCanonicalValueSource::Canonical)
    );
    assert_eq!(
        projection.sources.supervision,
        Some(AgentCanonicalValueSource::Canonical)
    );
    assert_eq!(
        projection.sources.capability_policy,
        Some(AgentCanonicalValueSource::Canonical)
    );
    assert_eq!(
        projection.sources.message_policy,
        Some(AgentCanonicalValueSource::Canonical)
    );
    assert_eq!(
        projection.lineage.unwrap().creation_cause,
        AgentLineageCreationCause::Migration
    );
    Ok(())
}

#[test]
fn lineage_children_merges_canonical_and_legacy_records() -> Result<()> {
    let temp_dir = tempdir()?;
    let db_path = temp_dir.path().join("state/runtime.sqlite");
    let lock_path = temp_dir.path().join("state/runtime.lock");
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
    let parent = identity(
        "parent",
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    let canonical_child = identity(
        "canonical-child",
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    let legacy_child = identity(
        "legacy-child",
        AgentKind::Child,
        AgentVisibility::Private,
        AgentOwnership::ParentSupervised,
        AgentProfilePreset::PrivateChild,
        Some("parent"),
        Some("task-legacy"),
    );
    for record in [&parent, &canonical_child, &legacy_child] {
        db.agent_identities().upsert(record)?;
    }
    let repository = db.agent_canonical_relations();
    repository.upsert_lineage(&AgentLineageRecord {
        child_agent_id: "canonical-child".into(),
        parent_agent_id: "parent".into(),
        creation_cause: AgentLineageCreationCause::Migration,
        revision: 1,
        created_at: canonical_child.created_at,
    })?;

    let children = repository.lineage_children("parent")?;
    assert_eq!(
        children
            .iter()
            .map(|record| record.child_agent_id.as_str())
            .collect::<Vec<_>>(),
        vec!["canonical-child", "legacy-child"],
        "canonical rows win over the legacy identity fallback and results stay sorted"
    );
    assert_eq!(children[1].parent_agent_id, "parent");
    assert_eq!(
        children[1].creation_cause,
        AgentLineageCreationCause::LegacySpawn
    );
    assert!(
        repository.lineage_children("other")?.is_empty(),
        "children of an unrelated parent must not be returned"
    );
    Ok(())
}

#[test]
fn repository_rejects_two_active_supervisors_for_one_child() -> Result<()> {
    let temp_dir = tempdir()?;
    let db_path = temp_dir.path().join("state/runtime.sqlite");
    let lock_path = temp_dir.path().join("state/runtime.lock");
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
    for identity in [
        identity(
            "parent-a",
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        ),
        identity(
            "parent-b",
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        ),
        identity(
            "child",
            AgentKind::Child,
            AgentVisibility::Private,
            AgentOwnership::ParentSupervised,
            AgentProfilePreset::PrivateChild,
            Some("parent-a"),
            Some("task-child"),
        ),
    ] {
        db.agent_identities().upsert(&identity)?;
    }
    let created_at = Utc.with_ymd_and_hms(2026, 9, 6, 0, 0, 0).unwrap();
    let repository = db.agent_canonical_relations();
    repository.upsert_supervision(&AgentSupervisionRecord {
        supervision_id: "supervision-a".into(),
        supervisor_agent_id: "parent-a".into(),
        child_agent_id: "child".into(),
        delegated_from_work_item_id: None,
        delegated_from_task_id: Some("task-child".into()),
        state: AgentSupervisionState::Active,
        revision: 1,
        created_at,
        updated_at: created_at,
    })?;
    let error = repository
        .upsert_supervision(&AgentSupervisionRecord {
            supervision_id: "supervision-b".into(),
            supervisor_agent_id: "parent-b".into(),
            child_agent_id: "child".into(),
            delegated_from_work_item_id: None,
            delegated_from_task_id: Some("task-child".into()),
            state: AgentSupervisionState::Active,
            revision: 1,
            created_at,
            updated_at: created_at,
        })
        .unwrap_err();
    assert!(error.to_string().contains("UNIQUE constraint failed"));
    Ok(())
}

#[test]
fn backfill_dry_run_apply_and_retry_are_idempotent() -> Result<()> {
    let temp_dir = tempdir()?;
    let db_path = temp_dir.path().join("state/runtime.sqlite");
    let lock_path = temp_dir.path().join("state/runtime.lock");
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
    let worker = identity(
        "worker",
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    db.agent_identities().upsert(&worker)?;
    let original_identity_payload: String = db.connection()?.query_row(
        "SELECT payload_json FROM agent_identities WHERE agent_id = 'worker'",
        [],
        |row| row.get(0),
    )?;

    let dry_run = db.agent_canonical_relations().backfill(false, 5)?;
    assert_eq!(dry_run.run_id, None);
    assert_eq!(dry_run.scanned_agents, 1);
    assert_eq!(dry_run.changed_agents, 1);
    assert_eq!(dry_run.migrated_axes, 4);
    assert_eq!(
        db.agent_canonical_relations()
            .latest("worker")?
            .unwrap()
            .sources
            .durability,
        Some(AgentCanonicalValueSource::Legacy)
    );

    let applied = db.agent_canonical_relations().backfill(true, 5)?;
    assert!(applied.run_id.is_some());
    assert_eq!(applied.changed_agents, 1);
    assert_eq!(applied.diagnostic_agents, 0);
    let projection = db.agent_canonical_relations().latest("worker")?.unwrap();
    assert_eq!(
        projection.sources.durability,
        Some(AgentCanonicalValueSource::Canonical)
    );
    assert_eq!(
        projection.sources.lifecycle_attachment,
        Some(AgentCanonicalValueSource::Canonical)
    );
    assert_eq!(
        projection.sources.capability_policy,
        Some(AgentCanonicalValueSource::Canonical)
    );
    assert_eq!(
        projection.sources.message_policy,
        Some(AgentCanonicalValueSource::Canonical)
    );

    let retry = db.agent_canonical_relations().backfill(true, 5)?;
    assert_eq!(retry.changed_agents, 0);
    assert_eq!(retry.unchanged_agents, 1);
    assert_eq!(retry.migrated_axes, 0);
    let identity_payload: String = db.connection()?.query_row(
        "SELECT payload_json FROM agent_identities WHERE agent_id = 'worker'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(identity_payload, original_identity_payload);
    let outcome_count: i64 = db.connection()?.query_row(
        "SELECT COUNT(*) FROM agent_relation_backfill_outcomes",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(outcome_count, 2);
    Ok(())
}

#[test]
fn backfill_resumes_running_run_from_per_agent_checkpoints_after_reopen() -> Result<()> {
    let temp_dir = tempdir()?;
    let db_path = temp_dir.path().join("state/runtime.sqlite");
    let lock_path = temp_dir.path().join("state/runtime.lock");
    let run_id = "agent-relations-interrupted";
    let started_at = Utc.with_ymd_and_hms(2026, 9, 6, 1, 0, 0).unwrap();
    {
        let db = crate::runtime_db::RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        for agent_id in ["agent-a", "agent-b"] {
            db.agent_identities().upsert(&identity(
                agent_id,
                AgentKind::Named,
                AgentVisibility::Public,
                AgentOwnership::SelfOwned,
                AgentProfilePreset::PublicNamed,
                None,
                None,
            ))?;
        }
        db.transaction(|tx| {
            tx.execute(
                "INSERT INTO agent_relation_backfill_runs (
                   run_id, status, started_at, updated_at
                 ) VALUES (?1, 'running', ?2, ?2)",
                params![run_id, timestamp(started_at)],
            )?;
            backfill_agent_relations_tx(tx, "agent-a", true, Some(run_id))?;
            Ok(())
        })?;
    }

    let reopened = crate::runtime_db::RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
    let report = reopened.agent_canonical_relations().backfill(true, 5)?;
    assert_eq!(report.run_id.as_deref(), Some(run_id));
    assert_eq!(report.started_at, started_at);
    assert_eq!(report.scanned_agents, 2);
    assert_eq!(report.changed_agents, 2);
    let run_status: String = reopened.connection()?.query_row(
        "SELECT status FROM agent_relation_backfill_runs WHERE run_id = ?1",
        [run_id],
        |row| row.get(0),
    )?;
    assert_eq!(run_status, "completed");
    let outcome_count: i64 = reopened.connection()?.query_row(
        "SELECT COUNT(*) FROM agent_relation_backfill_outcomes WHERE run_id = ?1",
        [run_id],
        |row| row.get(0),
    )?;
    assert_eq!(outcome_count, 2);
    for agent_id in ["agent-a", "agent-b"] {
        let projection = reopened
            .agent_canonical_relations()
            .latest(agent_id)?
            .unwrap();
        assert_eq!(
            projection.sources.durability,
            Some(AgentCanonicalValueSource::Canonical)
        );
    }
    Ok(())
}

#[test]
fn backfill_report_does_not_migrate_and_apply_can_record_pre_migration_backup() -> Result<()> {
    let temp_dir = tempdir()?;
    let db_path = temp_dir.path().join("state/runtime.sqlite");
    let lock_path = temp_dir.path().join("state/runtime.lock");
    {
        let db = crate::runtime_db::RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        db.agent_identities().upsert(&identity(
            "worker",
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        ))?;
        db.connection()?.execute_batch(
            "DROP TABLE agent_relation_backfill_outcomes;
             DROP TABLE agent_relation_backfill_runs;
             DROP TABLE agent_message_deliveries;
             DELETE FROM schema_migrations WHERE version > 59;",
        )?;
    }

    let inspection =
        crate::runtime_db::RuntimeDb::open_for_agent_relation_backfill(&db_path, &lock_path)?;
    assert_eq!(inspection.current_schema_version()?, 59);
    let report = inspection.agent_canonical_relations().backfill(false, 5)?;
    assert_eq!(report.scanned_agents, 1);
    assert_eq!(inspection.current_schema_version()?, 59);
    let backfill_table_count: i64 = inspection.connection()?.query_row(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'table' AND name = 'agent_relation_backfill_runs'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(backfill_table_count, 0);

    let backup_path = inspection.create_agent_relation_backfill_backup()?;
    assert!(backup_path.is_file());
    let backup_version: i64 = rusqlite::Connection::open(&backup_path)?.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(backup_version, 59);
    drop(inspection);

    let migrated = crate::runtime_db::RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
    let backup_path = backup_path.display().to_string();
    let applied = migrated.agent_canonical_relations().backfill_with_backup(
        true,
        5,
        Some(backup_path.clone()),
    )?;
    assert_eq!(migrated.current_schema_version()?, 62);
    assert_eq!(applied.backup_path.as_deref(), Some(backup_path.as_str()));
    Ok(())
}

#[test]
fn backfill_isolates_missing_supervision_evidence_and_migrates_clear_axes() -> Result<()> {
    let temp_dir = tempdir()?;
    let db_path = temp_dir.path().join("state/runtime.sqlite");
    let lock_path = temp_dir.path().join("state/runtime.lock");
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
    let parent = identity(
        "parent",
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    let child = identity(
        "child",
        AgentKind::Child,
        AgentVisibility::Private,
        AgentOwnership::ParentSupervised,
        AgentProfilePreset::PrivateChild,
        Some("parent"),
        Some("missing-task"),
    );
    db.agent_identities().upsert(&parent)?;
    db.agent_identities().upsert(&child)?;

    let report = db.agent_canonical_relations().backfill(true, 10)?;
    assert_eq!(report.scanned_agents, 2);
    assert_eq!(report.diagnostic_agents, 1);
    let diagnostic = report
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.agent_id == "child")
        .unwrap();
    assert!(diagnostic
        .migrated_axes
        .contains(&AgentCanonicalRelationAxis::Lineage));
    assert!(diagnostic
        .migrated_axes
        .contains(&AgentCanonicalRelationAxis::CapabilityPolicy));
    assert!(!diagnostic
        .migrated_axes
        .contains(&AgentCanonicalRelationAxis::MessagePolicy));
    assert!(diagnostic.issues.iter().any(|issue| {
        issue.axis == AgentCanonicalRelationAxis::Supervision
            && issue.resolution == AgentCanonicalResolution::MissingEvidence
    }));

    let projection = db.agent_canonical_relations().latest("child")?.unwrap();
    assert_eq!(
        projection.sources.lineage,
        Some(AgentCanonicalValueSource::Canonical)
    );
    assert_eq!(projection.supervision, None);
    assert_eq!(
        projection.resolution,
        AgentCanonicalResolution::MissingEvidence
    );
    Ok(())
}

#[test]
fn backfill_uses_durable_task_evidence_for_supervised_children() -> Result<()> {
    let temp_dir = tempdir()?;
    let db_path = temp_dir.path().join("state/runtime.sqlite");
    let lock_path = temp_dir.path().join("state/runtime.lock");
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
    let parent = identity(
        "parent",
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    let child = identity(
        "child",
        AgentKind::Child,
        AgentVisibility::Private,
        AgentOwnership::ParentSupervised,
        AgentProfilePreset::PrivateChild,
        Some("parent"),
        Some("task-child"),
    );
    db.agent_identities().upsert(&parent)?;
    db.agent_identities().upsert(&child)?;
    db.tasks()
        .upsert(&supervision_task("task-child", "parent", "child"))?;

    let report = db.agent_canonical_relations().backfill(true, 10)?;
    assert_eq!(report.diagnostic_agents, 0);
    let projection = db.agent_canonical_relations().latest("child")?.unwrap();
    assert_eq!(projection.resolution, AgentCanonicalResolution::Resolved);
    assert_eq!(
        projection.sources.supervision,
        Some(AgentCanonicalValueSource::Canonical)
    );
    assert_eq!(
        projection
            .supervision
            .as_ref()
            .and_then(|record| record.delegated_from_work_item_id.as_deref()),
        Some("work-parent")
    );
    Ok(())
}

#[test]
fn backfill_preserves_tombstoned_identity_and_task_while_closing_supervision() -> Result<()> {
    let temp_dir = tempdir()?;
    let db_path = temp_dir.path().join("state/runtime.sqlite");
    let lock_path = temp_dir.path().join("state/runtime.lock");
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
    let parent = identity(
        "parent",
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    let mut child = identity(
        "child",
        AgentKind::Child,
        AgentVisibility::Private,
        AgentOwnership::ParentSupervised,
        AgentProfilePreset::PrivateChild,
        Some("parent"),
        Some("task-child"),
    );
    child.status = AgentRegistryStatus::Deleted;
    child.deleted_at = Some(child.updated_at);
    let task = supervision_task("task-child", "parent", "child");
    db.agent_identities().upsert(&parent)?;
    db.agent_identities().upsert(&child)?;
    db.tasks().upsert(&task)?;
    let identity_payload_before: String = db.connection()?.query_row(
        "SELECT payload_json FROM agent_identities WHERE agent_id = 'child'",
        [],
        |row| row.get(0),
    )?;
    let task_payload_before: String = db.connection()?.query_row(
        "SELECT payload_json FROM tasks WHERE task_id = 'task-child'",
        [],
        |row| row.get(0),
    )?;

    let report = db.agent_canonical_relations().backfill(true, 5)?;
    assert_eq!(report.diagnostic_agents, 0);
    let projection = db.agent_canonical_relations().latest("child")?.unwrap();
    assert_eq!(
        projection.identity_lifecycle,
        AgentIdentityLifecycle::Deleted
    );
    assert_eq!(
        projection.lifecycle_fence,
        AgentLifecycleFenceState::Tombstoned
    );
    assert_eq!(
        projection.supervision.as_ref().unwrap().state,
        AgentSupervisionState::Closed
    );
    let identity_payload_after: String = db.connection()?.query_row(
        "SELECT payload_json FROM agent_identities WHERE agent_id = 'child'",
        [],
        |row| row.get(0),
    )?;
    let task_payload_after: String = db.connection()?.query_row(
        "SELECT payload_json FROM tasks WHERE task_id = 'task-child'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(identity_payload_after, identity_payload_before);
    assert_eq!(task_payload_after, task_payload_before);
    Ok(())
}

#[test]
fn deletion_fence_and_finalize_transition_supervision_atomically() -> Result<()> {
    let temp_dir = tempdir()?;
    let db_path = temp_dir.path().join("state/runtime.sqlite");
    let lock_path = temp_dir.path().join("state/runtime.lock");
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
    let parent = identity(
        "parent",
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    let child = identity(
        "child",
        AgentKind::Child,
        AgentVisibility::Private,
        AgentOwnership::ParentSupervised,
        AgentProfilePreset::PrivateChild,
        Some("parent"),
        Some("task-child"),
    );
    db.agent_identities().upsert(&parent)?;
    db.agent_identities().create_with_relations(
        &child,
        &supervised_creation_records(&child, "parent", "task-child", Some("work-parent")),
    )?;

    let (deleting, job, created) =
        db.agent_deletions()
            .begin("child", child.revision, "operator", false)?;
    assert!(created);
    assert_eq!(deleting.status, AgentRegistryStatus::Deleting);
    let fenced = db.agent_canonical_relations().latest("child")?.unwrap();
    assert_eq!(
        fenced.supervision.as_ref().unwrap().state,
        AgentSupervisionState::CleanupRequired
    );
    assert_eq!(
        fenced.supervision.as_ref().unwrap().revision,
        deleting.revision
    );

    let (deleted, completed_job) = db.agent_deletions().finalize(&job)?;
    assert_eq!(deleted.status, AgentRegistryStatus::Deleted);
    assert_eq!(
        completed_job.status,
        crate::types::AgentDeletionStatus::Completed
    );
    let tombstoned = db.agent_canonical_relations().latest("child")?.unwrap();
    assert_eq!(
        tombstoned.supervision.as_ref().unwrap().state,
        AgentSupervisionState::Closed
    );
    assert_eq!(
        tombstoned.supervision.as_ref().unwrap().revision,
        deleted.revision
    );
    Ok(())
}
