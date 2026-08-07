//! Deterministic, model-free evaluation for prompt projections.
//!
//! The manifest records semantic inputs and budget decisions. It intentionally
//! does not parse rendered Markdown and does not participate in provider
//! request lowering.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    prompt::{EffectivePrompt, PromptSection, PromptStability},
    token_estimate::estimate_text_tokens,
};

pub const PROJECTION_MANIFEST_SCHEMA_VERSION: u32 = 1;
pub const PROJECTION_SCORECARD_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProjectionOwner {
    WorkItem { work_item_id: String },
    Conversation { interaction_id: String },
    AgentLifecycle { agent_id: String },
    LegacyUnbound { agent_id: String },
    Command,
}

impl ProjectionOwner {
    fn key(&self) -> String {
        match self {
            Self::WorkItem { work_item_id } => format!("work_item:{work_item_id}"),
            Self::Conversation { interaction_id } => format!("conversation:{interaction_id}"),
            Self::AgentLifecycle { agent_id } => format!("agent_lifecycle:{agent_id}"),
            Self::LegacyUnbound { agent_id } => format!("legacy_unbound:{agent_id}"),
            Self::Command => "command".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionBindingSummary {
    pub source_message_id: String,
    pub turn_id: String,
    pub work_item_id: Option<String>,
    pub claimed_work_revision: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionEvidenceRole {
    CurrentInput,
    DirectPredecessor,
    Turn,
    Input,
    Result,
    ToolResult,
    WorkItemState,
    WaitState,
    Lifecycle,
    Supporting,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ProjectionEvidenceRef {
    pub reference: String,
    pub role: ProjectionEvidenceRole,
    pub owner: ProjectionOwner,
}

impl ProjectionEvidenceRef {
    pub fn new(
        reference: impl Into<String>,
        role: ProjectionEvidenceRole,
        owner: ProjectionOwner,
    ) -> Self {
        Self {
            reference: reference.into(),
            role,
            owner,
        }
    }
}

pub type ProjectionEvidenceIndex = BTreeMap<String, Vec<ProjectionEvidenceRef>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionRepresentation {
    Full,
    Compact,
    Truncated,
    Omitted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionSectionManifest {
    pub order: usize,
    pub section_id: String,
    pub section_name: String,
    pub stability: PromptStability,
    pub representation: ProjectionRepresentation,
    pub reason: String,
    pub requested_estimated_tokens: usize,
    pub allocated_estimated_tokens: usize,
    pub rendered_chars: usize,
    pub content_sha256: Option<String>,
    pub selected_evidence_refs: Vec<ProjectionEvidenceRef>,
    pub omitted_evidence_refs: Vec<ProjectionEvidenceRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionInvariantStatus {
    Pass,
    Fail,
    NotApplicable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionInvariantResult {
    pub code: String,
    pub status: ProjectionInvariantStatus,
    pub details: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionManifest {
    pub schema_version: u32,
    pub projector: String,
    pub activation_owner: ProjectionOwner,
    pub activation_binding: Option<ProjectionBindingSummary>,
    pub turn_id: Option<String>,
    pub prompt_budget_estimated_tokens: usize,
    pub allocated_estimated_tokens: usize,
    pub system_sections: Vec<ProjectionSectionManifest>,
    pub context_sections: Vec<ProjectionSectionManifest>,
    pub invariant_results: Vec<ProjectionInvariantResult>,
}

impl ProjectionManifest {
    pub fn canonical_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self).map(|json| format!("{json}\n"))
    }

    pub fn byte_sha256(&self) -> serde_json::Result<String> {
        self.canonical_json()
            .map(|json| format!("{:x}", Sha256::digest(json.as_bytes())))
    }

    pub fn evaluate(mut self) -> Self {
        self.invariant_results = evaluate_manifest(&self);
        self
    }

    fn active_evidence(&self) -> impl Iterator<Item = &ProjectionEvidenceRef> {
        self.system_sections
            .iter()
            .chain(&self.context_sections)
            .flat_map(|section| section.selected_evidence_refs.iter())
    }

    fn all_evidence(&self) -> impl Iterator<Item = &ProjectionEvidenceRef> {
        self.system_sections
            .iter()
            .chain(&self.context_sections)
            .flat_map(|section| {
                section
                    .selected_evidence_refs
                    .iter()
                    .chain(&section.omitted_evidence_refs)
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionManifestDiff {
    pub baseline_sha256: String,
    pub candidate_sha256: String,
    pub added_selected_evidence_refs: Vec<String>,
    pub removed_selected_evidence_refs: Vec<String>,
    pub changed_sections: Vec<String>,
    pub candidate_failed_invariants: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionScorecard {
    pub schema_version: u32,
    pub passed: bool,
    pub baseline_projector: String,
    pub candidate_projector: String,
    pub diff: ProjectionManifestDiff,
    pub assertions: Vec<ProjectionInvariantResult>,
}

pub fn compare_projection_manifests(
    baseline: &ProjectionManifest,
    candidate: &ProjectionManifest,
) -> serde_json::Result<ProjectionScorecard> {
    let baseline_refs = selected_ref_set(baseline);
    let candidate_refs = selected_ref_set(candidate);
    let added_selected_evidence_refs = candidate_refs
        .difference(&baseline_refs)
        .cloned()
        .collect::<Vec<_>>();
    let removed_selected_evidence_refs = baseline_refs
        .difference(&candidate_refs)
        .cloned()
        .collect::<Vec<_>>();
    let baseline_sections = section_signatures(baseline);
    let candidate_sections = section_signatures(candidate);
    let changed_sections = baseline_sections
        .keys()
        .chain(candidate_sections.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|id| baseline_sections.get(*id) != candidate_sections.get(*id))
        .cloned()
        .collect::<Vec<_>>();
    let candidate_failed_invariants = candidate
        .invariant_results
        .iter()
        .filter(|result| result.status == ProjectionInvariantStatus::Fail)
        .map(|result| result.code.clone())
        .collect::<Vec<_>>();
    let assertions = vec![
        owner_is_unchanged(baseline, candidate),
        binding_is_unchanged(baseline, candidate),
        budget_is_respected(candidate),
        current_input_is_retained(candidate),
    ];
    let passed = candidate_failed_invariants.is_empty()
        && assertions
            .iter()
            .all(|assertion| assertion.status != ProjectionInvariantStatus::Fail);

    Ok(ProjectionScorecard {
        schema_version: PROJECTION_SCORECARD_SCHEMA_VERSION,
        passed,
        baseline_projector: baseline.projector.clone(),
        candidate_projector: candidate.projector.clone(),
        diff: ProjectionManifestDiff {
            baseline_sha256: baseline.byte_sha256()?,
            candidate_sha256: candidate.byte_sha256()?,
            added_selected_evidence_refs,
            removed_selected_evidence_refs,
            changed_sections,
            candidate_failed_invariants,
        },
        assertions,
    })
}

pub fn compare_budget_monotonicity(
    larger_budget: &ProjectionManifest,
    smaller_budget: &ProjectionManifest,
) -> ProjectionInvariantResult {
    if smaller_budget.prompt_budget_estimated_tokens >= larger_budget.prompt_budget_estimated_tokens
    {
        return invariant(
            "budget_monotonic_degradation",
            ProjectionInvariantStatus::NotApplicable,
            ["manifests are not ordered from larger to smaller budget"],
        );
    }
    let larger = selected_ref_set(larger_budget);
    let smaller = selected_ref_set(smaller_budget);
    let unexpected = smaller.difference(&larger).cloned().collect::<Vec<_>>();
    if unexpected.is_empty() {
        invariant(
            "budget_monotonic_degradation",
            ProjectionInvariantStatus::Pass,
            std::iter::empty::<String>(),
        )
    } else {
        invariant(
            "budget_monotonic_degradation",
            ProjectionInvariantStatus::Fail,
            unexpected
                .into_iter()
                .map(|reference| format!("smaller budget introduced {reference}")),
        )
    }
}

pub(crate) fn manifest_from_effective_prompt(prompt: &EffectivePrompt) -> ProjectionManifest {
    let system_sections = prompt
        .system_sections
        .iter()
        .enumerate()
        .map(|(order, section)| selected_section_manifest(order, section))
        .collect();
    let selected_context = prompt
        .context_sections
        .iter()
        .map(|section| (section.id.as_str(), section))
        .collect::<BTreeMap<_, _>>();
    let context_sections = prompt
        .context_plan_evidence
        .decisions
        .iter()
        .enumerate()
        .map(|(order, decision)| {
            let section = selected_context
                .get(decision.candidate_id.as_str())
                .copied();
            let representation = match decision.outcome.label() {
                "full" => ProjectionRepresentation::Full,
                "compact" => ProjectionRepresentation::Compact,
                "truncated" => ProjectionRepresentation::Truncated,
                "omitted" => ProjectionRepresentation::Omitted,
                _ => ProjectionRepresentation::Omitted,
            };
            let evidence = prompt
                .projection_evidence
                .get(&decision.candidate_id)
                .cloned()
                .unwrap_or_default();
            let (selected_evidence_refs, omitted_evidence_refs) =
                if representation == ProjectionRepresentation::Omitted {
                    (Vec::new(), evidence)
                } else {
                    (evidence, Vec::new())
                };
            ProjectionSectionManifest {
                order,
                section_id: decision.candidate_id.clone(),
                section_name: decision.section_name.clone(),
                stability: section
                    .map(|section| section.stability)
                    .unwrap_or(PromptStability::TurnScoped),
                representation,
                reason: decision.reason.code().to_string(),
                requested_estimated_tokens: decision.requested_estimated_tokens,
                allocated_estimated_tokens: decision.allocated_estimated_tokens,
                rendered_chars: section
                    .map(|section| section.content.chars().count())
                    .unwrap_or_default(),
                content_sha256: section.map(section_sha256),
                selected_evidence_refs,
                omitted_evidence_refs,
            }
        })
        .collect();
    ProjectionManifest {
        schema_version: PROJECTION_MANIFEST_SCHEMA_VERSION,
        projector: "legacy_context_sections_v1".to_string(),
        activation_owner: prompt.projection_owner.clone(),
        activation_binding: prompt.projection_binding.clone(),
        turn_id: prompt.projection_turn_id.clone(),
        prompt_budget_estimated_tokens: prompt.context_plan_evidence.total_budget_estimated_tokens,
        allocated_estimated_tokens: prompt.context_plan_evidence.allocated_estimated_tokens,
        system_sections,
        context_sections,
        invariant_results: Vec::new(),
    }
    .evaluate()
}

fn selected_section_manifest(order: usize, section: &PromptSection) -> ProjectionSectionManifest {
    ProjectionSectionManifest {
        order,
        section_id: section.id.clone(),
        section_name: section.name.clone(),
        stability: section.stability,
        representation: ProjectionRepresentation::Full,
        reason: "system_section".to_string(),
        requested_estimated_tokens: estimate_text_tokens(&section.content),
        allocated_estimated_tokens: estimate_text_tokens(&section.content),
        rendered_chars: section.content.chars().count(),
        content_sha256: Some(section_sha256(section)),
        selected_evidence_refs: Vec::new(),
        omitted_evidence_refs: Vec::new(),
    }
}

fn section_sha256(section: &PromptSection) -> String {
    format!("{:x}", Sha256::digest(section.content.as_bytes()))
}

fn evaluate_manifest(manifest: &ProjectionManifest) -> Vec<ProjectionInvariantResult> {
    vec![
        budget_is_respected(manifest),
        activation_binding_is_consistent(manifest),
        current_input_is_retained(manifest),
        direct_predecessor_is_retained(manifest),
        canonical_evidence_is_unique(manifest),
        evidence_owner_is_consistent(manifest),
        representations_are_exclusive(manifest),
    ]
}

fn activation_binding_is_consistent(manifest: &ProjectionManifest) -> ProjectionInvariantResult {
    let Some(binding) = manifest.activation_binding.as_ref() else {
        return invariant(
            "activation_binding_consistent",
            ProjectionInvariantStatus::NotApplicable,
            ["manifest does not include a legacy activation binding"],
        );
    };
    let matches = match binding.work_item_id.as_ref() {
        Some(work_item_id) => {
            manifest.activation_owner
                == (ProjectionOwner::WorkItem {
                    work_item_id: work_item_id.clone(),
                })
        }
        None => matches!(
            manifest.activation_owner,
            ProjectionOwner::LegacyUnbound { .. }
        ),
    };
    if matches {
        invariant(
            "activation_binding_consistent",
            ProjectionInvariantStatus::Pass,
            std::iter::empty::<String>(),
        )
    } else {
        invariant(
            "activation_binding_consistent",
            ProjectionInvariantStatus::Fail,
            [format!(
                "binding work_item_id={} conflicts with owner={}",
                binding.work_item_id.as_deref().unwrap_or("<none>"),
                manifest.activation_owner.key()
            )],
        )
    }
}

fn budget_is_respected(manifest: &ProjectionManifest) -> ProjectionInvariantResult {
    if manifest.allocated_estimated_tokens <= manifest.prompt_budget_estimated_tokens {
        invariant(
            "prompt_budget_respected",
            ProjectionInvariantStatus::Pass,
            std::iter::empty::<String>(),
        )
    } else {
        invariant(
            "prompt_budget_respected",
            ProjectionInvariantStatus::Fail,
            [format!(
                "allocated {} exceeds budget {}",
                manifest.allocated_estimated_tokens, manifest.prompt_budget_estimated_tokens
            )],
        )
    }
}

fn current_input_is_retained(manifest: &ProjectionManifest) -> ProjectionInvariantResult {
    let refs = manifest
        .active_evidence()
        .filter(|evidence| evidence.role == ProjectionEvidenceRole::CurrentInput)
        .map(|evidence| evidence.reference.clone())
        .collect::<Vec<_>>();
    match refs.len() {
        1 => invariant(
            "current_input_retained",
            ProjectionInvariantStatus::Pass,
            std::iter::empty::<String>(),
        ),
        0 => invariant(
            "current_input_retained",
            ProjectionInvariantStatus::Fail,
            ["no selected current-input evidence ref"],
        ),
        _ => invariant(
            "current_input_retained",
            ProjectionInvariantStatus::Fail,
            [format!(
                "current input appears {} times: {}",
                refs.len(),
                refs.join(", ")
            )],
        ),
    }
}

fn direct_predecessor_is_retained(manifest: &ProjectionManifest) -> ProjectionInvariantResult {
    let expected = manifest
        .all_evidence()
        .filter(|evidence| evidence.role == ProjectionEvidenceRole::DirectPredecessor)
        .count();
    let selected = manifest
        .active_evidence()
        .filter(|evidence| evidence.role == ProjectionEvidenceRole::DirectPredecessor)
        .count();
    match (expected, selected) {
        (0, 0) => invariant(
            "direct_predecessor_retained",
            ProjectionInvariantStatus::NotApplicable,
            ["fixture does not identify a direct predecessor"],
        ),
        (1, 1) => invariant(
            "direct_predecessor_retained",
            ProjectionInvariantStatus::Pass,
            std::iter::empty::<String>(),
        ),
        (expected, selected) => invariant(
            "direct_predecessor_retained",
            ProjectionInvariantStatus::Fail,
            [format!(
                "selected {selected} of {expected} direct predecessor refs"
            )],
        ),
    }
}

fn canonical_evidence_is_unique(manifest: &ProjectionManifest) -> ProjectionInvariantResult {
    let mut counts = BTreeMap::<String, usize>::new();
    for evidence in manifest.active_evidence() {
        *counts.entry(evidence.reference.clone()).or_default() += 1;
    }
    let duplicates = counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(reference, count)| format!("{reference} selected {count} times"))
        .collect::<Vec<_>>();
    if duplicates.is_empty() {
        invariant(
            "canonical_evidence_unique",
            ProjectionInvariantStatus::Pass,
            std::iter::empty::<String>(),
        )
    } else {
        invariant(
            "canonical_evidence_unique",
            ProjectionInvariantStatus::Fail,
            duplicates,
        )
    }
}

fn evidence_owner_is_consistent(manifest: &ProjectionManifest) -> ProjectionInvariantResult {
    let expected = manifest.activation_owner.key();
    let mismatches = manifest
        .active_evidence()
        .filter(|evidence| evidence.owner != manifest.activation_owner)
        .map(|evidence| {
            format!(
                "{} owner={} expected={expected}",
                evidence.reference,
                evidence.owner.key()
            )
        })
        .collect::<Vec<_>>();
    if mismatches.is_empty() {
        invariant(
            "evidence_owner_consistent",
            ProjectionInvariantStatus::Pass,
            std::iter::empty::<String>(),
        )
    } else {
        invariant(
            "evidence_owner_consistent",
            ProjectionInvariantStatus::Fail,
            mismatches,
        )
    }
}

fn representations_are_exclusive(manifest: &ProjectionManifest) -> ProjectionInvariantResult {
    let duplicates = manifest
        .context_sections
        .iter()
        .map(|section| section.section_id.as_str())
        .fold(BTreeMap::<&str, usize>::new(), |mut counts, id| {
            *counts.entry(id).or_default() += 1;
            counts
        })
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(id, count)| format!("{id} has {count} representations"))
        .collect::<Vec<_>>();
    if duplicates.is_empty() {
        invariant(
            "section_representation_exclusive",
            ProjectionInvariantStatus::Pass,
            std::iter::empty::<String>(),
        )
    } else {
        invariant(
            "section_representation_exclusive",
            ProjectionInvariantStatus::Fail,
            duplicates,
        )
    }
}

fn owner_is_unchanged(
    baseline: &ProjectionManifest,
    candidate: &ProjectionManifest,
) -> ProjectionInvariantResult {
    if baseline.activation_owner == candidate.activation_owner {
        invariant(
            "activation_owner_unchanged",
            ProjectionInvariantStatus::Pass,
            std::iter::empty::<String>(),
        )
    } else {
        invariant(
            "activation_owner_unchanged",
            ProjectionInvariantStatus::Fail,
            [format!(
                "baseline={} candidate={}",
                baseline.activation_owner.key(),
                candidate.activation_owner.key()
            )],
        )
    }
}

fn binding_is_unchanged(
    baseline: &ProjectionManifest,
    candidate: &ProjectionManifest,
) -> ProjectionInvariantResult {
    if baseline.activation_binding == candidate.activation_binding {
        invariant(
            "activation_binding_unchanged",
            ProjectionInvariantStatus::Pass,
            std::iter::empty::<String>(),
        )
    } else {
        invariant(
            "activation_binding_unchanged",
            ProjectionInvariantStatus::Fail,
            ["candidate changed the legacy activation binding summary"],
        )
    }
}

fn invariant(
    code: impl Into<String>,
    status: ProjectionInvariantStatus,
    details: impl IntoIterator<Item = impl Into<String>>,
) -> ProjectionInvariantResult {
    ProjectionInvariantResult {
        code: code.into(),
        status,
        details: details.into_iter().map(Into::into).collect(),
    }
}

fn selected_ref_set(manifest: &ProjectionManifest) -> BTreeSet<String> {
    manifest
        .active_evidence()
        .map(|evidence| evidence.reference.clone())
        .collect()
}

fn section_signatures(
    manifest: &ProjectionManifest,
) -> BTreeMap<String, (ProjectionRepresentation, Option<String>, usize)> {
    manifest
        .context_sections
        .iter()
        .map(|section| {
            (
                section.section_id.clone(),
                (
                    section.representation,
                    section.content_sha256.clone(),
                    section.allocated_estimated_tokens,
                ),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(reference: &str, role: ProjectionEvidenceRole) -> ProjectionEvidenceRef {
        ProjectionEvidenceRef::new(
            reference,
            role,
            ProjectionOwner::AgentLifecycle {
                agent_id: "agent-1".into(),
            },
        )
    }

    fn manifest(refs: Vec<ProjectionEvidenceRef>) -> ProjectionManifest {
        ProjectionManifest {
            schema_version: PROJECTION_MANIFEST_SCHEMA_VERSION,
            projector: "test".into(),
            activation_owner: ProjectionOwner::AgentLifecycle {
                agent_id: "agent-1".into(),
            },
            activation_binding: None,
            turn_id: Some("turn-1".into()),
            prompt_budget_estimated_tokens: 100,
            allocated_estimated_tokens: 20,
            system_sections: Vec::new(),
            context_sections: vec![ProjectionSectionManifest {
                order: 0,
                section_id: "current_input".into(),
                section_name: "current_input".into(),
                stability: PromptStability::TurnScoped,
                representation: ProjectionRepresentation::Full,
                reason: "selected_full".into(),
                requested_estimated_tokens: 20,
                allocated_estimated_tokens: 20,
                rendered_chars: 40,
                content_sha256: Some("abc".into()),
                selected_evidence_refs: refs,
                omitted_evidence_refs: Vec::new(),
            }],
            invariant_results: Vec::new(),
        }
        .evaluate()
    }

    fn omitted_manifest(refs: Vec<ProjectionEvidenceRef>) -> ProjectionManifest {
        let mut manifest = manifest(vec![evidence(
            "message:current",
            ProjectionEvidenceRole::CurrentInput,
        )]);
        manifest.context_sections.push(ProjectionSectionManifest {
            order: 1,
            section_id: "omitted".into(),
            section_name: "omitted".into(),
            stability: PromptStability::TurnScoped,
            representation: ProjectionRepresentation::Omitted,
            reason: "omitted_lower_priority".into(),
            requested_estimated_tokens: 20,
            allocated_estimated_tokens: 0,
            rendered_chars: 0,
            content_sha256: None,
            selected_evidence_refs: Vec::new(),
            omitted_evidence_refs: refs,
        });
        manifest.evaluate()
    }

    #[test]
    fn canonical_json_is_byte_stable() {
        let manifest = manifest(vec![evidence(
            "message:current",
            ProjectionEvidenceRole::CurrentInput,
        )]);

        assert_eq!(
            manifest.canonical_json().unwrap(),
            manifest.canonical_json().unwrap()
        );
        assert_eq!(
            manifest.byte_sha256().unwrap(),
            manifest.byte_sha256().unwrap()
        );
    }

    #[test]
    fn restart_rebuild_is_byte_equivalent() {
        let before = manifest(vec![
            evidence("message:current", ProjectionEvidenceRole::CurrentInput),
            evidence("message:prior", ProjectionEvidenceRole::DirectPredecessor),
        ]);
        let rebuilt: ProjectionManifest =
            serde_json::from_str(&before.canonical_json().unwrap()).unwrap();

        assert_eq!(
            before.canonical_json().unwrap(),
            rebuilt.canonical_json().unwrap()
        );
        assert_eq!(
            before.byte_sha256().unwrap(),
            rebuilt.byte_sha256().unwrap()
        );
    }

    #[test]
    fn self_test_detects_duplicate_ref() {
        let current = evidence("message:current", ProjectionEvidenceRole::CurrentInput);
        let manifest = manifest(vec![current.clone(), current]);

        assert!(manifest.invariant_results.iter().any(|result| {
            result.code == "canonical_evidence_unique"
                && result.status == ProjectionInvariantStatus::Fail
        }));
    }

    #[test]
    fn self_test_detects_wrong_owner() {
        let mut current = evidence("message:current", ProjectionEvidenceRole::CurrentInput);
        current.owner = ProjectionOwner::WorkItem {
            work_item_id: "other".into(),
        };
        let manifest = manifest(vec![current]);

        assert!(manifest.invariant_results.iter().any(|result| {
            result.code == "evidence_owner_consistent"
                && result.status == ProjectionInvariantStatus::Fail
        }));
    }

    #[test]
    fn self_test_detects_owner_binding_mismatch() {
        let mut manifest = manifest(vec![evidence(
            "message:current",
            ProjectionEvidenceRole::CurrentInput,
        )]);
        manifest.activation_binding = Some(ProjectionBindingSummary {
            source_message_id: "message:current".into(),
            turn_id: "turn-1".into(),
            work_item_id: Some("work-1".into()),
            claimed_work_revision: Some(1),
        });
        let manifest = manifest.evaluate();

        assert!(manifest.invariant_results.iter().any(|result| {
            result.code == "activation_binding_consistent"
                && result.status == ProjectionInvariantStatus::Fail
        }));
    }

    #[test]
    fn self_test_detects_missing_current_input() {
        let manifest = manifest(vec![evidence("turn:prior", ProjectionEvidenceRole::Turn)]);

        assert!(manifest.invariant_results.iter().any(|result| {
            result.code == "current_input_retained"
                && result.status == ProjectionInvariantStatus::Fail
        }));
    }

    #[test]
    fn self_test_detects_missing_direct_predecessor() {
        let manifest = omitted_manifest(vec![evidence(
            "message:prior",
            ProjectionEvidenceRole::DirectPredecessor,
        )]);

        assert!(manifest.invariant_results.iter().any(|result| {
            result.code == "direct_predecessor_retained"
                && result.status == ProjectionInvariantStatus::Fail
        }));
    }

    #[test]
    fn self_test_detects_cross_owner_leakage() {
        let mut leaked = evidence("message:foreign", ProjectionEvidenceRole::Supporting);
        leaked.owner = ProjectionOwner::Conversation {
            interaction_id: "other-interaction".into(),
        };
        let manifest = manifest(vec![
            evidence("message:current", ProjectionEvidenceRole::CurrentInput),
            leaked,
        ]);

        assert!(manifest.invariant_results.iter().any(|result| {
            result.code == "evidence_owner_consistent"
                && result.status == ProjectionInvariantStatus::Fail
        }));
    }

    #[test]
    fn self_test_detects_non_monotonic_budget_projection() {
        let larger = manifest(vec![evidence(
            "message:current",
            ProjectionEvidenceRole::CurrentInput,
        )]);
        let mut smaller = manifest(vec![
            evidence("message:current", ProjectionEvidenceRole::CurrentInput),
            evidence("brief:new", ProjectionEvidenceRole::Result),
        ]);
        smaller.prompt_budget_estimated_tokens = 50;

        assert_eq!(
            compare_budget_monotonicity(&larger, &smaller).status,
            ProjectionInvariantStatus::Fail
        );
    }
}
