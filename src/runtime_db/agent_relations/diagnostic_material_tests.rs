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
use std::cell::Cell;

const CHILD: &str = "child-汉字\0\"\\\n";
const PARENT: &str = "parent-汉字\0\"\\\n";
const TASK: &str = "task-汉字\0\"\\\n";

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "relation-diagnostic-material".into(),
        identity_incarnation: 1,
    }
}

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}

fn cost(detail: &str) -> (u64, usize) {
    (
        (detail.len()
            + std::mem::size_of::<String>()
            + std::mem::size_of::<AgentCanonicalProjectionIssue>()) as u64
            * 8,
        16,
    )
}

fn identity() -> AgentIdentityRecord {
    AgentIdentityRecord::new(
        CHILD,
        AgentKind::Child,
        AgentVisibility::Private,
        AgentOwnership::ParentSupervised,
        AgentProfilePreset::PrivateChild,
        Some(PARENT.into()),
        Some(TASK.into()),
    )
}

fn append(issues: &mut Vec<AgentCanonicalProjectionIssue>, index: usize) -> Result<()> {
    let axis = AgentCanonicalRelationAxis::Lineage;
    let resolution = AgentCanonicalResolution::Contradictory;
    match index {
        0 => issue(issues, axis, resolution, format_args!("static diagnostic")),
        1 => issue(
            issues,
            axis,
            resolution,
            format_args!("legacy lineage parent {PARENT} conflicts with parent {CHILD}"),
        ),
        2 => issue(
            issues,
            axis,
            resolution,
            format_args!("delegated supervision task {TASK} is missing"),
        ),
        3 => issue(
            issues,
            axis,
            resolution,
            format_args!(
                "supervision state {:?} conflicts with identity lifecycle {:?}",
                AgentSupervisionState::Closed,
                AgentRegistryStatus::Active
            ),
        ),
        _ => issue(issues, axis, resolution, format_args!("")),
    }
}

#[test]
fn diagnostic_text_and_vector_growth_admit_exact_remaining_cost() -> Result<()> {
    let mut expected = Vec::new();
    for index in 0..5 {
        append(&mut expected, index)?;
    }
    assert_eq!(expected[0].detail, "static diagnostic");
    assert_eq!(
        expected[1].detail,
        format!("legacy lineage parent {PARENT} conflicts with parent {CHILD}")
    );
    assert_eq!(
        expected[2].detail,
        format!("delegated supervision task {TASK} is missing")
    );
    assert_eq!(
        expected[3].detail,
        "supervision state Closed conflicts with identity lifecycle Active"
    );
    assert!(expected[4].detail.is_empty());
    let prior = (13 * 8, 3 * 8);
    let exact = expected.iter().fold(add(header(), prior), |sum, item| {
        add(sum, cost(&item.detail))
    });
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
    let mut issues = Vec::new();
    {
        let _entry = phase.enter();
        charge_owned_metadata(13, 3)?;
        for index in 0..5 {
            append(&mut issues, index)?;
        }
    }
    assert_eq!(issues, expected);
    assert_eq!(phase.finish()?, exact);
    drop((issues, phase));
    assert_eq!(ledger.report()?.observed_bytes, 0);

    let mut before = add(header(), prior);
    for (index, expected_issue) in expected.iter().enumerate() {
        let next = add(before, cost(&expected_issue.detail));
        for bytes in [true, false] {
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                next.0 - u64::from(bytes),
                next.1 - usize::from(!bytes),
            )?;
            let holder = phase.guard()?;
            let mut issues = Vec::new();
            {
                let _entry = phase.enter();
                charge_owned_metadata(13, 3)?;
                for previous in 0..index {
                    append(&mut issues, previous)?;
                }
                let capacity = issues.capacity();
                let error = append(&mut issues, index).unwrap_err();
                assert!(format!("{error:#}").contains("ResourceExhausted"));
                assert_eq!(issues.capacity(), capacity);
                assert_eq!(issues, expected[..index]);
                assert!(ResourceReadMaterial::active_guard().is_err());
            }
            assert!(phase.finish().is_err());
            assert_eq!(ledger.report()?.observed_bytes, before.0);
            drop((issues, phase));
            assert_eq!(ledger.report()?.observed_bytes, before.0);
            drop(holder);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        before = next;
    }
    Ok(())
}

