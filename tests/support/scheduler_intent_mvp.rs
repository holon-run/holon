use std::collections::{BTreeMap, BTreeSet};

use holon::domain::scheduler_semantic::{
    structural_semantic_proposal, validate_semantic_proposal, SemanticDecisionInput,
    SemanticProposal, SemanticProposalValidationError,
};
use serde::{Deserialize, Serialize};

pub use holon::domain::scheduler_semantic::{
    SemanticProposal as Proposal, SemanticProposalValidationError as ValidationError,
};

#[derive(Debug, Clone, Deserialize)]
pub struct Corpus {
    pub schema_version: u64,
    pub cases: Vec<SemanticDecisionInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Score {
    pub total: usize,
    pub valid: usize,
    pub invalid: usize,
    pub exact_correct: usize,
    pub unresolved: usize,
    pub target_bindings: usize,
    pub correct_target_bindings: usize,
    pub wrong_target_bindings: usize,
}

impl Score {
    pub fn exact_accuracy(&self) -> f64 {
        ratio(self.exact_correct, self.total)
    }

    pub fn target_binding_precision(&self) -> f64 {
        ratio(self.correct_target_bindings, self.target_bindings)
    }

    pub fn unresolved_rate(&self) -> f64 {
        ratio(self.unresolved, self.total)
    }
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        1.0
    } else {
        numerator as f64 / denominator as f64
    }
}

pub fn validate(
    case: &SemanticDecisionInput,
    proposal: &SemanticProposal,
) -> Result<(), SemanticProposalValidationError> {
    validate_semantic_proposal(case, proposal)
}

pub fn structural_baseline(case: &SemanticDecisionInput) -> SemanticProposal {
    structural_semantic_proposal(case)
}

pub fn score(
    cases: &[SemanticDecisionInput],
    gold: &[SemanticProposal],
    proposals: &[SemanticProposal],
) -> Score {
    let gold_by_id: BTreeMap<&str, &Proposal> = gold
        .iter()
        .map(|proposal| (proposal.input_id(), proposal))
        .collect();
    let proposals_by_id: BTreeMap<&str, &Proposal> = proposals
        .iter()
        .map(|proposal| (proposal.input_id(), proposal))
        .collect();

    let mut score = Score {
        total: cases.len(),
        valid: 0,
        invalid: 0,
        exact_correct: 0,
        unresolved: 0,
        target_bindings: 0,
        correct_target_bindings: 0,
        wrong_target_bindings: 0,
    };
    for case in cases {
        let gold = gold_by_id
            .get(case.id.as_str())
            .unwrap_or_else(|| panic!("missing gold proposal for {}", case.id));
        let proposed = proposals_by_id
            .get(case.id.as_str())
            .unwrap_or_else(|| panic!("missing proposal for {}", case.id));
        let effective = if validate(case, proposed).is_ok() {
            score.valid += 1;
            (*proposed).clone()
        } else {
            score.invalid += 1;
            unresolved(case)
        };

        if effective == **gold {
            score.exact_correct += 1;
        }
        if effective.is_unresolved() {
            score.unresolved += 1;
        }
        if effective.is_target_binding() {
            score.target_bindings += 1;
            if effective == **gold {
                score.correct_target_bindings += 1;
            } else {
                score.wrong_target_bindings += 1;
            }
        }
    }
    score
}

pub fn assert_complete_run(cases: &[SemanticDecisionInput], proposals: &[SemanticProposal]) {
    assert_eq!(
        proposals.len(),
        cases.len(),
        "proposal run must contain exactly one proposal per case"
    );
    let expected: BTreeSet<&str> = cases.iter().map(|case| case.id.as_str()).collect();
    let actual: BTreeSet<&str> = proposals
        .iter()
        .map(|proposal| proposal.input_id())
        .collect();
    assert_eq!(actual, expected, "proposal run case ids");
}

fn unresolved(case: &SemanticDecisionInput) -> SemanticProposal {
    SemanticProposal::Unresolved {
        input_id: case.id.clone(),
        snapshot_revision: case.snapshot_revision,
    }
}
