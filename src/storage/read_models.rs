//! Runtime read-model and recovery projections over canonical persistence.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::Result;

use crate::{
    runtime_db::RuntimeDb,
    types::{
        AgentPostureProjection, AgentSchedulingPosture, AgentState, AgentStatus,
        ExternalTriggerStatus, MessageEnvelope, TimerRecord, TimerStatus, WaitConditionKind,
        WaitConditionRecord, WaitConditionStatus, WorkItemDelegationRecord, WorkItemRecord,
        WorkItemSchedulingState, WorkItemState,
    },
};

use super::{
    recovery::RecoverySnapshot,
    work_queue::{
        compare_queue_display_order, compare_scheduling_projection_order, WorkItemCandidateClass,
        WorkQueueReadModel,
    },
};

#[derive(Debug, Clone)]
pub struct RuntimeReadModels {
    runtime_db: RuntimeDb,
    agent_id: Option<String>,
}

impl RuntimeReadModels {
    pub(crate) fn new(runtime_db: RuntimeDb, agent_id: Option<String>) -> Self {
        Self {
            runtime_db,
            agent_id,
        }
    }

    pub fn work_queue(&self) -> Result<WorkQueueReadModel> {
        let current_work_item_id = self
            .read_agent()?
            .and_then(|agent| agent.current_work_item_id);
        let mut latest = HashMap::<String, WorkItemRecord>::new();
        if let Some(agent_id) = self.agent_id.as_deref() {
            for record in self
                .runtime_db
                .work_items()
                .latest_for_agent(agent_id, usize::MAX)?
            {
                latest.insert(record.id.clone(), record);
            }
        }

        let current = current_work_item_id
            .as_deref()
            .and_then(|id| latest.get(id))
            .filter(|item| item.state == WorkItemState::Open)
            .cloned();

        let trigger_delivery_by_id = self
            .latest_external_triggers()?
            .into_iter()
            .filter(|trigger| trigger.status == ExternalTriggerStatus::Active)
            .filter_map(|trigger| {
                trigger
                    .last_delivered_at
                    .map(|delivered_at| (trigger.external_trigger_id, delivered_at))
            })
            .collect::<BTreeMap<_, _>>();
        let active_wait_conditions = self
            .active_wait_conditions()?
            .into_iter()
            .filter_map(|condition| condition.work_item_id.clone().map(|id| (id, condition)))
            .fold(
                BTreeMap::<String, Vec<WaitConditionRecord>>::new(),
                |mut acc, (id, condition)| {
                    acc.entry(id).or_default().push(condition);
                    acc
                },
            );
        let active_continuation_suspended_ids = self
            .runtime_db
            .work_item_continuations()
            .active_for_agent(self.agent_id.as_deref().unwrap_or_default())?
            .into_iter()
            .map(|frame| frame.suspended_work_item_id)
            .collect::<BTreeSet<_>>();
        let mut items = latest
            .values()
            .cloned()
            .map(|item| {
                let is_current = current_work_item_id.as_deref() == Some(item.id.as_str())
                    && item.state == WorkItemState::Open;
                let yielded = item.state == WorkItemState::Open
                    && active_continuation_suspended_ids.contains(&item.id);
                crate::work_item_scheduling::derive_work_item_scheduling(
                    crate::work_item_scheduling::WorkItemSchedulingFacts {
                        work_item: &item,
                        is_current,
                        is_yielded: yielded,
                        active_wait_conditions: active_wait_conditions
                            .get(&item.id)
                            .map(Vec::as_slice)
                            .unwrap_or(&[]),
                        trigger_delivery_by_id: &trigger_delivery_by_id,
                    },
                )
            })
            .collect::<Vec<_>>();

        items.sort_by(compare_scheduling_projection_order);
        let current_runnable = items
            .iter()
            .find(|item| item.candidate_class == WorkItemCandidateClass::CurrentRunnable)
            .cloned();
        let triggered_blocked = items
            .iter()
            .filter(|item| item.candidate_class == WorkItemCandidateClass::TriggeredBlocked)
            .take(3)
            .cloned()
            .collect::<Vec<_>>();
        let queued_runnable = items
            .iter()
            .filter(|item| item.candidate_class == WorkItemCandidateClass::QueuedRunnable)
            .take(5)
            .cloned()
            .collect::<Vec<_>>();
        let yielded = items
            .iter()
            .filter(|item| item.candidate_class == WorkItemCandidateClass::Yielded)
            .take(5)
            .cloned()
            .collect::<Vec<_>>();
        let waiting_for_operator = items
            .iter()
            .filter(|item| item.candidate_class == WorkItemCandidateClass::WaitingForOperator)
            .take(3)
            .cloned()
            .collect::<Vec<_>>();
        let blocked = items
            .iter()
            .filter(|item| item.candidate_class == WorkItemCandidateClass::Blocked)
            .filter(|item| item.scheduling_state != WorkItemSchedulingState::WaitingTask)
            .take(3)
            .cloned()
            .collect::<Vec<_>>();
        let completed_recent = items
            .iter()
            .filter(|item| item.candidate_class == WorkItemCandidateClass::CompletedRecent)
            .take(3)
            .cloned()
            .collect::<Vec<_>>();

        let mut queued_blocked = latest
            .values()
            .filter(|item| {
                item.state == WorkItemState::Open
                    && Some(item.id.as_str()) != current_work_item_id.as_deref()
                    && !active_continuation_suspended_ids.contains(&item.id)
                    && !active_wait_conditions
                        .get(&item.id)
                        .is_some_and(|conditions| {
                            conditions
                                .iter()
                                .any(|condition| condition.kind == WaitConditionKind::Task)
                        })
            })
            .cloned()
            .collect::<Vec<_>>();
        queued_blocked.sort_by(compare_queue_display_order);

        Ok(WorkQueueReadModel {
            current,
            queued_blocked,
            items,
            current_runnable,
            triggered_blocked,
            queued_runnable,
            yielded,
            waiting_for_operator,
            blocked,
            completed_recent,
        })
    }

