use super::*;
use crate::{
    runtime::scheduler::SchedulerWorkSelection,
    types::{AgentPostureProjection, AgentSchedulingPosture, WorkItemSchedulingState},
    work_item_scheduling::{QueueWorkRank, WorkItemCandidateClass},
};

impl CapturedWork {
    pub(crate) fn scheduler_work_selection(
        &self,
        state: &AgentState,
        ranked: &[QueueWorkRank<'_>],
    ) -> Result<SchedulerWorkSelection> {
        let current = ranked.iter().find(|rank| is_current(state, rank));
        let waiting = ranked.iter().find(|rank| {
            (is_current(state, rank) || rank.has_active_waits)
                && matches!(
                    rank.scheduling_state,
                    WorkItemSchedulingState::WaitingOperator
                        | WorkItemSchedulingState::WaitingTask
                        | WorkItemSchedulingState::WaitingExternal
                        | WorkItemSchedulingState::WaitingTimer
                        | WorkItemSchedulingState::WaitingSystem
                )
        });
        let runnable = ranked
            .iter()
            .filter(|rank| rank.candidate_class == WorkItemCandidateClass::QueuedRunnable)
            .take(5);
        let mut queued_runnable = super::context_queue::context_slots(runnable.clone().count())?;
        for rank in runnable {
            queued_runnable.push(clone_queue_work_record(rank.work_item)?);
        }
        Ok(SchedulerWorkSelection {
            current: current
                .map(|rank| clone_queue_work_record(rank.work_item))
                .transpose()?,
            current_scheduling_state: current.map(|rank| rank.scheduling_state),
            queued_runnable,
            waiting: waiting
                .map(|rank| clone_queue_work_record(rank.work_item))
                .transpose()?,
            waiting_scheduling_state: waiting.map(|rank| rank.scheduling_state),
        })
    }

    pub(crate) fn scheduler_work_posture(
        &self,
        state: &AgentState,
        has_queued_input: bool,
        ranked: &[QueueWorkRank<'_>],
        active_waits: &[WaitConditionRecord],
    ) -> Result<AgentPostureProjection> {
        let first_class = |class| ranked.iter().find(|rank| rank.candidate_class == class);
        let current_state = |scheduling_state| {
            ranked
                .iter()
                .find(|rank| is_current(state, rank) && rank.scheduling_state == scheduling_state)
        };
        let current_window = |class| {
            ranked
                .iter()
                .filter(|rank| {
                    rank.candidate_class == class
                        && (class != WorkItemCandidateClass::Blocked
                            || rank.scheduling_state != WorkItemSchedulingState::WaitingTask)
                })
                .take(3)
                .find(|rank| is_current(state, rank))
        };
        // Preserve the exact display-window posture policy while borrowing all
        // canonical rows. In particular, Task waits are not a blocked window.
        let selected = first_class(WorkItemCandidateClass::CurrentRunnable)
            .or_else(|| first_class(WorkItemCandidateClass::QueuedRunnable))
            .map(|rank| (AgentSchedulingPosture::HasRunnableWork, rank))
            .or_else(|| {
                current_state(WorkItemSchedulingState::WaitingTask)
                    .map(|rank| (AgentSchedulingPosture::WaitingForTask, rank))
            })
            .or_else(|| {
                current_state(WorkItemSchedulingState::WaitingExternal)
                    .map(|rank| (AgentSchedulingPosture::WaitingForExternal, rank))
            })
            .or_else(|| {
                current_window(WorkItemCandidateClass::WaitingForOperator)
                    .map(|rank| (AgentSchedulingPosture::WaitingForOperator, rank))
            })
            .or_else(|| {
                current_window(WorkItemCandidateClass::Blocked)
                    .or_else(|| current_window(WorkItemCandidateClass::TriggeredBlocked))
                    .map(|rank| (AgentSchedulingPosture::Blocked, rank))
            });
        crate::storage::derive_agent_posture_from_selected_work(
            state,
            has_queued_input,
            selected.map(|(posture, rank)| {
                (
                    posture,
                    rank.work_item,
                    rank.scheduling_state,
                    is_current(state, rank),
                )
            }),
            active_waits,
        )
    }

    /// Exact claim evidence is independent of the scheduler's candidate window.
    pub(crate) fn scheduler_work_projection(
        &self,
        state: &AgentState,
        triggers: &[ExternalTriggerRecord],
        id: &str,
    ) -> Result<Option<crate::work_item_scheduling::WorkItemSchedulingProjection>> {
        self.queue_work_ranks(state, triggers)?
            .iter()
            .find(|rank| rank.work_item.id == id)
            .map(|rank| self.materialize_queue_work(state, triggers, rank.work_item))
            .transpose()
    }

    pub(crate) fn scheduler_blocked_work<'a>(
        &'a self,
        ranked: &'a [QueueWorkRank<'a>],
    ) -> impl Iterator<Item = &'a WorkItemRecord> {
        ranked.iter().filter_map(|rank| {
            let work = rank.work_item;
            (rank.scheduling_state == WorkItemSchedulingState::Blocked
                && work.agent_id == self.scope.agent_id
                && work.blocked_by.is_some()
                && work.recheck_at.is_none()
                && !self
                    .queue_active_waits()
                    .any(|wait| wait.work_item_id.as_deref() == Some(work.id.as_str())))
            .then_some(work)
        })
    }
}

fn is_current(state: &AgentState, rank: &QueueWorkRank<'_>) -> bool {
    rank.work_item.state == WorkItemState::Open
        && state.current_work_item_id.as_deref() == Some(rank.work_item.id.as_str())
}
