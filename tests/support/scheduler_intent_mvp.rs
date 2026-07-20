use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct Corpus {
    pub schema_version: u64,
    pub cases: Vec<Case>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Case {
    pub id: String,
    pub snapshot_revision: u64,
    pub operator_input: String,
    #[serde(default)]
    pub waits: Vec<WaitCandidate>,
    #[serde(default)]
    pub work_items: Vec<WorkItemCandidate>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WaitCandidate {
    pub wait_id: String,
    pub generation: u64,
    pub state: WaitCandidateState,
    pub owner_work_item_id: String,
    pub summary: String,
    #[serde(default)]
    pub routing_keys: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitCandidateState {
    Active,
    Resolved,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkItemCandidate {
    pub work_item_id: String,
    pub revision: u64,
    pub state: WorkItemCandidateState,
    pub summary: String,
    #[serde(default)]
    pub routing_keys: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemCandidateState {
    Runnable,
    Waiting,
    Terminal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Proposal {
    BindWait {
        case_id: String,
        snapshot_revision: u64,
        wait_id: String,
        generation: u64,
    },
    BindWorkItem {
        case_id: String,
        snapshot_revision: u64,
        work_item_id: String,
        revision: u64,
    },
    NewInteraction {
        case_id: String,
        snapshot_revision: u64,
    },
    Unresolved {
        case_id: String,
        snapshot_revision: u64,
    },
}

impl Proposal {
    pub fn case_id(&self) -> &str {
        match self {
            Self::BindWait { case_id, .. }
            | Self::BindWorkItem { case_id, .. }
            | Self::NewInteraction { case_id, .. }
            | Self::Unresolved { case_id, .. } => case_id,
        }
    }

    pub fn snapshot_revision(&self) -> u64 {
        match self {
            Self::BindWait {
                snapshot_revision, ..
            }
            | Self::BindWorkItem {
                snapshot_revision, ..
            }
            | Self::NewInteraction {
                snapshot_revision, ..
            }
            | Self::Unresolved {
                snapshot_revision, ..
            } => *snapshot_revision,
        }
    }

    pub fn is_target_binding(&self) -> bool {
        matches!(self, Self::BindWait { .. } | Self::BindWorkItem { .. })
    }

    pub fn is_unresolved(&self) -> bool {
        matches!(self, Self::Unresolved { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    CaseIdMismatch,
    StaleSnapshotRevision,
    UnknownWait,
    InactiveWait,
    StaleWaitGeneration,
    UnknownWorkItem,
    TerminalWorkItem,
    StaleWorkItemRevision,
    AmbiguousBinding,
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

pub fn validate(case: &Case, proposal: &Proposal) -> Result<(), ValidationError> {
    if proposal.case_id() != case.id {
        return Err(ValidationError::CaseIdMismatch);
    }
    if proposal.snapshot_revision() != case.snapshot_revision {
        return Err(ValidationError::StaleSnapshotRevision);
    }
    match proposal {
        Proposal::BindWait {
            wait_id,
            generation,
            ..
        } => {
            let wait = case
                .waits
                .iter()
                .find(|candidate| candidate.wait_id == *wait_id)
                .ok_or(ValidationError::UnknownWait)?;
            if wait.state != WaitCandidateState::Active {
                return Err(ValidationError::InactiveWait);
            }
            if wait.generation != *generation {
                return Err(ValidationError::StaleWaitGeneration);
            }
            let active_waits: Vec<_> = case
                .waits
                .iter()
                .filter(|candidate| candidate.state == WaitCandidateState::Active)
                .collect();
            if active_waits.len() > 1
                && !uniquely_matches(
                    &case.operator_input,
                    wait_id,
                    active_waits
                        .iter()
                        .map(|candidate| (candidate.wait_id.as_str(), &candidate.routing_keys)),
                )
            {
                return Err(ValidationError::AmbiguousBinding);
            }
        }
        Proposal::BindWorkItem {
            work_item_id,
            revision,
            ..
        } => {
            let work_item = case
                .work_items
                .iter()
                .find(|candidate| candidate.work_item_id == *work_item_id)
                .ok_or(ValidationError::UnknownWorkItem)?;
            if work_item.state == WorkItemCandidateState::Terminal {
                return Err(ValidationError::TerminalWorkItem);
            }
            if work_item.revision != *revision {
                return Err(ValidationError::StaleWorkItemRevision);
            }
            let eligible_work_items: Vec<_> = case
                .work_items
                .iter()
                .filter(|candidate| candidate.state != WorkItemCandidateState::Terminal)
                .collect();
            if eligible_work_items.len() > 1
                && !uniquely_matches(
                    &case.operator_input,
                    work_item_id,
                    eligible_work_items.iter().map(|candidate| {
                        (candidate.work_item_id.as_str(), &candidate.routing_keys)
                    }),
                )
            {
                return Err(ValidationError::AmbiguousBinding);
            }
        }
        Proposal::NewInteraction { .. } | Proposal::Unresolved { .. } => {}
    }
    Ok(())
}

fn uniquely_matches<'a>(
    operator_input: &str,
    selected_id: &str,
    candidates: impl Iterator<Item = (&'a str, &'a Vec<String>)>,
) -> bool {
    let matched: Vec<_> = candidates
        .filter(|(candidate_id, routing_keys)| {
            operator_input.contains(candidate_id)
                || routing_keys
                    .iter()
                    .any(|routing_key| operator_input.contains(routing_key))
        })
        .map(|(candidate_id, _)| candidate_id)
        .collect();
    matched == [selected_id]
}

pub fn structural_baseline(case: &Case) -> Proposal {
    for wait in &case.waits {
        if case.operator_input.contains(&wait.wait_id) {
            return Proposal::BindWait {
                case_id: case.id.clone(),
                snapshot_revision: case.snapshot_revision,
                wait_id: wait.wait_id.clone(),
                generation: wait.generation,
            };
        }
    }
    for work_item in &case.work_items {
        if case.operator_input.contains(&work_item.work_item_id) {
            return Proposal::BindWorkItem {
                case_id: case.id.clone(),
                snapshot_revision: case.snapshot_revision,
                work_item_id: work_item.work_item_id.clone(),
                revision: work_item.revision,
            };
        }
    }
    unresolved(case)
}

pub fn score(cases: &[Case], gold: &[Proposal], proposals: &[Proposal]) -> Score {
    let gold_by_id: BTreeMap<&str, &Proposal> = gold
        .iter()
        .map(|proposal| (proposal.case_id(), proposal))
        .collect();
    let proposals_by_id: BTreeMap<&str, &Proposal> = proposals
        .iter()
        .map(|proposal| (proposal.case_id(), proposal))
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

pub fn assert_complete_run(cases: &[Case], proposals: &[Proposal]) {
    assert_eq!(
        proposals.len(),
        cases.len(),
        "proposal run must contain exactly one proposal per case"
    );
    let expected: BTreeSet<&str> = cases.iter().map(|case| case.id.as_str()).collect();
    let actual: BTreeSet<&str> = proposals
        .iter()
        .map(|proposal| proposal.case_id())
        .collect();
    assert_eq!(actual, expected, "proposal run case ids");
}

fn unresolved(case: &Case) -> Proposal {
    Proposal::Unresolved {
        case_id: case.id.clone(),
        snapshot_revision: case.snapshot_revision,
    }
}
