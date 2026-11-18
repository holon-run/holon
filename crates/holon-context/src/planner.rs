//! Deterministic context candidate selection and budget planning.

use std::collections::{BTreeMap, BTreeSet};

use crate::PromptSection;

use super::budget::{estimate_section_tokens, fit_section_to_budget};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RetentionPriority {
    Low,
    Normal,
    High,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DropTier {
    Last,
    Late,
    Normal,
    Early,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextCandidatePolicy {
    pub pinned: bool,
    pub priority: RetentionPriority,
    pub drop_tier: DropTier,
    pub render_order: u16,
}

#[derive(Debug, Clone)]
pub struct ContextCandidate {
    pub full: PromptSection,
    pub compact: Option<PromptSection>,
    pub policy: ContextCandidatePolicy,
}

impl ContextCandidate {
    pub fn new(
        full: PromptSection,
        compact: Option<PromptSection>,
        policy: ContextCandidatePolicy,
    ) -> Self {
        Self {
            full,
            compact,
            policy,
        }
    }

    fn id(&self) -> &str {
        &self.full.id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextPlanOutcome {
    Full,
    Compact,
    Truncated,
    Omitted,
}

impl ContextPlanOutcome {
    pub fn label(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Compact => "compact",
            Self::Truncated => "truncated",
            Self::Omitted => "omitted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextPlanReason {
    SelectedFull,
    SelectedCompactForPinnedMinimum,
    SelectedCompactForBudget,
    TruncatedToRemainingBudget,
    OmittedLowerPriority,
    OmittedDropTier,
}

impl ContextPlanReason {
    pub fn code(self) -> &'static str {
        match self {
            Self::SelectedFull => "selected_full",
            Self::SelectedCompactForPinnedMinimum => "selected_compact_for_pinned_minimum",
            Self::SelectedCompactForBudget => "selected_compact_for_budget",
            Self::TruncatedToRemainingBudget => "truncated_to_remaining_budget",
            Self::OmittedLowerPriority => "omitted_lower_priority",
            Self::OmittedDropTier => "omitted_drop_tier",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextPlanDecision {
    pub candidate_id: String,
    pub section_name: String,
    pub requested_estimated_tokens: usize,
    pub minimum_estimated_tokens: usize,
    pub allocated_estimated_tokens: usize,
    pub outcome: ContextPlanOutcome,
    pub reason: ContextPlanReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContextPlanEvidence {
    pub total_budget_estimated_tokens: usize,
    pub allocated_estimated_tokens: usize,
    pub decisions: Vec<ContextPlanDecision>,
}

impl ContextPlanEvidence {
    pub fn record_reprojection(&mut self, candidate_id: &str, replacement: Option<&PromptSection>) {
        let Some(decision) = self
            .decisions
            .iter_mut()
            .find(|decision| decision.candidate_id == candidate_id)
        else {
            return;
        };
        match replacement {
            Some(section) => {
                decision.allocated_estimated_tokens = estimate_section_tokens(section);
                decision.outcome = ContextPlanOutcome::Truncated;
                decision.reason = ContextPlanReason::TruncatedToRemainingBudget;
            }
            None => {
                decision.allocated_estimated_tokens = 0;
                decision.outcome = ContextPlanOutcome::Omitted;
                decision.reason = ContextPlanReason::OmittedLowerPriority;
            }
        }
        self.allocated_estimated_tokens = self
            .decisions
            .iter()
            .map(|decision| decision.allocated_estimated_tokens)
            .sum();
    }
}

#[derive(Debug, Clone)]
pub struct ContextPlan {
    pub sections: Vec<PromptSection>,
    pub evidence: ContextPlanEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContextPlanningError {
    #[error("context candidate id must not be empty")]
    EmptyCandidateId,
    #[error("duplicate context candidate id: {0}")]
    DuplicateCandidateId(String),
    #[error("context candidate {candidate_id} compact representation id does not match")]
    CompactCandidateIdMismatch { candidate_id: String },
    #[error("pinned context candidate {candidate_id} has no compact representation")]
    MissingPinnedCompact { candidate_id: String },
    #[error(
        "pinned_minimum_over_budget: budget={budget_estimated_tokens}, pinned_minimum={pinned_minimum_estimated_tokens}"
    )]
    PinnedMinimumOverBudget {
        budget_estimated_tokens: usize,
        pinned_minimum_estimated_tokens: usize,
    },
}

#[derive(Debug, Clone)]
struct Selection {
    section: Option<PromptSection>,
    decision: ContextPlanDecision,
    render_order: u16,
}

pub fn plan_context(
    mut candidates: Vec<ContextCandidate>,
    total_budget: usize,
) -> Result<ContextPlan, ContextPlanningError> {
    validate_candidates(&candidates)?;
    candidates.sort_by(selection_order);

    let pinned_minimum = candidates
        .iter()
        .filter(|candidate| candidate.policy.pinned)
        .map(|candidate| {
            estimate_section_tokens(
                candidate
                    .compact
                    .as_ref()
                    .expect("validated pinned compact representation"),
            )
        })
        .sum::<usize>();
    if pinned_minimum > total_budget {
        return Err(ContextPlanningError::PinnedMinimumOverBudget {
            budget_estimated_tokens: total_budget,
            pinned_minimum_estimated_tokens: pinned_minimum,
        });
    }

    let mut remaining_budget = total_budget.saturating_sub(pinned_minimum);
    let mut selections = BTreeMap::new();

    for candidate in &candidates {
        let requested = estimate_section_tokens(&candidate.full);
        let minimum = candidate
            .compact
            .as_ref()
            .map(estimate_section_tokens)
            .unwrap_or(0);
        if candidate.policy.pinned {
            let compact = candidate
                .compact
                .clone()
                .expect("validated pinned compact representation");
            selections.insert(
                candidate.id().to_string(),
                Selection {
                    section: Some(compact),
                    decision: ContextPlanDecision {
                        candidate_id: candidate.id().to_string(),
                        section_name: candidate.full.name.clone(),
                        requested_estimated_tokens: requested,
                        minimum_estimated_tokens: minimum,
                        allocated_estimated_tokens: minimum,
                        outcome: ContextPlanOutcome::Compact,
                        reason: ContextPlanReason::SelectedCompactForPinnedMinimum,
                    },
                    render_order: candidate.policy.render_order,
                },
            );
        }
    }

    for candidate in candidates {
        let requested = estimate_section_tokens(&candidate.full);
        let minimum = candidate
            .compact
            .as_ref()
            .map(estimate_section_tokens)
            .unwrap_or(0);

        if candidate.policy.pinned {
            let additional = requested.saturating_sub(minimum);
            if additional <= remaining_budget {
                remaining_budget = remaining_budget.saturating_sub(additional);
                let selection = selections
                    .get_mut(candidate.id())
                    .expect("pinned selection initialized");
                selection.section = Some(candidate.full);
                selection.decision.allocated_estimated_tokens = requested;
                selection.decision.outcome = ContextPlanOutcome::Full;
                selection.decision.reason = ContextPlanReason::SelectedFull;
            }
            continue;
        }

        let (section, allocated, outcome, reason) = if requested <= remaining_budget {
            (
                Some(candidate.full.clone()),
                requested,
                ContextPlanOutcome::Full,
                ContextPlanReason::SelectedFull,
            )
        } else if let Some(compact) = candidate
            .compact
            .clone()
            .filter(|compact| estimate_section_tokens(compact) <= remaining_budget)
        {
            let allocated = estimate_section_tokens(&compact);
            (
                Some(compact),
                allocated,
                ContextPlanOutcome::Compact,
                ContextPlanReason::SelectedCompactForBudget,
            )
        } else {
            let truncated = candidate
                .compact
                .is_none()
                .then(|| fit_section_to_budget(candidate.full.clone(), remaining_budget))
                .flatten();
            if let Some(truncated) = truncated {
                let allocated = estimate_section_tokens(&truncated);
                (
                    Some(truncated),
                    allocated,
                    ContextPlanOutcome::Truncated,
                    ContextPlanReason::TruncatedToRemainingBudget,
                )
            } else {
                let reason = if candidate.policy.drop_tier == DropTier::Early {
                    ContextPlanReason::OmittedDropTier
                } else {
                    ContextPlanReason::OmittedLowerPriority
                };
                (None, 0, ContextPlanOutcome::Omitted, reason)
            }
        };
        remaining_budget = remaining_budget.saturating_sub(allocated);
        selections.insert(
            candidate.id().to_string(),
            Selection {
                section,
                decision: ContextPlanDecision {
                    candidate_id: candidate.id().to_string(),
                    section_name: candidate.full.name,
                    requested_estimated_tokens: requested,
                    minimum_estimated_tokens: minimum,
                    allocated_estimated_tokens: allocated,
                    outcome,
                    reason,
                },
                render_order: candidate.policy.render_order,
            },
        );
    }

    let mut selections = selections.into_values().collect::<Vec<_>>();
    let mut decisions = selections
        .iter()
        .map(|selection| selection.decision.clone())
        .collect::<Vec<_>>();
    decisions.sort_by(|left, right| left.candidate_id.cmp(&right.candidate_id));
    selections.sort_by(|left, right| {
        left.render_order
            .cmp(&right.render_order)
            .then_with(|| left.decision.candidate_id.cmp(&right.decision.candidate_id))
    });
    let sections = selections
        .into_iter()
        .filter_map(|selection| selection.section)
        .collect::<Vec<_>>();
    let allocated_estimated_tokens = decisions
        .iter()
        .map(|decision| decision.allocated_estimated_tokens)
        .sum();

    Ok(ContextPlan {
        sections,
        evidence: ContextPlanEvidence {
            total_budget_estimated_tokens: total_budget,
            allocated_estimated_tokens,
            decisions,
        },
    })
}

fn validate_candidates(candidates: &[ContextCandidate]) -> Result<(), ContextPlanningError> {
    let mut ids = BTreeSet::new();
    for candidate in candidates {
        let id = candidate.id();
        if id.trim().is_empty() {
            return Err(ContextPlanningError::EmptyCandidateId);
        }
        if !ids.insert(id) {
            return Err(ContextPlanningError::DuplicateCandidateId(id.to_string()));
        }
        if let Some(compact) = candidate.compact.as_ref() {
            if compact.id != id {
                return Err(ContextPlanningError::CompactCandidateIdMismatch {
                    candidate_id: id.to_string(),
                });
            }
        } else if candidate.policy.pinned {
            return Err(ContextPlanningError::MissingPinnedCompact {
                candidate_id: id.to_string(),
            });
        }
    }
    Ok(())
}

fn selection_order(left: &ContextCandidate, right: &ContextCandidate) -> std::cmp::Ordering {
    right
        .policy
        .pinned
        .cmp(&left.policy.pinned)
        .then_with(|| right.policy.priority.cmp(&left.policy.priority))
        .then_with(|| left.policy.drop_tier.cmp(&right.policy.drop_tier))
        .then_with(|| left.id().cmp(right.id()))
}

#[cfg(test)]
mod tests;
