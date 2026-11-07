use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    types::AgentVisibility,
};
use std::sync::Arc;

const CHILD: &str = "child-汉字\0\"\\\n";
const PARENT: &str = "parent-汉字\0\"\\\n";
const TASK: &str = "task-汉字\0\"\\\n";
const WORK: &str = "work-汉字\0\"\\\n";

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "legacy-relation-material".into(),
        identity_incarnation: 1,
    }
}

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}

fn cost(text: &str) -> (u64, usize) {
    (((text.len() + std::mem::size_of::<String>()) as u64) * 8, 8)
}

fn identity() -> AgentIdentityRecord {
    let mut identity = AgentIdentityRecord::new(
        CHILD,
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::ParentSupervised,
        AgentProfilePreset::PrivateChild,
        Some(PARENT.into()),
        Some(TASK.into()),
    );
    let now = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00.000Z")
        .unwrap()
        .with_timezone(&Utc);
    identity.created_at = now;
    identity.updated_at = now;
    identity
}

fn evidence() -> LegacySupervisionTaskEvidence {
    LegacySupervisionTaskEvidence {
        task_id: TASK.into(),
        owner_agent_id: PARENT.into(),
        child_agent_id: Some(CHILD.into()),
        is_child_agent_task: true,
        delegated_from_work_item_id: Some(WORK.into()),
    }
}

fn lineage(identity: &AgentIdentityRecord) -> AgentLineageRecord {
    AgentLineageRecord {
        child_agent_id: CHILD.into(),
        parent_agent_id: PARENT.into(),
        creation_cause: AgentLineageCreationCause::LegacySpawn,
        revision: 0,
        created_at: identity.created_at,
    }
}

fn supervision(identity: &AgentIdentityRecord, work: bool) -> AgentSupervisionRecord {
    AgentSupervisionRecord {
        supervision_id: format!("legacy:{TASK}"),
        supervisor_agent_id: PARENT.into(),
        child_agent_id: CHILD.into(),
        delegated_from_work_item_id: work.then(|| WORK.into()),
        delegated_from_task_id: Some(TASK.into()),
        state: match identity.status {
            AgentRegistryStatus::Active => AgentSupervisionState::Active,
            AgentRegistryStatus::Deleting => AgentSupervisionState::CleanupRequired,
            AgentRegistryStatus::Deleted => AgentSupervisionState::Closed,
        },
        revision: 0,
        created_at: identity.created_at,
        updated_at: identity.updated_at,
    }
}

#[test]
fn legacy_relation_ids_admit_each_copy_and_format_with_exact_remaining_budget() -> Result<()> {
    let prior = (13 * 8, 3 * 8);
    for status in [
        AgentRegistryStatus::Active,
        AgentRegistryStatus::Deleting,
        AgentRegistryStatus::Deleted,
    ] {
        let mut identity = identity();
        identity.status = status;
        let lineage = lineage(&identity);
        for axis in 0..3 {
            let mut task = evidence();
            if axis == 2 {
                task.delegated_from_work_item_id = None;
            }
            let expected = if axis == 0 {
                (Some(lineage.clone()), None)
            } else {
                (None, Some(supervision(&identity, axis == 1)))
            };
            let read = || -> Result<_> {
                let mut issues = Vec::new();
                let result = if axis == 0 {
                    (legacy_lineage(&identity, &mut issues)?, None)
                } else {
                    (
                        None,
                        legacy_supervision(&identity, Some(&lineage), Some(&task), &mut issues)?,
                    )
                };
                assert!(issues.is_empty());
                Ok(result)
            };
            assert_eq!(read()?, expected);
            let costs = if axis == 0 {
                vec![cost(CHILD), cost(PARENT)]
            } else {
                let mut costs = vec![cost(&format!("legacy:{TASK}")), cost(PARENT), cost(CHILD)];
                if axis == 1 {
                    costs.push(cost(WORK));
                }
                costs.push(cost(TASK));
                costs
            };
            let exact = costs.iter().copied().fold(add(header(), prior), add);
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase =
                ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
            {
                let _entry = phase.enter();
                charge_owned_metadata(13, 3)?;
                assert_eq!(read()?, expected);
            }
            assert_eq!(phase.finish()?, exact);
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, 0);
            let mut before = add(header(), prior);
            for step in costs {
                let next = add(before, step);
                for bytes in [true, false] {
                    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
                    let phase = ResourceReadMaterial::with_test_limits(
                        &ledger,
                        &scope(),
                        next.0 - u64::from(bytes),
                        next.1 - usize::from(!bytes),
                    )?;
                    let holder = phase.guard()?;
                    {
                        let _entry = phase.enter();
                        charge_owned_metadata(13, 3)?;
                        let error = read().unwrap_err();
                        assert!(
                            format!("{error:#}").contains("ResourceExhausted"),
                            "{error:#}"
                        );
                        assert!(ResourceReadMaterial::active_guard().is_err());
                    }
                    assert!(phase.finish().is_err());
                    assert_eq!(ledger.report()?.observed_bytes, before.0);
                    drop(phase);
                    assert_eq!(ledger.report()?.observed_bytes, before.0);
                    drop(holder);
                    assert_eq!(ledger.report()?.observed_bytes, 0);
                }
                before = next;
            }
        }
    }
    Ok(())
}

