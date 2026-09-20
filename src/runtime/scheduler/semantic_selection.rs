use super::*;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct AutonomousContinuationCandidate {
    pub(crate) work_item_id: String,
    pub(crate) work_item_revision: u64,
    pub(crate) work_item_generation: Option<u64>,
    pub(crate) reactivation_mode: WorkReactivationMode,
}

#[async_trait]
pub(crate) trait AsyncSemanticCandidateSelectionHook: Send + Sync {
    async fn select_autonomous_continuation(
        &self,
        context: &AutonomousContinuationSelectionContext,
    ) -> Result<SemanticCandidateSelectionHookResult, SemanticCandidateSelectionHookError>;
}

impl AutonomousContinuationCandidate {
    fn from_work_item(
        work_item: &WorkItemRecord,
        work_item_generation: Option<u64>,
        reactivation_mode: WorkReactivationMode,
    ) -> Self {
        Self {
            work_item_id: work_item.id.clone(),
            work_item_revision: work_item.revision,
            work_item_generation,
            reactivation_mode,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AutonomousContinuationSnapshotIdentity {
    pub(crate) agent_id: String,
    pub(crate) status: AgentStatus,
    pub(crate) queue_len: usize,
    pub(crate) active_run_id: Option<String>,
    pub(crate) candidates: Vec<AutonomousContinuationCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AutonomousContinuationSelectionContext {
    pub(crate) snapshot_identity: AutonomousContinuationSnapshotIdentity,
    pub(crate) candidates: Vec<AutonomousContinuationCandidate>,
    pub(crate) baseline: AutonomousContinuationCandidate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AutonomousContinuationProposal {
    pub(crate) snapshot_identity: AutonomousContinuationSnapshotIdentity,
    pub(crate) candidate: AutonomousContinuationCandidate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SemanticCandidateSelectionHookResult {
    Propose(AutonomousContinuationProposal),
    #[cfg_attr(not(test), allow(dead_code))]
    Abstain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SemanticCandidateSelectionHookError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AutonomousContinuationFallbackReason {
    HookUnavailable,
    HookError,
    Abstain,
    InvalidProposal,
    StaleSnapshot,
}

impl AutonomousContinuationFallbackReason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::HookUnavailable => "hook_unavailable",
            Self::HookError => "hook_error",
            Self::Abstain => "abstain",
            Self::InvalidProposal => "invalid_proposal",
            Self::StaleSnapshot => "stale_snapshot",
        }
    }
}

pub(crate) trait SemanticCandidateSelectionHook: Send + Sync {
    fn select_autonomous_continuation(
        &self,
        context: &AutonomousContinuationSelectionContext,
    ) -> Result<SemanticCandidateSelectionHookResult, SemanticCandidateSelectionHookError>;
}

pub(crate) struct StaticSemanticCandidateSelectionHook;

impl SemanticCandidateSelectionHook for StaticSemanticCandidateSelectionHook {
    fn select_autonomous_continuation(
        &self,
        context: &AutonomousContinuationSelectionContext,
    ) -> Result<SemanticCandidateSelectionHookResult, SemanticCandidateSelectionHookError> {
        Ok(SemanticCandidateSelectionHookResult::Propose(
            AutonomousContinuationProposal {
                snapshot_identity: context.snapshot_identity.clone(),
                candidate: context.baseline.clone(),
            },
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AutonomousContinuationSelection {
    snapshot_identity: AutonomousContinuationSnapshotIdentity,
    candidate: AutonomousContinuationCandidate,
    fallback_reason: Option<AutonomousContinuationFallbackReason>,
}

impl AutonomousContinuationSelection {
    pub(crate) fn fallback_reason(&self) -> Option<AutonomousContinuationFallbackReason> {
        self.fallback_reason
    }

    pub(crate) fn candidate_count(&self) -> usize {
        self.snapshot_identity.candidates.len()
    }
}

fn legal_autonomous_continuation_candidates(
    projection: &SchedulerProjection,
) -> Vec<AutonomousContinuationCandidate> {
    let current = projection
        .current_work_item
        .as_ref()
        .filter(|_| {
            projection.current_work_item_scheduling_state == Some(WorkItemSchedulingState::Runnable)
        })
        .filter(|work_item| projection.execution_authorizes_autonomous(work_item))
        .map(|work_item| {
            AutonomousContinuationCandidate::from_work_item(
                work_item,
                projection.autonomous_execution_generation(work_item),
                WorkReactivationMode::ContinueActive,
            )
        });
    current
        .into_iter()
        .chain(
            projection
                .queued_runnable_work_items
                .iter()
                .filter(|work_item| projection.execution_authorizes_autonomous(work_item))
                .map(|work_item| {
                    AutonomousContinuationCandidate::from_work_item(
                        work_item,
                        projection.autonomous_execution_generation(work_item),
                        WorkReactivationMode::ActivateQueued,
                    )
                }),
        )
        .collect()
}

fn autonomous_continuation_context(
    projection: &SchedulerProjection,
) -> Option<AutonomousContinuationSelectionContext> {
    let candidates = legal_autonomous_continuation_candidates(projection);
    let baseline = candidates.first()?.clone();
    Some(AutonomousContinuationSelectionContext {
        snapshot_identity: AutonomousContinuationSnapshotIdentity {
            agent_id: projection.agent_id.clone(),
            status: projection.status.clone(),
            queue_len: projection.queue_len,
            active_run_id: projection.active_run_id.clone(),
            candidates: candidates.clone(),
        },
        candidates,
        baseline,
    })
}

pub(crate) fn select_autonomous_continuation(
    projection: &SchedulerProjection,
) -> Option<AutonomousContinuationSelection> {
    select_autonomous_continuation_with_hook(
        projection,
        Some(&StaticSemanticCandidateSelectionHook),
    )
}

pub(crate) fn select_autonomous_continuation_with_hook(
    projection: &SchedulerProjection,
    hook: Option<&dyn SemanticCandidateSelectionHook>,
) -> Option<AutonomousContinuationSelection> {
    let context = autonomous_continuation_context(projection)?;
    let (candidate, fallback_reason) = if context.candidates.len() == 1 {
        (context.baseline.clone(), None)
    } else {
        match hook {
            None => (
                context.baseline.clone(),
                Some(AutonomousContinuationFallbackReason::HookUnavailable),
            ),
            Some(hook) => match hook.select_autonomous_continuation(&context) {
                Ok(SemanticCandidateSelectionHookResult::Propose(proposal)) => {
                    if proposal.snapshot_identity != context.snapshot_identity {
                        (
                            context.baseline.clone(),
                            Some(AutonomousContinuationFallbackReason::StaleSnapshot),
                        )
                    } else if !context.candidates.contains(&proposal.candidate) {
                        (
                            context.baseline.clone(),
                            Some(AutonomousContinuationFallbackReason::InvalidProposal),
                        )
                    } else {
                        (proposal.candidate, None)
                    }
                }
                Ok(SemanticCandidateSelectionHookResult::Abstain) => (
                    context.baseline.clone(),
                    Some(AutonomousContinuationFallbackReason::Abstain),
                ),
                Err(_) => (
                    context.baseline.clone(),
                    Some(AutonomousContinuationFallbackReason::HookError),
                ),
            },
        }
    };
    Some(AutonomousContinuationSelection {
        snapshot_identity: context.snapshot_identity,
        candidate,
        fallback_reason,
    })
}

pub(crate) async fn select_autonomous_continuation_with_async_hook(
    projection: &SchedulerProjection,
    hook: Option<&dyn AsyncSemanticCandidateSelectionHook>,
) -> Option<AutonomousContinuationSelection> {
    let context = autonomous_continuation_context(projection)?;
    let (candidate, fallback_reason) = if context.candidates.len() == 1 {
        (context.baseline.clone(), None)
    } else {
        match hook {
            None => (
                context.baseline.clone(),
                Some(AutonomousContinuationFallbackReason::HookUnavailable),
            ),
            Some(hook) => match hook.select_autonomous_continuation(&context).await {
                Ok(SemanticCandidateSelectionHookResult::Propose(proposal)) => {
                    if proposal.snapshot_identity != context.snapshot_identity {
                        (
                            context.baseline.clone(),
                            Some(AutonomousContinuationFallbackReason::StaleSnapshot),
                        )
                    } else if !context.candidates.contains(&proposal.candidate) {
                        (
                            context.baseline.clone(),
                            Some(AutonomousContinuationFallbackReason::InvalidProposal),
                        )
                    } else {
                        (proposal.candidate, None)
                    }
                }
                Ok(SemanticCandidateSelectionHookResult::Abstain) => (
                    context.baseline.clone(),
                    Some(AutonomousContinuationFallbackReason::Abstain),
                ),
                Err(_) => (
                    context.baseline.clone(),
                    Some(AutonomousContinuationFallbackReason::HookError),
                ),
            },
        }
    };
    Some(AutonomousContinuationSelection {
        snapshot_identity: context.snapshot_identity,
        candidate,
        fallback_reason,
    })
}

pub(crate) fn resolve_autonomous_continuation_work_item<'a>(
    projection: &'a SchedulerProjection,
    selection: &AutonomousContinuationSelection,
) -> Option<(&'a WorkItemRecord, WorkReactivationMode, Option<u64>)> {
    let current_snapshot = autonomous_continuation_context(projection)?.snapshot_identity;
    if current_snapshot != selection.snapshot_identity {
        return None;
    }
    match selection.candidate.reactivation_mode {
        WorkReactivationMode::ContinueActive => projection
            .current_work_item
            .as_ref()
            .filter(|work_item| {
                work_item.id == selection.candidate.work_item_id
                    && work_item.revision == selection.candidate.work_item_revision
            })
            .map(|work_item| {
                (
                    work_item,
                    WorkReactivationMode::ContinueActive,
                    selection.candidate.work_item_generation,
                )
            }),
        WorkReactivationMode::ActivateQueued => projection
            .queued_runnable_work_items
            .iter()
            .find(|work_item| {
                work_item.id == selection.candidate.work_item_id
                    && work_item.revision == selection.candidate.work_item_revision
            })
            .map(|work_item| {
                (
                    work_item,
                    WorkReactivationMode::ActivateQueued,
                    selection.candidate.work_item_generation,
                )
            }),
    }
}
