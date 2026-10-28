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

const AGENT: &str = "policy-汉字\0\"\\\n";
const PARENT: &str = "parent-汉字\0\"\\\n";
type Cost = (u64, usize);

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "legacy-policy-material".into(),
        identity_incarnation: 1,
    }
}

fn header() -> Cost {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn add(a: Cost, b: Cost) -> Cost {
    (a.0 + b.0, a.1 + b.1)
}

fn text_cost(text: &str) -> Cost {
    (((text.len() + std::mem::size_of::<String>()) as u64) * 8, 8)
}

fn slots<T>(count: usize) -> Cost {
    ((std::mem::size_of::<T>() * count) as u64 * 8, count * 8)
}

fn identity() -> AgentIdentityRecord {
    AgentIdentityRecord::new(
        AGENT,
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    )
}

fn supervision(
    identity: &AgentIdentityRecord,
    state: AgentSupervisionState,
) -> AgentSupervisionRecord {
    AgentSupervisionRecord {
        supervision_id: "supervision-policy".into(),
        supervisor_agent_id: PARENT.into(),
        child_agent_id: AGENT.into(),
        delegated_from_work_item_id: None,
        delegated_from_task_id: None,
        state,
        revision: 0,
        created_at: identity.created_at,
        updated_at: identity.updated_at,
    }
}

fn read_axis(
    axis: usize,
    identity: &AgentIdentityRecord,
    supervision: Option<&AgentSupervisionRecord>,
) -> Result<AgentCanonicalRecordSet> {
    let mut records = AgentCanonicalRecordSet::default();
    let mut issues = Vec::new();
    match axis {
        0 => records.durability = legacy_durability(identity, supervision, &mut issues)?,
        1 => {
            records.lifecycle_attachment =
                legacy_lifecycle_attachment(identity, supervision, &mut issues)?
        }
        2 => records.capability_policy = legacy_capability_policy(identity, &mut issues)?,
        3 => records.message_policy = Some(legacy_message_policy(identity, supervision)?),
        _ => unreachable!(),
    }
    assert!(issues.is_empty());
    Ok(records)
}

fn costs(axis: usize, parent: bool) -> Vec<Cost> {
    match axis {
        0 | 1 => vec![text_cost(AGENT)],
        2 => vec![slots::<AgentCapabilityPolicyRule>(6), text_cost(AGENT)],
        3 => {
            let mut steps = vec![
                slots::<AgentMessagePolicyRule>(if parent { 3 } else { 2 }),
                text_cost("operator_control"),
                text_cost("runtime:agent-invocation"),
                text_cost("agent_invocation"),
            ];
            if parent {
                steps.extend([text_cost(PARENT), text_cost("supervision_follow_up")]);
            }
            steps.push(text_cost(AGENT));
            steps
        }
        _ => unreachable!(),
    }
}

#[test]
fn legacy_policy_success_material_exact_and_each_allocation_rejects_before_copy() -> Result<()> {
    let prior = (13 * 8, 3 * 8);
    for axis in 0..4 {
        for case in 0..4 {
            let mut identity = identity();
            if case % 2 == 1 {
                identity.ownership = Some(AgentOwnership::ParentSupervised);
                identity.profile_preset = Some(AgentProfilePreset::PrivateChild);
            }
            if case >= 2 {
                identity.profile_preset = None;
            }
            let supervision = supervision(
                &identity,
                match case {
                    1 => AgentSupervisionState::CleanupRequired,
                    2 => AgentSupervisionState::Closed,
                    _ => AgentSupervisionState::Active,
                },
            );
            let parent = (case != 0).then_some(&supervision);
            let baseline = read_axis(axis, &identity, parent)?;
            let steps = costs(axis, case == 1 || case == 3);
            let exact = steps.iter().copied().fold(add(header(), prior), add);
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase =
                ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
            {
                let _entry = phase.enter();
                charge_owned_metadata(13, 3)?;
                assert_eq!(read_axis(axis, &identity, parent)?, baseline);
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
                    let holder = phase.guard()?;
                    {
                        let _entry = phase.enter();
                        charge_owned_metadata(13, 3)?;
                        let error = read_axis(axis, &identity, parent).unwrap_err();
                        assert!(
                            format!("{error:#}").contains("ResourceExhausted"),
                            "{axis}/{case}: {error:#}"
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
fn legacy_policy_values_preserve_presets_durability_and_parent_rule_states() -> Result<()> {
    for private in [false, true] {
        let mut identity = identity();
        identity.ownership = Some(if private {
            AgentOwnership::ParentSupervised
        } else {
            AgentOwnership::SelfOwned
        });
        // Exercise inference as well as explicit presets in the allocation test.
        identity.profile_preset = None;
        let supervision = supervision(&identity, AgentSupervisionState::Active);
        let mut issues = Vec::new();
        let capability = legacy_capability_policy(&identity, &mut issues)?.unwrap();
        assert_eq!(capability.agent_id, AGENT);
        assert_eq!(capability.revision, 0);
        assert_eq!(capability.created_at, identity.created_at);
        assert_eq!(
            capability.rules,
            [
                AgentCapabilityFamily::CoreAgent,
                AgentCapabilityFamily::LocalEnvironment,
                AgentCapabilityFamily::Web,
                AgentCapabilityFamily::AgentCreation,
                AgentCapabilityFamily::AuthorityExpanding,
                AgentCapabilityFamily::ExternalTrigger,
            ]
            .into_iter()
            .map(|family| AgentCapabilityPolicyRule {
                family,
                effect: if private
                    && matches!(
                        family,
                        AgentCapabilityFamily::AgentCreation
                            | AgentCapabilityFamily::AuthorityExpanding
                    ) {
                    AgentPolicyEffect::Deny
                } else {
                    AgentPolicyEffect::Allow
                },
            })
            .collect::<Vec<_>>()
        );
        for explicit in [
            None,
            Some(crate::types::AgentDurability::Persistent),
            Some(crate::types::AgentDurability::Ephemeral),
        ] {
            identity.durability = explicit;
            assert_eq!(
                legacy_durability(&identity, Some(&supervision), &mut issues)?,
                Some(AgentDurabilityRecord {
                    agent_id: AGENT.into(),
                    durability: match explicit {
                        Some(crate::types::AgentDurability::Ephemeral) => {
                            AgentCanonicalDurability::Ephemeral
                        }
                        Some(crate::types::AgentDurability::Persistent) => {
                            AgentCanonicalDurability::Persistent
                        }
                        None if private => AgentCanonicalDurability::Ephemeral,
                        None => AgentCanonicalDurability::Persistent,
                    },
                    revision: 0,
                    created_at: identity.created_at,
                })
            );
        }
        assert_eq!(
            legacy_lifecycle_attachment(&identity, Some(&supervision), &mut issues)?,
            Some(AgentLifecycleAttachmentRecord {
                agent_id: AGENT.into(),
                attachment: if private {
                    AgentLifecycleAttachment::SupervisionAttached
                } else {
                    AgentLifecycleAttachment::Independent
                },
                revision: 0,
                created_at: identity.created_at,
            })
        );
        assert!(issues.is_empty());
        for state in [
            AgentSupervisionState::Active,
            AgentSupervisionState::CleanupRequired,
            AgentSupervisionState::Closed,
        ] {
            let mut supervision = supervision.clone();
            supervision.state = state;
            let mut rules = vec![
                AgentMessagePolicyRule {
                    principal_kind: AgentMessagePrincipalKind::Operator,
                    principal_id: None,
                    route: Some("operator_control".into()),
                    effect: AgentPolicyEffect::Allow,
                },
                AgentMessagePolicyRule {
                    principal_kind: AgentMessagePrincipalKind::RuntimeCapability,
                    principal_id: Some("runtime:agent-invocation".into()),
                    route: Some("agent_invocation".into()),
                    effect: AgentPolicyEffect::Allow,
                },
            ];
            if state != AgentSupervisionState::Closed {
                rules.push(AgentMessagePolicyRule {
                    principal_kind: AgentMessagePrincipalKind::SupervisingParent,
                    principal_id: Some(PARENT.into()),
                    route: Some("supervision_follow_up".into()),
                    effect: AgentPolicyEffect::Allow,
                });
            }
            assert_eq!(
                legacy_message_policy(&identity, Some(&supervision))?,
                AgentMessagePolicyRecord {
                    agent_id: AGENT.into(),
                    revision: 0,
                    default_effect: AgentPolicyEffect::Deny,
                    rules,
                    created_at: identity.created_at,
                }
            );
        }
    }
    Ok(())
}

#[test]
fn legacy_policy_missing_evidence_does_not_allocate_success_material() -> Result<()> {
    let mut identity = identity();
    identity.ownership = Some(AgentOwnership::ParentSupervised);
    identity.durability = None;
    let mut expected_issues = Vec::new();
    assert!(legacy_durability(&identity, None, &mut expected_issues)?.is_none());
    assert!(legacy_lifecycle_attachment(&identity, None, &mut expected_issues)?.is_none());
    identity.ownership = Some(AgentOwnership::SelfOwned);
    identity.visibility = AgentVisibility::Private;
    identity.profile_preset = None;
    assert!(legacy_capability_policy(&identity, &mut expected_issues)?.is_none());
    assert_eq!(expected_issues.len(), 3);
    let exact = expected_issues.iter().fold(header(), |sum, issue| {
        add(
            sum,
            (
                (issue.detail.len()
                    + std::mem::size_of::<String>()
                    + std::mem::size_of::<AgentCanonicalProjectionIssue>()) as u64
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
        identity.ownership = Some(AgentOwnership::ParentSupervised);
        assert!(legacy_durability(&identity, None, &mut issues)?.is_none());
        assert!(legacy_lifecycle_attachment(&identity, None, &mut issues)?.is_none());
        identity.ownership = Some(AgentOwnership::SelfOwned);
        assert!(legacy_capability_policy(&identity, &mut issues)?.is_none());
    }
    // Missing evidence allocates only diagnostics, not successful relation material.
    assert_eq!(issues, expected_issues);
    assert_eq!(phase.finish()?, exact);
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn projection_id_admission_is_required_even_when_all_policy_axes_are_canonical() -> Result<()> {
    let identity = identity();
    let baseline =
        project_agent_canonical_relations(&identity, AgentCanonicalRecordSet::default(), None)?;
    assert!(baseline.issues.is_empty());
    let canonical = AgentCanonicalRecordSet {
        durability: baseline.durability.clone(),
        lifecycle_attachment: baseline.lifecycle_attachment.clone(),
        capability_policy: baseline.capability_policy.clone(),
        message_policy: baseline.message_policy.clone(),
        ..Default::default()
    };
    let expected = project_agent_canonical_relations(&identity, canonical.clone(), None)?;
    assert_eq!(expected.durability, baseline.durability);
    assert_eq!(expected.lifecycle_attachment, baseline.lifecycle_attachment);
    assert_eq!(expected.capability_policy, baseline.capability_policy);
    assert_eq!(expected.message_policy, baseline.message_policy);
    assert_eq!(
        expected.sources.message_policy,
        Some(AgentCanonicalValueSource::Canonical)
    );
    let prior = (13 * 8, 3 * 8);
    let exact = add(add(header(), prior), text_cost(AGENT));
    for deny in [None, Some(true), Some(false)] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope(),
            exact.0 - u64::from(deny == Some(true)),
            exact.1 - usize::from(deny == Some(false)),
        )?;
        let holder = phase.guard()?;
        let records = canonical.clone();
        {
            let _entry = phase.enter();
            charge_owned_metadata(13, 3)?;
            let result = project_agent_canonical_relations(&identity, records, None);
            if deny.is_some() {
                let error = result.unwrap_err();
                assert!(
                    format!("{error:#}").contains("ResourceExhausted"),
                    "{error:#}"
                );
                assert!(ResourceReadMaterial::active_guard().is_err());
            } else {
                assert_eq!(result?, expected);
            }
        }
        let used = if deny.is_some() {
            assert!(phase.finish().is_err());
            add(header(), prior)
        } else {
            assert_eq!(phase.finish()?, exact);
            exact
        };
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, used.0);
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
