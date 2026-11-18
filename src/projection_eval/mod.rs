//! Adapter from the runtime prompt to the deterministic projection evaluator.

pub mod baseline;
pub(crate) mod baseline_specs;

use holon_context::projection_eval::{
    legacy_manifest_from_input, manifest_from_input, ProjectionManifestInput,
};

pub use holon_context::projection_eval::{
    compare_budget_monotonicity, compare_history_selectors, compare_projection_manifests,
    HistorySelector, ProjectionBindingSummary, ProjectionDiagnostics, ProjectionEvidenceIndex,
    ProjectionEvidenceRef, ProjectionEvidenceRole, ProjectionInvariantResult,
    ProjectionInvariantStatus, ProjectionManifest, ProjectionManifestDiff, ProjectionOutcome,
    ProjectionOwner, ProjectionRepresentation, ProjectionScorecard, ProjectionSectionManifest,
    HISTORY_SELECTOR_SCHEMA_VERSION, PROJECTION_MANIFEST_SCHEMA_VERSION,
    PROJECTION_SCORECARD_SCHEMA_VERSION,
};

use crate::{prompt::EffectivePrompt, token_estimate::estimate_text_tokens};

fn manifest_input(prompt: &EffectivePrompt) -> ProjectionManifestInput<'_> {
    ProjectionManifestInput {
        system_sections: &prompt.system_sections,
        context_sections: &prompt.context_sections,
        owner: &prompt.projection_owner,
        binding: prompt.projection_binding.as_ref(),
        turn_id: prompt.projection_turn_id.as_deref(),
        evidence: &prompt.projection_evidence,
        plan_evidence: &prompt.context_plan_evidence,
    }
}

pub(crate) fn manifest_from_effective_prompt(prompt: &EffectivePrompt) -> ProjectionManifest {
    legacy_manifest_from_input(manifest_input(prompt), estimate_text_tokens)
}

pub(crate) fn manifest_from_effective_prompt_with_selector(
    prompt: &EffectivePrompt,
    selector: HistorySelector,
) -> ProjectionManifest {
    manifest_from_input(manifest_input(prompt), selector, estimate_text_tokens)
}

/// Evaluate both history selectors from one already-built prompt. This keeps
/// selector comparison request-scoped and guarantees a shared activation
/// snapshot without issuing another provider request.
pub fn compare_prompt_history_selectors(
    prompt: &EffectivePrompt,
) -> serde_json::Result<ProjectionScorecard> {
    let recent_turns =
        manifest_from_effective_prompt_with_selector(prompt, HistorySelector::RecentTurns);
    let work_item_scoped =
        manifest_from_effective_prompt_with_selector(prompt, HistorySelector::WorkItemScoped);
    compare_history_selectors(&recent_turns, &work_item_scoped)
}
