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
