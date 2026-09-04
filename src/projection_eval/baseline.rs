//! Corpus fixture builder for Phase 0 legacy baseline manifests.
//!
//! Each fixture constructs an `EffectivePrompt` that mirrors the durable-fact
//! scenario described by a frozen corpus case.  The manifest is produced by
//! the real `manifest_from_effective_prompt` code path — no parallel
//! reconstruction of projection logic.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::context::{
    ContextPlanDecision, ContextPlanEvidence, ContextPlanOutcome, ContextPlanReason,
};
use crate::projection_eval::baseline_specs::baseline_case_specs;
use crate::projection_eval::{
    manifest_from_effective_prompt, manifest_from_effective_prompt_with_selector, HistorySelector,
    ProjectionBindingSummary, ProjectionEvidenceIndex, ProjectionEvidenceRef,
    ProjectionEvidenceRole, ProjectionManifest, ProjectionOwner,
};
use crate::prompt::{EffectivePrompt, PromptCacheIdentity, PromptSection, PromptStability};
use crate::system::{ExecutionProfile, ExecutionSnapshot};
use crate::token_estimate::estimate_text_tokens;
use crate::types::{
    AgentIdentityView, AgentKind, AgentOwnership, AgentProfilePreset, AgentRegistryStatus,
    AgentVisibility, LoadedAgentMemory, LoadedAgentsMd,
};

// ---------------------------------------------------------------------------
// Spec types
// ---------------------------------------------------------------------------

/// Priority tier controlling budget-dependent degradation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EvidenceTier {
    /// Current input — always full.
    Current,
    /// Direct predecessor — full or compact.
    Predecessor,
    /// Core dialogue — full, compact, or truncated.
    Core,
    /// Runtime state — full, compact, or omitted.
    Runtime,
    /// Background events — full or omitted.
    Background,
}

