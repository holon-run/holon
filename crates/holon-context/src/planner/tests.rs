use crate::{PromptLayout, PromptSection, PromptStability};

use super::*;

fn prompt_section(id: &str, body: &str) -> PromptSection {
    PromptSection {
        name: id.to_string(),
        id: id.to_string(),
        content: body.to_string(),
        stability: PromptStability::TurnScoped,
        layout: PromptLayout::ScopedSuffix,
    }
}

fn policy(
    pinned: bool,
    priority: RetentionPriority,
    drop_tier: DropTier,
    render_order: u16,
) -> ContextCandidatePolicy {
    ContextCandidatePolicy {
        pinned,
        priority,
        drop_tier,
        render_order,
    }
}

#[test]
fn preserves_all_pinned_compact_representations_at_minimum_budget() {
    let candidates = ["current_input", "current_work_item", "continuation_anchor"]
        .into_iter()
        .enumerate()
        .map(|(index, id)| {
            ContextCandidate::new(
                prompt_section(id, &"full ".repeat(80)),
                Some(prompt_section(id, "compact truth")),
                policy(
                    true,
                    RetentionPriority::Critical,
                    DropTier::Last,
                    index as u16,
                ),
            )
        })
        .collect::<Vec<_>>();
    let minimum = candidates
        .iter()
        .map(|candidate| estimate_section_tokens(candidate.compact.as_ref().unwrap()))
        .sum();

    let plan = plan_context(candidates, minimum).unwrap();

    assert_eq!(plan.sections.len(), 3);
    assert!(plan
        .evidence
        .decisions
        .iter()
        .all(|decision| decision.outcome == ContextPlanOutcome::Compact));
    assert_eq!(plan.evidence.allocated_estimated_tokens, minimum);
}

#[test]
fn fails_closed_when_pinned_minimum_exceeds_budget() {
    let candidate = ContextCandidate::new(
        prompt_section("current_input", "full input"),
        Some(prompt_section("current_input", "minimum input")),
        policy(true, RetentionPriority::Critical, DropTier::Last, 0),
    );
    let minimum = estimate_section_tokens(candidate.compact.as_ref().unwrap());

    let error = plan_context(vec![candidate], minimum - 1).unwrap_err();

    assert_eq!(
        error,
        ContextPlanningError::PinnedMinimumOverBudget {
            budget_estimated_tokens: minimum - 1,
            pinned_minimum_estimated_tokens: minimum,
        }
    );
}