#[test]
fn rejected_diagnostic_counts_borrowed_text_without_formatting_or_reserving() -> Result<()> {
    struct Counted<'a>(&'a Cell<usize>);
    impl std::fmt::Display for Counted<'_> {
        fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.0.set(self.0.get() + 1);
            output.write_str(PARENT)
        }
    }
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), header().0, header().1)?;
    let calls = Cell::new(0);
    let mut issues = Vec::new();
    {
        let _entry = phase.enter();
        let error = issue(
            &mut issues,
            AgentCanonicalRelationAxis::Lineage,
            AgentCanonicalResolution::Contradictory,
            format_args!("parent {}", Counted(&calls)),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("ResourceExhausted"));
    }
    assert_eq!(calls.get(), 1);
    assert_eq!(issues.capacity(), 0);
    assert!(issues.is_empty());
    assert!(phase.finish().is_err());
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn legacy_diagnostic_helpers_admit_missing_and_conflicting_evidence() -> Result<()> {
    for case in 0..5 {
        let mut identity = identity();
        let detail = match case {
            0 => {
                identity.parent_agent_id = None;
                identity.lineage_parent_agent_id = None;
                "legacy child identity has no parent evidence"
            }
            1 => {
                identity.ownership = Some(AgentOwnership::SelfOwned);
                "self-owned legacy identity still carries delegated supervision task evidence"
            }
            2 => "parent-supervised durability cannot be resolved without supervision evidence",
            3 => {
                "parent-supervised lifecycle attachment cannot be resolved without supervision evidence"
            }
            _ => {
                identity.ownership = Some(AgentOwnership::SelfOwned);
                identity.profile_preset = None;
                "legacy capability package cannot be resolved"
            }
        };
        let read = |issues: &mut Vec<_>| -> Result<()> {
            match case {
                0 => assert!(legacy_lineage(&identity, issues)?.is_none()),
                1 => assert!(legacy_supervision(&identity, None, None, issues)?.is_none()),
                2 => assert!(legacy_durability(&identity, None, issues)?.is_none()),
                3 => assert!(legacy_lifecycle_attachment(&identity, None, issues)?.is_none()),
                _ => assert!(legacy_capability_policy(&identity, issues)?.is_none()),
            }
            Ok(())
        };
        let mut expected = Vec::new();
        read(&mut expected)?;
        assert_eq!(expected.len(), 1);
        assert_eq!(expected[0].detail, detail);
        let exact = add(header(), cost(detail));
        for reject in [None, Some(true), Some(false)] {
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                exact.0 - u64::from(reject == Some(true)),
                exact.1 - usize::from(reject == Some(false)),
            )?;
            let mut issues = Vec::new();
            {
                let _entry = phase.enter();
                if reject.is_some() {
                    assert!(format!("{:#}", read(&mut issues).unwrap_err())
                        .contains("ResourceExhausted"));
                    assert!(issues.is_empty());
                } else {
                    read(&mut issues)?;
                    assert_eq!(issues, expected);
                }
            }
            if reject.is_some() {
                assert!(phase.finish().is_err());
            } else {
                assert_eq!(phase.finish()?, exact);
            }
            drop((issues, phase));
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn projection_validators_preserve_issue_order_resolution_and_each_rejection() -> Result<()> {
    let mut identity = identity();
    let task = LegacySupervisionTaskEvidence {
        task_id: TASK.into(),
        owner_agent_id: PARENT.into(),
        child_agent_id: Some(CHILD.into()),
        is_child_agent_task: true,
        delegated_from_work_item_id: None,
    };
    let projection = project_agent_canonical_relations(
        &identity,
        AgentCanonicalRecordSet::default(),
        Some(&task),
    )?;
    assert!(projection.issues.is_empty());
    let mut records = AgentCanonicalRecordSet {
        lineage: projection.lineage,
        supervision: projection.supervision,
        durability: projection.durability,
        lifecycle_attachment: projection.lifecycle_attachment,
        capability_policy: projection.capability_policy,
        message_policy: projection.message_policy,
    };
    records.lineage.as_mut().unwrap().parent_agent_id = "different".into();
    records.supervision.as_mut().unwrap().supervisor_agent_id = "different".into();
    records.durability.as_mut().unwrap().durability = AgentCanonicalDurability::Persistent;
    records.lifecycle_attachment.as_mut().unwrap().attachment =
        AgentLifecycleAttachment::Independent;
    identity.profile_preset = Some(AgentProfilePreset::PublicNamed);
    identity.durability = Some(crate::types::AgentDurability::Ephemeral);
    identity.status = AgentRegistryStatus::Deleted;
    let expected = project_agent_canonical_relations(&identity, records.clone(), Some(&task))?;
    let details = [
        (
            AgentCanonicalRelationAxis::LifecycleAttachment,
            "public_named legacy preset conflicts with supervision evidence",
        ),
        (
            AgentCanonicalRelationAxis::Lineage,
            "canonical lineage differs from legacy parent evidence",
        ),
        (
            AgentCanonicalRelationAxis::Supervision,
            "canonical supervision differs from legacy ownership, parent, or task evidence",
        ),
        (
            AgentCanonicalRelationAxis::Durability,
            "canonical durability differs from explicit legacy durability",
        ),
        (
            AgentCanonicalRelationAxis::LifecycleAttachment,
            "canonical lifecycle attachment differs from legacy ownership evidence",
        ),
        (
            AgentCanonicalRelationAxis::Supervision,
            "supervision state Active conflicts with identity lifecycle Deleted",
        ),
        (
            AgentCanonicalRelationAxis::LifecycleAttachment,
            "independent lifecycle conflicts with active supervision",
        ),
        (
            AgentCanonicalRelationAxis::Durability,
            "persistent durability conflicts with first-release active supervision",
        ),
    ];
    assert_eq!(expected.issues.len(), details.len());
    for (issue, (axis, detail)) in expected.issues.iter().zip(details) {
        assert_eq!(issue.axis, axis);
        assert_eq!(issue.detail, detail);
        assert_eq!(issue.resolution, AgentCanonicalResolution::Contradictory);
    }
    assert_eq!(expected.resolution, AgentCanonicalResolution::Contradictory);
    let mut steps: Vec<_> = details.iter().map(|(_, detail)| cost(detail)).collect();
    steps.push(((CHILD.len() + std::mem::size_of::<String>()) as u64 * 8, 8));
    let prior = (13 * 8, 3 * 8);
    let exact = steps.iter().copied().fold(add(header(), prior), add);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
    let owned = records.clone();
    {
        let _entry = phase.enter();
        charge_owned_metadata(13, 3)?;
        assert_eq!(
            project_agent_canonical_relations(&identity, owned, Some(&task))?,
            expected
        );
    }
    assert_eq!(phase.finish()?, exact);
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let mut before = add(header(), prior);
    for step in steps {
        let next = add(before, step);
        for bytes in [true, false] {
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                next.0 - u64::from(bytes),
                next.1 - usize::from(!bytes),
            )?;
            let owned = records.clone();
            {
                let _entry = phase.enter();
                charge_owned_metadata(13, 3)?;
                let error =
                    project_agent_canonical_relations(&identity, owned, Some(&task)).unwrap_err();
                assert!(format!("{error:#}").contains("ResourceExhausted"));
                assert!(ResourceReadMaterial::active_guard().is_err());
            }
            assert!(phase.finish().is_err());
            assert_eq!(ledger.report()?.observed_bytes, before.0);
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        before = next;
    }
    // Cleanup debt is still allowed for an otherwise active identity.
    identity.status = AgentRegistryStatus::Active;
    records.supervision.as_mut().unwrap().state = AgentSupervisionState::CleanupRequired;
    let mut issues = Vec::new();
    validate_effective_relations(
        &identity,
        records.supervision.as_ref(),
        None,
        None,
        &mut issues,
    )?;
    assert!(issues.is_empty());
    Ok(())
}