#[derive(Debug, Clone)]
pub struct BaselineEvidenceSpec {
    pub reference: String,
    pub role: ProjectionEvidenceRole,
    pub owner: ProjectionOwner,
    pub tier: EvidenceTier,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct BaselineCaseSpec {
    pub case_id: String,
    pub owner: ProjectionOwner,
    pub binding: Option<ProjectionBindingSummary>,
    pub turn_id: Option<String>,
    pub evidence: Vec<BaselineEvidenceSpec>,
    pub forbidden_refs: Vec<String>,
}

// ---------------------------------------------------------------------------
// Budget-tier mapping
// ---------------------------------------------------------------------------

pub const BUDGET_LARGE: usize = 16_384;
pub const BUDGET_MEDIUM: usize = 4_096;
pub const BUDGET_SMALL: usize = 2_048;

pub const BASELINE_BUDGETS: &[usize] = &[BUDGET_SMALL, BUDGET_MEDIUM, BUDGET_LARGE];

/// Determine the representation for an evidence item at a given budget.
/// Forbidden refs are always omitted.
fn representation_for(tier: EvidenceTier, budget: usize, is_forbidden: bool) -> ContextPlanOutcome {
    if is_forbidden {
        return ContextPlanOutcome::Omitted;
    }
    match (tier, budget) {
        (_, BUDGET_LARGE) => ContextPlanOutcome::Full,
        (EvidenceTier::Current, _) => ContextPlanOutcome::Full,
        (EvidenceTier::Predecessor, BUDGET_MEDIUM) => ContextPlanOutcome::Full,
        (EvidenceTier::Core, BUDGET_MEDIUM) => ContextPlanOutcome::Compact,
        (EvidenceTier::Runtime, BUDGET_MEDIUM) => ContextPlanOutcome::Compact,
        (EvidenceTier::Background, BUDGET_MEDIUM) => ContextPlanOutcome::Omitted,
        (EvidenceTier::Predecessor, BUDGET_SMALL) => ContextPlanOutcome::Compact,
        (EvidenceTier::Core, BUDGET_SMALL) => ContextPlanOutcome::Truncated,
        (EvidenceTier::Runtime, BUDGET_SMALL) => ContextPlanOutcome::Omitted,
        _ => ContextPlanOutcome::Omitted,
    }
}

fn reason_for(outcome: ContextPlanOutcome) -> ContextPlanReason {
    match outcome {
        ContextPlanOutcome::Full => ContextPlanReason::SelectedFull,
        ContextPlanOutcome::Compact => ContextPlanReason::SelectedCompactForBudget,
        ContextPlanOutcome::Truncated => ContextPlanReason::TruncatedToRemainingBudget,
        ContextPlanOutcome::Omitted => ContextPlanReason::OmittedLowerPriority,
    }
}

// ---------------------------------------------------------------------------
// Prompt construction
// ---------------------------------------------------------------------------

fn baseline_identity() -> AgentIdentityView {
    AgentIdentityView {
        agent_id: "baseline-agent".into(),
        kind: AgentKind::Default,
        visibility: AgentVisibility::Public,
        ownership: AgentOwnership::SelfOwned,
        profile_preset: AgentProfilePreset::PublicNamed,
        status: AgentRegistryStatus::Active,
        is_default_agent: true,
        parent_agent_id: None,
        lineage_parent_agent_id: None,
        delegated_from_task_id: None,
    }
}

fn baseline_execution() -> ExecutionSnapshot {
    ExecutionSnapshot {
        profile: ExecutionProfile::default(),
        policy: ExecutionProfile::default().policy_snapshot(),
        attached_workspaces: vec![],
        workspace_id: None,
        workspace_anchor: PathBuf::from("/tmp/baseline"),
        execution_root: PathBuf::from("/tmp/baseline"),
        cwd: PathBuf::from("/tmp/baseline"),
        execution_root_id: None,
        projection_kind: None,
        access_mode: None,
        worktree_root: None,
        execution_roots: Vec::new(),
    }
}

/// Build an `EffectivePrompt` fixture from a case spec at a given budget.
/// The prompt flows through the real `manifest_from_effective_prompt`.
pub fn build_baseline_prompt(spec: &BaselineCaseSpec, budget: usize) -> EffectivePrompt {
    let forbidden: std::collections::BTreeSet<&str> =
        spec.forbidden_refs.iter().map(|r| r.as_str()).collect();

    let mut context_sections: Vec<PromptSection> = Vec::new();
    let mut projection_evidence: ProjectionEvidenceIndex = BTreeMap::new();
    let mut decisions: Vec<ContextPlanDecision> = Vec::new();
    let mut allocated = 0usize;

    for ev in &spec.evidence {
        let is_forbidden = forbidden.contains(ev.reference.as_str());
        let outcome = representation_for(ev.tier, budget, is_forbidden);
        let reason = reason_for(outcome);
        let tokens = estimate_text_tokens(&ev.content);

        let (allocated_tokens, _rendered_chars, content) = match outcome {
            ContextPlanOutcome::Full => (tokens, ev.content.chars().count(), ev.content.clone()),
            ContextPlanOutcome::Compact => {
                let compact = format!("[compact] {}", ev.content);
                let compact_tokens = estimate_text_tokens(&compact);
                (compact_tokens, compact.chars().count(), compact)
            }
            ContextPlanOutcome::Truncated => {
                let truncated: String = format!(
                    "[truncated] {}",
                    ev.content.chars().take(40).collect::<String>()
                );
                let trunc_tokens = estimate_text_tokens(&truncated);
                (trunc_tokens, truncated.chars().count(), truncated)
            }
            ContextPlanOutcome::Omitted => (0, 0, String::new()),
        };
        allocated += allocated_tokens;

        let section = PromptSection {
            name: ev.reference.clone(),
            id: ev.reference.clone(),
            content,
            stability: if ev.tier == EvidenceTier::Current {
                PromptStability::TurnScoped
            } else {
                PromptStability::AgentScoped
            },
        };

        let evidence_ref = ProjectionEvidenceRef {
            reference: ev.reference.clone(),
            role: ev.role,
            owner: ev.owner.clone(),
        };

        if outcome != ContextPlanOutcome::Omitted {
            context_sections.push(section);
        }
        projection_evidence
            .entry(ev.reference.clone())
            .or_default()
            .push(evidence_ref);

        decisions.push(ContextPlanDecision {
            candidate_id: ev.reference.clone(),
            section_name: ev.reference.clone(),
            requested_estimated_tokens: tokens,
            minimum_estimated_tokens: tokens,
            allocated_estimated_tokens: allocated_tokens,
            outcome,
            reason,
        });
    }

    let plan_evidence = ContextPlanEvidence {
        total_budget_estimated_tokens: budget,
        allocated_estimated_tokens: allocated,
        decisions,
    };

    let system_section = PromptSection {
        name: "identity".into(),
        id: "identity".into(),
        content: "Baseline system prompt for projection-eval.".into(),
        stability: PromptStability::Stable,
    };

    EffectivePrompt {
        agent_home: PathBuf::from("/tmp/baseline-agent"),
        identity: baseline_identity(),
        execution: baseline_execution(),
        loaded_agents_md: LoadedAgentsMd::default(),
        cache_identity: PromptCacheIdentity {
            agent_id: "baseline-agent".into(),
            prompt_cache_key: "baseline".into(),
            context_fingerprint: "baseline-fingerprint".into(),
            compression_epoch: 0,
        },
        loaded_agent_memory: LoadedAgentMemory::default(),
        system_sections: vec![system_section],
        context_sections,
        rendered_system_prompt: "Baseline system prompt for projection-eval.".into(),
        rendered_context_attachment: String::new(),
        projection_owner: spec.owner.clone(),
        projection_binding: spec.binding.clone(),
        projection_turn_id: spec.turn_id.clone(),
        projection_evidence,
        context_plan_evidence: plan_evidence,
        recent_turns_reprojection: None,
    }
}

/// Generate a manifest from a case spec at a given budget.
pub fn generate_manifest(spec: &BaselineCaseSpec, budget: usize) -> ProjectionManifest {
    let prompt = build_baseline_prompt(spec, budget);
    manifest_from_effective_prompt(&prompt)
}

/// Generate a request-scoped manifest for a specific history selector.
pub fn generate_manifest_for_selector(
    spec: &BaselineCaseSpec,
    budget: usize,
    selector: HistorySelector,
) -> ProjectionManifest {
    let prompt = build_baseline_prompt(spec, budget);
    manifest_from_effective_prompt_with_selector(&prompt, selector)
}

/// Generate all 36 baseline manifests (12 cases x 3 budgets).
pub fn generate_all_manifests() -> Vec<(String, usize, ProjectionManifest)> {
    let specs = baseline_case_specs();
    let mut results = Vec::with_capacity(specs.len() * BASELINE_BUDGETS.len());
    for spec in &specs {
        for &budget in BASELINE_BUDGETS {
            let manifest = generate_manifest(spec, budget);
            results.push((spec.case_id.clone(), budget, manifest));
        }
    }
    results
}

fn phase_1_owner(case_id: &str, owner: &ProjectionOwner) -> ProjectionOwner {
    match owner {
        ProjectionOwner::LegacyUnbound { agent_id } => ProjectionOwner::Conversation {
            interaction_id: crate::ids::interaction_id(&[
                agent_id,
                "projection_eval_phase_1",
                case_id,
            ]),
        },
        owner => owner.clone(),
    }
}

fn phase_1_evidence_owner(case_id: &str, evidence: &BaselineEvidenceSpec) -> ProjectionOwner {
    if evidence.role == ProjectionEvidenceRole::Lifecycle
        || matches!(
            evidence.reference.as_str(),
            "message:external-wake" | "message:timer-wake" | "message:lifecycle-wake"
        )
    {
        return ProjectionOwner::AgentLifecycle {
            agent_id: "agent-1".into(),
        };
    }
    phase_1_owner(case_id, &evidence.owner)
}

pub fn phase_1_case_specs() -> Vec<BaselineCaseSpec> {
    baseline_case_specs()
        .into_iter()
        .map(|mut spec| {
            spec.owner = phase_1_owner(&spec.case_id, &spec.owner);
            if let Some(binding) = spec.binding.as_mut() {
                binding.owner = Some(spec.owner.clone());
            }
            for evidence in &mut spec.evidence {
                evidence.owner = phase_1_evidence_owner(&spec.case_id, evidence);
            }
            spec
        })
        .collect()
}

pub fn generate_all_phase_1_manifests() -> Vec<(String, usize, ProjectionManifest)> {
    let specs = phase_1_case_specs();
    let mut results = Vec::with_capacity(specs.len() * BASELINE_BUDGETS.len());
    for spec in &specs {
        for &budget in BASELINE_BUDGETS {
            results.push((
                spec.case_id.clone(),
                budget,
                generate_manifest(spec, budget),
            ));
        }
    }
    results
}

/// Manifest filename for a case + budget.
pub fn manifest_filename(case_id: &str, budget: usize) -> String {
    format!("{case_id}-{budget}.json")
}

/// Deterministic scorecard entry for one case x budget manifest.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BaselineScorecardEntry {
    pub case_id: String,
    pub budget: usize,
    pub manifest: String,
    pub sha256: String,
    pub invariants_pass: bool,
    pub failed_invariants: Vec<String>,
}