    pub fn agent_posture(&self, agent: &AgentState) -> Result<AgentPostureProjection> {
        if matches!(agent.status, AgentStatus::Stopped) {
            return Ok(AgentPostureProjection {
                posture: AgentSchedulingPosture::Archived,
                reason: "agent lifecycle is stopped".into(),
                work_item_id: None,
                task_id: None,
                run_id: None,
            });
        }

        if let Some(run_id) = agent.current_run_id.clone() {
            return Ok(AgentPostureProjection {
                posture: AgentSchedulingPosture::ActiveTurn,
                reason: "agent has an active turn".into(),
                work_item_id: agent.current_turn_work_item_id.clone(),
                task_id: None,
                run_id: Some(run_id),
            });
        }

        if self
            .runtime_db
            .queue_entries()
            .has_queued_for_agent(&agent.id)?
        {
            return Ok(AgentPostureProjection {
                posture: AgentSchedulingPosture::HasQueuedInput,
                reason: "agent has queued input".into(),
                work_item_id: None,
                task_id: None,
                run_id: None,
            });
        }

        let work_queue = self.work_queue()?;
        if let Some(item) = work_queue
            .current_runnable
            .as_ref()
            .or_else(|| work_queue.queued_runnable.first())
        {
            return Ok(AgentPostureProjection {
                posture: AgentSchedulingPosture::HasRunnableWork,
                reason: item.posture_reason(),
                work_item_id: Some(item.work_item.id.clone()),
                task_id: None,
                run_id: None,
            });
        }

        if let Some(item) = work_queue
            .items
            .iter()
            .find(|item| item.scheduling_state == WorkItemSchedulingState::WaitingTask)
        {
            let task_id = self
                .active_wait_conditions()?
                .into_iter()
                .find(|condition| {
                    condition.status == WaitConditionStatus::Active
                        && condition.kind == WaitConditionKind::Task
                        && condition.work_item_id.as_deref() == Some(item.work_item.id.as_str())
                })
                .and_then(|condition| condition.subject_ref);
            return Ok(AgentPostureProjection {
                posture: AgentSchedulingPosture::WaitingForTask,
                reason: item.posture_reason(),
                work_item_id: Some(item.work_item.id.clone()),
                task_id,
                run_id: None,
            });
        }

        if let Some(item) = work_queue
            .items
            .iter()
            .find(|item| item.scheduling_state == WorkItemSchedulingState::WaitingExternal)
        {
            return Ok(AgentPostureProjection {
                posture: AgentSchedulingPosture::WaitingForExternal,
                reason: item.posture_reason(),
                work_item_id: Some(item.work_item.id.clone()),
                task_id: None,
                run_id: None,
            });
        }

        if let Some(item) = work_queue.waiting_for_operator.first() {
            return Ok(AgentPostureProjection {
                posture: AgentSchedulingPosture::WaitingForOperator,
                reason: item.posture_reason(),
                work_item_id: Some(item.work_item.id.clone()),
                task_id: None,
                run_id: None,
            });
        }

        if let Some(item) = work_queue
            .blocked
            .first()
            .or_else(|| work_queue.triggered_blocked.first())
        {
            return Ok(AgentPostureProjection {
                posture: AgentSchedulingPosture::Blocked,
                reason: item.posture_reason(),
                work_item_id: Some(item.work_item.id.clone()),
                task_id: None,
                run_id: None,
            });
        }

        Ok(AgentPostureProjection {
            posture: AgentSchedulingPosture::Idle,
            reason: "no queued input, active turn, runnable work, or active waits".into(),
            work_item_id: None,
            task_id: None,
            run_id: None,
        })
    }