#[test]
fn candidate_input_order_does_not_change_plan_or_render_order() {
    let candidates = vec![
        ContextCandidate::new(
            prompt_section("beta", "beta"),
            None,
            policy(false, RetentionPriority::Normal, DropTier::Normal, 20),
        ),
        ContextCandidate::new(
            prompt_section("alpha", "alpha"),
            None,
            policy(false, RetentionPriority::Normal, DropTier::Normal, 10),
        ),
    ];
    let budget = candidates
        .iter()
        .map(|candidate| estimate_section_tokens(&candidate.full))
        .sum();

    let forward = plan_context(candidates.clone(), budget).unwrap();
    let reverse = plan_context(candidates.into_iter().rev().collect(), budget).unwrap();

    assert_eq!(forward.evidence, reverse.evidence);
    assert_eq!(
        forward
            .sections
            .iter()
            .map(|section| section.id.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha", "beta"]
    );
    assert_eq!(
        forward
            .sections
            .iter()
            .map(|section| section.id.as_str())
            .collect::<Vec<_>>(),
        reverse
            .sections
            .iter()
            .map(|section| section.id.as_str())
            .collect::<Vec<_>>()
    );
}

#[test]
fn priority_drop_tier_and_stable_id_drive_selection_not_render_order() {
    let candidates = vec![
        ContextCandidate::new(
            prompt_section("zeta", "selected by stable id"),
            None,
            policy(false, RetentionPriority::High, DropTier::Last, 10),
        ),
        ContextCandidate::new(
            prompt_section("alpha", "selected by drop tier"),
            None,
            policy(false, RetentionPriority::High, DropTier::Late, 30),
        ),
        ContextCandidate::new(
            prompt_section("beta", "omitted"),
            None,
            policy(false, RetentionPriority::High, DropTier::Late, 20),
        ),
    ];
    let budget =
        estimate_section_tokens(&candidates[0].full) + estimate_section_tokens(&candidates[1].full);

    let plan = plan_context(candidates, budget).unwrap();
    let included = plan
        .sections
        .iter()
        .map(|section| section.id.as_str())
        .collect::<Vec<_>>();

    assert_eq!(included, vec!["zeta", "alpha"]);
    assert_eq!(
        plan.evidence
            .decisions
            .iter()
            .find(|decision| decision.candidate_id == "beta")
            .unwrap()
            .outcome,
        ContextPlanOutcome::Omitted
    );
}

#[test]
fn records_full_compact_truncated_and_omitted_outcomes() {
    let full = ContextCandidate::new(
        prompt_section("full", "fits"),
        None,
        policy(false, RetentionPriority::Critical, DropTier::Last, 0),
    );
    let compact = ContextCandidate::new(
        prompt_section("compact", &"large ".repeat(80)),
        Some(prompt_section("compact", "small")),
        policy(false, RetentionPriority::High, DropTier::Last, 1),
    );
    let truncated = ContextCandidate::new(
        prompt_section("truncated", &"truncate ".repeat(80)),
        None,
        policy(false, RetentionPriority::Normal, DropTier::Normal, 2),
    );
    let omitted = ContextCandidate::new(
        prompt_section("omitted", "cannot fit"),
        None,
        policy(false, RetentionPriority::Low, DropTier::Early, 3),
    );
    let budget = estimate_section_tokens(&full.full)
        + estimate_section_tokens(compact.compact.as_ref().unwrap())
        + 24;

    let plan = plan_context(vec![omitted, truncated, compact, full], budget).unwrap();
    let outcomes = plan
        .evidence
        .decisions
        .iter()
        .map(|decision| (decision.candidate_id.as_str(), decision.outcome))
        .collect::<BTreeMap<_, _>>();

    assert_eq!(outcomes["full"], ContextPlanOutcome::Full);
    assert_eq!(outcomes["compact"], ContextPlanOutcome::Compact);
    assert_eq!(outcomes["truncated"], ContextPlanOutcome::Truncated);
    assert_eq!(outcomes["omitted"], ContextPlanOutcome::Omitted);
    assert_eq!(
        plan.evidence
            .decisions
            .iter()
            .find(|decision| decision.candidate_id == "omitted")
            .unwrap()
            .reason,
        ContextPlanReason::OmittedDropTier
    );
}

#[test]
fn reprojection_updates_recent_turn_allocation_evidence() {
    let candidate = ContextCandidate::new(
        prompt_section("recent_turns", &"turn ".repeat(80)),
        None,
        policy(false, RetentionPriority::High, DropTier::Late, 0),
    );
    let full_budget = estimate_section_tokens(&candidate.full);
    let mut plan = plan_context(vec![candidate], full_budget).unwrap();
    let replacement = prompt_section("recent_turns", "one compact projected turn");

    plan.evidence
        .record_reprojection("recent_turns", Some(&replacement));

    let decision = &plan.evidence.decisions[0];
    assert_eq!(decision.outcome, ContextPlanOutcome::Truncated);
    assert_eq!(
        decision.reason,
        ContextPlanReason::TruncatedToRemainingBudget
    );
    assert_eq!(
        decision.allocated_estimated_tokens,
        estimate_section_tokens(&replacement)
    );
    assert_eq!(
        plan.evidence.allocated_estimated_tokens,
        decision.allocated_estimated_tokens
    );
}