/// Deterministic scorecard for all 36 baseline manifests.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BaselineScorecard {
    pub schema_version: u32,
    pub baseline_id: String,
    pub projector: String,
    pub corpus: String,
    pub rubric: String,
    pub frozen_at: String,
    pub status: String,
    pub manifest_count: usize,
    pub entries: Vec<BaselineScorecardEntry>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Phase1ScorecardEntry {
    pub case_id: String,
    pub budget: usize,
    pub manifest: String,
    pub baseline_sha256: String,
    pub candidate_sha256: String,
    pub activation_owner_changed: bool,
    pub provider_sections_byte_identical: bool,
    pub invariants_pass: bool,
    pub failed_invariants: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Phase1Scorecard {
    pub schema_version: u32,
    pub candidate_id: String,
    pub baseline_id: String,
    pub projector: String,
    pub corpus: String,
    pub rubric: String,
    pub generated_at: String,
    pub status: String,
    pub manifest_count: usize,
    pub provider_sections_byte_identical: bool,
    pub entries: Vec<Phase1ScorecardEntry>,
}

fn provider_sections_byte_identical(
    baseline: &ProjectionManifest,
    candidate: &ProjectionManifest,
) -> bool {
    let equivalent =
        |baseline: &crate::projection_eval::ProjectionSectionManifest,
         candidate: &crate::projection_eval::ProjectionSectionManifest| {
            baseline.order == candidate.order
                && baseline.section_id == candidate.section_id
                && baseline.section_name == candidate.section_name
                && baseline.stability == candidate.stability
                && baseline.representation == candidate.representation
                && baseline.reason == candidate.reason
                && baseline.requested_estimated_tokens == candidate.requested_estimated_tokens
                && baseline.allocated_estimated_tokens == candidate.allocated_estimated_tokens
                && baseline.rendered_chars == candidate.rendered_chars
                && baseline.content_sha256 == candidate.content_sha256
        };
    baseline.system_sections.len() == candidate.system_sections.len()
        && baseline.context_sections.len() == candidate.context_sections.len()
        && baseline
            .system_sections
            .iter()
            .zip(&candidate.system_sections)
            .all(|(baseline, candidate)| equivalent(baseline, candidate))
        && baseline
            .context_sections
            .iter()
            .zip(&candidate.context_sections)
            .all(|(baseline, candidate)| equivalent(baseline, candidate))
}

/// Generate the deterministic baseline scorecard.
pub fn generate_scorecard() -> BaselineScorecard {
    let manifests = generate_all_manifests();
    let entries: Vec<_> = manifests
        .iter()
        .map(|(case_id, budget, manifest)| {
            let sha = manifest.byte_sha256().unwrap_or_default();
            let failed: Vec<String> = manifest
                .invariant_results
                .iter()
                .filter(|r| r.status == crate::projection_eval::ProjectionInvariantStatus::Fail)
                .map(|r| r.code.clone())
                .collect();
            BaselineScorecardEntry {
                case_id: case_id.clone(),
                budget: *budget,
                manifest: manifest_filename(case_id, *budget),
                sha256: sha,
                invariants_pass: failed.is_empty(),
                failed_invariants: failed,
            }
        })
        .collect();
    BaselineScorecard {
        schema_version: 1,
        baseline_id: "legacy-context-sections-v1-phase-0".into(),
        projector: "legacy_context_sections_v1".into(),
        corpus: "../corpus/cases.json".into(),
        rubric: "../corpus/rubric.json".into(),
        frozen_at: "2026-08-18".into(),
        status: "frozen_observation".into(),
        manifest_count: entries.len(),
        entries,
    }
}

pub fn generate_phase_1_scorecard() -> Phase1Scorecard {
    let baseline = generate_all_manifests();
    let candidate = generate_all_phase_1_manifests();
    let entries = baseline
        .iter()
        .zip(&candidate)
        .map(
            |((baseline_case, baseline_budget, baseline), (case_id, budget, candidate))| {
                assert_eq!((baseline_case, baseline_budget), (case_id, budget));
                let provider_sections_byte_identical =
                    provider_sections_byte_identical(baseline, candidate);
                let failed_invariants = candidate
                    .invariant_results
                    .iter()
                    .filter(|result| {
                        result.status == crate::projection_eval::ProjectionInvariantStatus::Fail
                    })
                    .map(|result| result.code.clone())
                    .collect::<Vec<_>>();
                Phase1ScorecardEntry {
                    case_id: case_id.clone(),
                    budget: *budget,
                    manifest: manifest_filename(case_id, *budget),
                    baseline_sha256: baseline.byte_sha256().unwrap_or_default(),
                    candidate_sha256: candidate.byte_sha256().unwrap_or_default(),
                    activation_owner_changed: baseline.activation_owner
                        != candidate.activation_owner,
                    provider_sections_byte_identical,
                    invariants_pass: failed_invariants.is_empty(),
                    failed_invariants,
                }
            },
        )
        .collect::<Vec<_>>();
    Phase1Scorecard {
        schema_version: 1,
        candidate_id: "activation-turn-owner-identity-phase-1".into(),
        baseline_id: "legacy-context-sections-v1-phase-0".into(),
        projector: "legacy_context_sections_v1".into(),
        corpus: "../corpus/cases.json".into(),
        rubric: "../corpus/rubric.json".into(),
        generated_at: "2026-08-28".into(),
        status: "owner_identity_only_observation".into(),
        manifest_count: entries.len(),
        provider_sections_byte_identical: entries
            .iter()
            .all(|entry| entry.provider_sections_byte_identical),
        entries,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection_eval::compare_budget_monotonicity;
    use crate::projection_eval::ProjectionInvariantStatus;
    use std::fs;

    fn manifest_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("benchmarks/projection-eval/baseline/manifests")
    }

    fn phase_1_manifest_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("benchmarks/projection-eval/candidate-phase-1/manifests")
    }

    #[test]
    fn all_36_manifests_generate_successfully() {
        let manifests = generate_all_manifests();
        assert_eq!(
            manifests.len(),
            36,
            "expected 12 cases x 3 budgets = 36 manifests"
        );
        for (case_id, budget, manifest) in &manifests {
            assert_eq!(manifest.schema_version, 1, "{case_id}-{budget}");
            assert!(
                !manifest.system_sections.is_empty(),
                "{case_id}-{budget} should have system sections"
            );
            assert!(
                !manifest.context_sections.is_empty(),
                "{case_id}-{budget} should have context sections"
            );
            assert!(
                !manifest.invariant_results.is_empty(),
                "{case_id}-{budget} should have invariant results"
            );
        }
    }

    #[test]
    fn manifests_are_byte_stable() {
        for (case_id, budget, manifest) in generate_all_manifests() {
            let json1 = manifest.canonical_json().unwrap();
            let sha1 = manifest.byte_sha256().unwrap();
            // Deserialize and re-serialize.
            let rebuilt: ProjectionManifest = serde_json::from_str(&json1).unwrap();
            let json2 = rebuilt.canonical_json().unwrap();
            let sha2 = rebuilt.byte_sha256().unwrap();
            assert_eq!(json1, json2, "byte-stability failed for {case_id}-{budget}");
            assert_eq!(sha1, sha2, "sha256 mismatch for {case_id}-{budget}");
        }
    }

    #[test]
    fn restart_reconstruction_is_byte_equivalent() {
        for (case_id, budget, manifest) in generate_all_manifests() {
            let original_json = manifest.canonical_json().unwrap();
            let original_sha = manifest.byte_sha256().unwrap();
            // Simulate restart: rebuild from same spec.
            let specs = baseline_case_specs();
            let spec = specs
                .iter()
                .find(|s| s.case_id == case_id)
                .expect("spec not found");
            let rebuilt = generate_manifest(spec, budget);
            let rebuilt_json = rebuilt.canonical_json().unwrap();
            let rebuilt_sha = rebuilt.byte_sha256().unwrap();
            assert_eq!(
                original_json, rebuilt_json,
                "restart equivalence failed for {case_id}-{budget}"
            );
            assert_eq!(
                original_sha, rebuilt_sha,
                "restart sha mismatch for {case_id}-{budget}"
            );
        }
    }

    #[test]
    fn budget_monotonicity_holds() {
        let specs = baseline_case_specs();
        for spec in &specs {
            let manifests: std::collections::BTreeMap<usize, ProjectionManifest> = BASELINE_BUDGETS
                .iter()
                .map(|&b| (b, generate_manifest(spec, b)))
                .collect();
            let budgets: Vec<usize> = BASELINE_BUDGETS.to_vec();
            // Compare larger vs smaller: smaller must select no new evidence.
            for i in 0..budgets.len() {
                for j in (i + 1)..budgets.len() {
                    let larger = &manifests[&budgets[j]];
                    let smaller = &manifests[&budgets[i]];
                    let result = compare_budget_monotonicity(larger, smaller);
                    assert_eq!(
                        result.status,
                        ProjectionInvariantStatus::Pass,
                        "budget monotonicity failed for {}: {} vs {}: {:?}",
                        spec.case_id,
                        budgets[i],
                        budgets[j],
                        result.details
                    );
                }
            }
        }
    }

    #[test]
    fn current_input_retained_at_all_budgets() {
        for (case_id, budget, manifest) in generate_all_manifests() {
            let current_input = manifest
                .invariant_results
                .iter()
                .find(|r| r.code == "current_input_retained")
                .expect("missing current_input_retained invariant");
            assert_eq!(
                current_input.status,
                ProjectionInvariantStatus::Pass,
                "current input not retained for {case_id}-{budget}: {:?}",
                current_input.details
            );
        }
    }

    #[test]
    fn frozen_manifests_match_generated() {
        let dir = manifest_dir();
        if !dir.exists() {
            // Baseline manifests not yet frozen — skip drift check.
            eprintln!("baseline manifests directory not found, skipping drift check");
            return;
        }
        for (case_id, budget, manifest) in generate_all_manifests() {
            let path = dir.join(manifest_filename(&case_id, budget));
            // Once the baseline directory exists, a missing file is real drift
            // (e.g. an accidentally deleted manifest) and must fail the check.
            let committed = fs::read_to_string(&path).unwrap_or_else(|err| {
                panic!("frozen manifest {case_id}-{budget} missing at {path:?}: {err} (regenerate with REGEN_BASELINE=1)")
            });
            let generated = manifest.canonical_json().unwrap();
            let committed_manifest: ProjectionManifest = serde_json::from_str(&committed)
                .unwrap_or_else(|err| {
                    panic!("failed to parse committed manifest {case_id}-{budget}: {err}")
                });
            let committed_canonical = committed_manifest.canonical_json().unwrap();
            assert_eq!(
                generated, committed_canonical,
                "drift detected for {case_id}-{budget}: generated manifest differs from frozen baseline"
            );
        }
    }

    /// Regenerate frozen manifests when REGEN_BASELINE=1 is set.
    /// Normal test runs skip this; it is a developer utility.
    #[test]
    fn regenerate_frozen_manifests() {
        if std::env::var("REGEN_BASELINE").ok().as_deref() != Some("1") {
            return;
        }
        let dir = manifest_dir();
        fs::create_dir_all(&dir).unwrap();
        for (case_id, budget, manifest) in generate_all_manifests() {
            let path = dir.join(manifest_filename(&case_id, budget));
            let json = manifest.canonical_json().unwrap();
            fs::write(&path, json).unwrap();
            println!("wrote {path:?}");
        }
    }

    #[test]
    fn scorecard_is_deterministic() {
        let sc1 = generate_scorecard();
        let sc2 = generate_scorecard();
        let json1 = serde_json::to_string_pretty(&sc1).unwrap();
        let json2 = serde_json::to_string_pretty(&sc2).unwrap();
        assert_eq!(json1, json2, "scorecard must be deterministic");
        assert_eq!(sc1.manifest_count, 36);
    }

    #[test]
    fn phase_1_candidate_changes_only_owner_metadata() {
        let scorecard = generate_phase_1_scorecard();
        assert_eq!(scorecard.manifest_count, 36);
        assert!(scorecard.provider_sections_byte_identical);
        assert!(scorecard
            .entries
            .iter()
            .all(|entry| entry.provider_sections_byte_identical));
        assert!(scorecard
            .entries
            .iter()
            .any(|entry| entry.activation_owner_changed));
    }

    #[test]
    fn selector_comparison_is_deterministic_and_same_activation_scoped() {
        let spec = baseline_case_specs()
            .into_iter()
            .find(|spec| spec.case_id == "work-item-switch-return")
            .expect("selector comparison fixture");
        let recent =
            generate_manifest_for_selector(&spec, BUDGET_MEDIUM, HistorySelector::RecentTurns);
        let scoped =
            generate_manifest_for_selector(&spec, BUDGET_MEDIUM, HistorySelector::WorkItemScoped);
        let prompt = build_baseline_prompt(&spec, BUDGET_MEDIUM);
        let scorecard = prompt.compare_history_selectors().unwrap();

        assert!(scorecard.passed);
        assert_eq!(
            recent.diagnostics.as_ref().unwrap().history_selector,
            HistorySelector::RecentTurns
        );
        assert_eq!(
            scoped.diagnostics.as_ref().unwrap().history_selector,
            HistorySelector::WorkItemScoped
        );
        assert_eq!(recent.activation_owner, scoped.activation_owner);
        assert_eq!(recent.activation_binding, scoped.activation_binding);
        assert_eq!(recent.turn_id, scoped.turn_id);
        assert!(scorecard.assertions.iter().any(|assertion| {
            assertion.code == "selector_comparison_same_activation"
                && assertion.status == ProjectionInvariantStatus::Pass
        }));
        assert_eq!(
            scorecard.diff.baseline_sha256,
            recent.byte_sha256().unwrap()
        );
        assert_eq!(
            scorecard.diff.candidate_sha256,
            scoped.byte_sha256().unwrap()
        );
    }

    #[test]
    fn frozen_phase_1_manifests_match_generated() {
        let dir = phase_1_manifest_dir();
        if !dir.exists() {
            eprintln!("Phase 1 candidate manifest directory not found, skipping drift check");
            return;
        }
        for (case_id, budget, manifest) in generate_all_phase_1_manifests() {
            let path = dir.join(manifest_filename(&case_id, budget));
            let committed = fs::read_to_string(&path).unwrap_or_else(|err| {
                panic!("Phase 1 manifest {case_id}-{budget} missing at {path:?}: {err}")
            });
            let committed_manifest: ProjectionManifest = serde_json::from_str(&committed).unwrap();
            assert_eq!(
                manifest.canonical_json().unwrap(),
                committed_manifest.canonical_json().unwrap(),
                "Phase 1 candidate drift detected for {case_id}-{budget}"
            );
        }
    }

    #[test]
    fn regenerate_phase_1_candidate() {
        if std::env::var("REGEN_PHASE1_CANDIDATE").ok().as_deref() != Some("1") {
            return;
        }
        let dir = phase_1_manifest_dir();
        fs::create_dir_all(&dir).unwrap();
        for (case_id, budget, manifest) in generate_all_phase_1_manifests() {
            let path = dir.join(manifest_filename(&case_id, budget));
            fs::write(&path, manifest.canonical_json().unwrap()).unwrap();
            println!("wrote {path:?}");
        }
        let scorecard = generate_phase_1_scorecard();
        let json = format!("{}\n", serde_json::to_string_pretty(&scorecard).unwrap());
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("benchmarks/projection-eval/candidate-phase-1/scorecard.json");
        fs::write(&path, json).unwrap();
        println!("wrote {path:?}");
    }

    #[test]
    fn regenerate_scorecard() {
        if std::env::var("REGEN_BASELINE").ok().as_deref() != Some("1") {
            return;
        }
        let sc = generate_scorecard();
        let json = format!("{}\n", serde_json::to_string_pretty(&sc).unwrap());
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("benchmarks/projection-eval/baseline/legacy-phase-0-scorecard.json");
        fs::write(&path, json).unwrap();
        println!("wrote {path:?}");
    }
}