#[test]
fn legacy_supervision_borrows_parent_until_all_evidence_matches() -> Result<()> {
    for case in 0..8 {
        let mut identity = identity();
        let lineage = lineage(&identity);
        let mut task = evidence();
        match case {
            1 => identity.delegated_from_task_id = None,
            3 => task.task_id = "wrong".into(),
            4 => task.owner_agent_id = "wrong".into(),
            5 => task.child_agent_id = Some("wrong".into()),
            6 => task.is_child_agent_task = false,
            7 => identity.ownership = Some(AgentOwnership::SelfOwned),
            _ => {}
        }
        let read = |issues: &mut Vec<_>| {
            legacy_supervision(
                &identity,
                (case != 0).then_some(&lineage),
                (case != 2).then_some(&task),
                issues,
            )
        };
        let mut expected_issues = Vec::new();
        assert_eq!(read(&mut expected_issues)?, None);
        assert_eq!(expected_issues.len(), 1);
        let exact = expected_issues.iter().fold(header(), |sum, issue| {
            add(
                sum,
                (
                    (issue.detail.len()
                        + std::mem::size_of::<String>()
                        + std::mem::size_of::<AgentCanonicalProjectionIssue>())
                        as u64
                        * 8,
                    16,
                ),
            )
        });
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
        let mut issues = Vec::new();
        {
            let _entry = phase.enter();
            assert_eq!(read(&mut issues)?, None);
            assert!(ResourceReadMaterial::active_guard()?.is_some());
        }
        assert_eq!(issues, expected_issues);
        // Only diagnostic text and its slot were allocated; no success-ID copy.
        assert_eq!(phase.finish()?, exact);
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn legacy_projection_propagates_admission_and_canonical_axes_skip_derivation() -> Result<()> {
    let identity = identity();
    let task = evidence();
    let records = AgentCanonicalRecordSet {
        lineage: Some(lineage(&identity)),
        supervision: Some(supervision(&identity, true)),
        ..Default::default()
    };
    let baseline = project_agent_canonical_relations(&identity, records.clone(), Some(&task))?;
    // The other four legacy axes and projection ID are independently admitted.
    let additional = [
        cost(CHILD), // durability
        cost(CHILD), // lifecycle attachment
        (
            std::mem::size_of::<AgentCapabilityPolicyRule>() as u64 * 6 * 8,
            6 * 8,
        ),
        cost(CHILD), // capability policy
        (
            std::mem::size_of::<AgentMessagePolicyRule>() as u64 * 3 * 8,
            3 * 8,
        ),
        cost("operator_control"),
        cost("runtime:agent-invocation"),
        cost("agent_invocation"),
        cost(PARENT),
        cost("supervision_follow_up"),
        cost(CHILD), // message policy
        cost(CHILD), // projection
    ]
    .into_iter()
    .fold(header(), add);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase =
        ResourceReadMaterial::with_test_limits(&ledger, &scope(), additional.0, additional.1)?;
    {
        let _entry = phase.enter();
        assert_eq!(
            project_agent_canonical_relations(&identity, records, Some(&task))?,
            baseline
        );
        let error = project_agent_canonical_relations(
            &identity,
            AgentCanonicalRecordSet::default(),
            Some(&task),
        )
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("ResourceExhausted"),
            "{error:#}"
        );
    }
    assert!(phase.finish().is_err());
    assert_eq!(ledger.report()?.observed_bytes, additional.0);
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

fn fixture() -> Result<(tempfile::TempDir, crate::runtime_db::RuntimeDb)> {
    let home = tempfile::tempdir()?;
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    let mut parent = identity();
    parent.agent_id = PARENT.into();
    parent.parent_agent_id = None;
    parent.lineage_parent_agent_id = None;
    parent.delegated_from_task_id = None;
    parent.ownership = Some(AgentOwnership::SelfOwned);
    parent.profile_preset = Some(AgentProfilePreset::PublicNamed);
    db.agent_identities().upsert(&parent)?;
    let identity = identity();
    db.agent_identities().upsert(&identity)?;
    db.tasks().upsert(&TaskRecord {
        id: TASK.into(),
        agent_id: PARENT.into(),
        kind: TaskKind::ChildAgentTask,
        status: crate::types::TaskStatus::Running,
        created_at: identity.created_at,
        updated_at: identity.updated_at,
        parent_message_id: None,
        work_item_id: Some(WORK.into()),
        summary: None,
        detail: Some(serde_json::json!({"child_agent_id": CHILD})),
        recovery: None,
    })?;
    Ok((home, db))
}

#[test]
fn legacy_lineage_admission_reaches_children_and_backfill_callers() -> Result<()> {
    let (_home, db) = fixture()?;
    let mut identity = identity();
    identity.ownership = Some(AgentOwnership::SelfOwned);
    identity.delegated_from_task_id = None;
    db.agent_identities().upsert(&identity)?;
    for backfill in [false, true] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase =
            ResourceReadMaterial::with_test_limits(&ledger, &scope(), header().0, header().1)?;
        {
            let _entry = phase.enter();
            let error = if backfill {
                db.transaction(|tx| backfill_agent_relations_tx(tx, CHILD, true, None))
                    .unwrap_err()
            } else {
                db.agent_canonical_relations()
                    .lineage_children(PARENT)
                    .unwrap_err()
            };
            assert!(
                format!("{error:#}").contains("ResourceExhausted"),
                "{error:#}"
            );
        }
        assert!(phase.finish().is_err());
        assert_eq!(ledger.report()?.observed_bytes, header().0);
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        let count: i64 =
            db.connection()?
                .query_row("SELECT COUNT(*) FROM agent_lineages", [], |r| r.get(0))?;
        assert_eq!(count, 0);
    }
    assert_eq!(
        db.agent_canonical_relations().lineage_children(PARENT)?,
        vec![lineage(&identity)]
    );
    Ok(())
}

#[test]
fn legacy_snapshot_sql_failure_retries_on_original_phase_and_retains_its_holder() -> Result<()> {
    let (_home, db) = fixture()?;
    let baseline = db.agent_projection_snapshot_rows(CHILD)?;
    assert!(baseline._material.is_none());
    let relations = &baseline.row.as_ref().unwrap().canonical_relations;
    assert_eq!(relations.lineage, Some(lineage(&identity())));
    assert_eq!(relations.supervision, Some(supervision(&identity(), true)));
    assert_eq!(
        relations.sources.lineage,
        Some(AgentCanonicalValueSource::Legacy)
    );
    assert_eq!(
        relations.sources.supervision,
        Some(AgentCanonicalValueSource::Legacy)
    );
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let holder = phase.guard()?;
    db.connection()?
        .execute_batch("ALTER TABLE work_items RENAME COLUMN plan_status TO held_plan_status;")?;
    {
        let _entry = phase.enter();
        let error = db.agent_projection_snapshot_rows(CHILD).unwrap_err();
        assert!(format!("{error:#}").contains("plan_status"), "{error:#}");
    }
    let failed_prefix = phase.finish()?;
    assert!(failed_prefix.0 > header().0);
    assert_eq!(ledger.report()?.observed_bytes, failed_prefix.0);
    db.connection()?
        .execute_batch("ALTER TABLE work_items RENAME COLUMN held_plan_status TO plan_status;")?;
    let snapshot = {
        let _entry = phase.enter();
        db.agent_projection_snapshot_rows(CHILD)?
    };
    assert_eq!(snapshot, baseline);
    assert!(Arc::ptr_eq(snapshot._material.as_ref().unwrap(), &holder));
    let charged = phase.finish()?;
    assert!(charged.0 > failed_prefix.0);
    assert!(charged.1 > failed_prefix.1);
    let cloned = snapshot.clone();
    assert_eq!(cloned, snapshot);
    assert_eq!(ledger.report()?.observed_bytes, charged.0 * 2);
    drop((phase, holder, snapshot));
    assert_eq!(ledger.report()?.observed_bytes, charged.0);
    assert_eq!(cloned.row.as_ref().unwrap().canonical_relations, *relations);
    drop(cloned);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