    pub fn waiting_contract_anchor(&self) -> Result<Option<WorkItemRecord>> {
        let projection = self.work_queue()?;
        Ok(projection.current.or_else(|| {
            projection
                .queued_blocked
                .into_iter()
                .filter(|item| item.blocked_by.is_some())
                .max_by(|left, right| {
                    left.updated_at
                        .cmp(&right.updated_at)
                        .then_with(|| left.created_at.cmp(&right.created_at))
                        .then_with(|| left.id.cmp(&right.id))
                })
        }))
    }

    pub fn recovery_snapshot(&self, agent_id: &str) -> Result<RecoverySnapshot> {
        let agent = self.read_agent()?;
        let mut messages_by_id = BTreeMap::<String, MessageEnvelope>::new();
        for message in self.runtime_db.messages().all(self.agent_id.as_deref())? {
            messages_by_id.insert(message.id.clone(), message);
        }

        let queued_entries = self.runtime_db.queue_entries().queued_for_agent(agent_id)?;
        let mut replay_messages = queued_entries
            .into_iter()
            .filter_map(|entry| messages_by_id.get(&entry.message_id).cloned())
            .collect::<Vec<_>>();
        replay_messages.sort_by(|left, right| match (left.message_seq, right.message_seq) {
            (Some(left_seq), Some(right_seq)) => left_seq
                .cmp(&right_seq)
                .then_with(|| left.created_at.cmp(&right.created_at)),
            _ => left.created_at.cmp(&right.created_at),
        });

        let active_tasks = self
            .runtime_db
            .tasks()
            .active_for_agent(agent_id, usize::MAX)?;
        let active_timers = self
            .latest_timer_records()?
            .into_iter()
            .filter(|record| record.agent_id == agent_id && record.status == TimerStatus::Active)
            .collect::<Vec<TimerRecord>>();

        Ok(RecoverySnapshot {
            agent,
            replay_messages,
            active_tasks,
            active_timers,
            work_items: self.latest_work_items()?,
            work_item_delegations: self.latest_work_item_delegations()?,
        })
    }

    pub(crate) fn active_wait_conditions_for_agent(
        &self,
        agent_id: &str,
    ) -> Result<Vec<WaitConditionRecord>> {
        let records = self
            .runtime_db
            .wait_conditions()
            .active_for_agent(agent_id)?;
        self.filter_active_wait_conditions_for_live_scope(records)
    }

    pub(crate) fn active_wait_conditions(&self) -> Result<Vec<WaitConditionRecord>> {
        let records = self.runtime_db.wait_conditions().active_all()?;
        self.filter_active_wait_conditions_for_live_scope(records)
    }

    fn filter_active_wait_conditions_for_live_scope(
        &self,
        records: Vec<WaitConditionRecord>,
    ) -> Result<Vec<WaitConditionRecord>> {
        let mut work_item_is_open = BTreeMap::<String, bool>::new();
        let mut live = Vec::new();
        for record in records {
            let Some(work_item_id) = record.work_item_id.as_deref() else {
                live.push(record);
                continue;
            };
            let is_open = match work_item_is_open.get(work_item_id) {
                Some(is_open) => *is_open,
                None => {
                    let is_open = self
                        .runtime_db
                        .work_items()
                        .latest(work_item_id)?
                        .is_some_and(|item| item.state == WorkItemState::Open);
                    work_item_is_open.insert(work_item_id.to_string(), is_open);
                    is_open
                }
            };
            if is_open {
                live.push(record);
            }
        }
        Ok(live)
    }

    fn read_agent(&self) -> Result<Option<AgentState>> {
        match self.agent_id.as_deref() {
            Some(agent_id) => self.runtime_db.agent_states().latest(agent_id),
            None => Ok(None),
        }
    }

    fn latest_external_triggers(&self) -> Result<Vec<crate::types::ExternalTriggerRecord>> {
        match self.agent_id.as_deref() {
            Some(agent_id) => self
                .runtime_db
                .external_triggers()
                .latest_for_agent(agent_id),
            None => self.runtime_db.external_triggers().latest_all(),
        }
    }

    fn latest_work_items(&self) -> Result<Vec<WorkItemRecord>> {
        match self.agent_id.as_deref() {
            Some(agent_id) => self
                .runtime_db
                .work_items()
                .latest_for_agent(agent_id, usize::MAX),
            None => self.runtime_db.work_items().latest_all(),
        }
    }

    fn latest_timer_records(&self) -> Result<Vec<TimerRecord>> {
        match self.agent_id.as_deref() {
            Some(agent_id) => self
                .runtime_db
                .timers()
                .recent_for_agent(agent_id, usize::MAX),
            None => self.runtime_db.timers().latest_all(),
        }
    }

    fn latest_work_item_delegations(&self) -> Result<Vec<WorkItemDelegationRecord>> {
        match self.agent_id.as_deref() {
            Some(agent_id) => self
                .runtime_db
                .work_item_delegations()
                .recent_for_agent(agent_id, usize::MAX),
            None => self.runtime_db.work_item_delegations().latest_all(),
        }
    }
}
