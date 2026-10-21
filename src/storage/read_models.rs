//! Runtime read-model and recovery projections over canonical persistence.

use crate::runtime_db::agent_list::AgentPostureFacts;

use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::Result;

use crate::diagnostics::attribution;

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

/// Terminal work items retained in scheduling projections. Must cover the
/// HTTP state bootstrap slice (`STATE_BOOTSTRAP_WORK_ITEM_LIMIT`) plus the
/// small per-class windows the read model surfaces.
pub(crate) const WORK_QUEUE_NON_OPEN_WINDOW: usize = 128;

#[derive(Debug, Clone)]
pub struct RuntimeReadModels {
    runtime_db: RuntimeDb,
    agent_id: Option<String>,
}

/// Wait conditions that stay live for posture derivation: conditions with
/// no referenced work item always stay live, and conditions whose
/// referenced work item is still open stay live. Open work items are
/// unbounded above, so open-set membership equals the per-agent
/// `latest_many` openness check in
/// `filter_active_wait_conditions_for_live_scope`.
fn live_scoped_wait_conditions(facts: &AgentPostureFacts) -> Vec<WaitConditionRecord> {
    let open_work_item_ids = facts
        .open_work_items
        .iter()
        .map(|item| item.id.as_str())
        .collect::<BTreeSet<_>>();
    facts
        .active_wait_conditions
        .iter()
        .filter(|condition| {
            condition
                .work_item_id
                .as_deref()
                .map_or(true, |work_item_id| {
                    open_work_item_ids.contains(work_item_id)
                })
        })
        .cloned()
        .collect()
}

/// Pure work-queue assembly shared by the per-agent read model and batch
/// projections. Mirrors the query-backed assembly exactly: facts come from
/// one committed read view, so the response never mixes commits.
pub(crate) fn work_queue_from_facts(facts: &AgentPostureFacts) -> WorkQueueReadModel {
    let conditions = live_scoped_wait_conditions(facts);
    work_queue_from_facts_with_conditions(facts, &conditions)
}

fn work_queue_from_facts_with_conditions(
    facts: &AgentPostureFacts,
    conditions: &[WaitConditionRecord],
) -> WorkQueueReadModel {
    let current_work_item_id = facts.current_work_item_id.clone();
    let mut latest = HashMap::<String, WorkItemRecord>::new();
    for record in &facts.open_work_items {
        latest.insert(record.id.clone(), record.clone());
    }
    for record in &facts.non_open_work_items {
        latest.insert(record.id.clone(), record.clone());
    }

    let current = current_work_item_id
        .as_deref()
        .and_then(|id| latest.get(id))
        .filter(|item| item.state == WorkItemState::Open)
        .cloned();

    let trigger_delivery_by_id = facts
        .external_triggers
        .iter()
        .filter(|trigger| trigger.status == ExternalTriggerStatus::Active)
        .filter_map(|trigger| {
            trigger
                .last_delivered_at
                .map(|delivered_at| (trigger.external_trigger_id.clone(), delivered_at))
        })
        .collect::<BTreeMap<_, _>>();
    let active_wait_conditions = conditions
        .iter()
        .filter_map(|condition| {
            condition
                .work_item_id
                .clone()
                .map(|id| (id, condition.clone()))
        })
        .fold(
            BTreeMap::<String, Vec<WaitConditionRecord>>::new(),
            |mut acc, (id, condition)| {
                acc.entry(id).or_default().push(condition);
                acc
            },
        );
    let active_continuation_suspended_ids = facts
        .active_continuations
        .iter()
        .map(|frame| frame.suspended_work_item_id.clone())
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
    let completing = items
        .iter()
        .filter(|item| item.candidate_class == WorkItemCandidateClass::Completing)
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

    WorkQueueReadModel {
        current,
        queued_blocked,
        items,
        current_runnable,
        triggered_blocked,
        queued_runnable,
        yielded,
        waiting_for_operator,
        blocked,
        completing,
        completed_recent,
    }
}

/// Pure posture derivation shared by the per-agent read model and batched
/// projections. `agent` is the caller's AgentState; every other fact comes
/// from one committed read view.
pub(crate) fn agent_posture_from_facts(
    agent: &AgentState,
    facts: &AgentPostureFacts,
) -> AgentPostureProjection {
    if matches!(agent.status, AgentStatus::Stopped) {
        return AgentPostureProjection {
            posture: AgentSchedulingPosture::Stopped,
            reason: "agent lifecycle is stopped".into(),
            work_item_id: None,
            task_id: None,
            run_id: None,
        };
    }

    if let Some(run_id) = agent.current_run_id.clone() {
        return AgentPostureProjection {
            posture: AgentSchedulingPosture::ActiveTurn,
            reason: "agent has an active turn".into(),
            work_item_id: agent.current_turn_work_item_id.clone(),
            task_id: None,
            run_id: Some(run_id),
        };
    }

    if facts.has_queued_input {
        return AgentPostureProjection {
            posture: AgentSchedulingPosture::HasQueuedInput,
            reason: "agent has queued input".into(),
            work_item_id: None,
            task_id: None,
            run_id: None,
        };
    }

    let conditions = live_scoped_wait_conditions(facts);
    let work_queue = work_queue_from_facts_with_conditions(facts, &conditions);
    agent_posture_rules_from_work_queue(&work_queue, &conditions)
}

/// Posture precedence rules over an assembled work queue. The live wait
/// conditions carry the `WaitingForTask` subject reference.
fn agent_posture_rules_from_work_queue(
    work_queue: &WorkQueueReadModel,
    live_wait_conditions: &[WaitConditionRecord],
) -> AgentPostureProjection {
    if let Some(item) = work_queue
        .current_runnable
        .as_ref()
        .or_else(|| work_queue.queued_runnable.first())
    {
        return AgentPostureProjection {
            posture: AgentSchedulingPosture::HasRunnableWork,
            reason: item.posture_reason(),
            work_item_id: Some(item.work_item.id.clone()),
            task_id: None,
            run_id: None,
        };
    }

    if let Some(item) = work_queue.items.iter().find(|item| {
        item.is_current && item.scheduling_state == WorkItemSchedulingState::WaitingTask
    }) {
        let task_id = live_wait_conditions
            .iter()
            .find(|condition| {
                condition.status == WaitConditionStatus::Active
                    && condition.kind == WaitConditionKind::Task
                    && condition.work_item_id.as_deref() == Some(item.work_item.id.as_str())
            })
            .and_then(|condition| condition.subject_ref.clone());
        return AgentPostureProjection {
            posture: AgentSchedulingPosture::WaitingForTask,
            reason: item.posture_reason(),
            work_item_id: Some(item.work_item.id.clone()),
            task_id,
            run_id: None,
        };
    }

    if let Some(item) = work_queue.items.iter().find(|item| {
        item.is_current && item.scheduling_state == WorkItemSchedulingState::WaitingExternal
    }) {
        return AgentPostureProjection {
            posture: AgentSchedulingPosture::WaitingForExternal,
            reason: item.posture_reason(),
            work_item_id: Some(item.work_item.id.clone()),
            task_id: None,
            run_id: None,
        };
    }

    if let Some(item) = work_queue
        .waiting_for_operator
        .iter()
        .find(|item| item.is_current)
    {
        return AgentPostureProjection {
            posture: AgentSchedulingPosture::WaitingForOperator,
            reason: item.posture_reason(),
            work_item_id: Some(item.work_item.id.clone()),
            task_id: None,
            run_id: None,
        };
    }

    if let Some(item) = work_queue
        .blocked
        .iter()
        .find(|item| item.is_current)
        .or_else(|| {
            work_queue
                .triggered_blocked
                .iter()
                .find(|item| item.is_current)
        })
    {
        return AgentPostureProjection {
            posture: AgentSchedulingPosture::Blocked,
            reason: item.posture_reason(),
            work_item_id: Some(item.work_item.id.clone()),
            task_id: None,
            run_id: None,
        };
    }

    AgentPostureProjection {
        posture: AgentSchedulingPosture::Idle,
        reason: "no queued input, active turn, runnable work, or active waits".into(),
        work_item_id: None,
        task_id: None,
        run_id: None,
    }
}

impl RuntimeReadModels {
    pub(crate) fn new(runtime_db: RuntimeDb, agent_id: Option<String>) -> Self {
        Self {
            runtime_db,
            agent_id,
        }
    }

    pub fn work_queue(&self) -> Result<WorkQueueReadModel> {
        let _timer = attribution::WORK_QUEUE.start();
        Ok(work_queue_from_facts(&self.work_queue_facts()?))
    }

    /// Gathers this read model's per-agent facts through the per-agent
    /// repository queries (one connection per query). The pure assembly in
    /// [`work_queue_from_facts`] and the batched `/agents/list` reader
    /// consume the same fact bundle.
    fn work_queue_facts(&self) -> Result<AgentPostureFacts> {
        let Some(agent_id) = self.agent_id.as_deref() else {
            return Ok(AgentPostureFacts::default());
        };
        let current_work_item_id = self
            .read_agent()?
            .and_then(|agent| agent.current_work_item_id);
        let mut timer = attribution::WORK_ITEMS.start();
        let open_work_items = self.runtime_db.work_items().open_for_agent(agent_id)?;
        let non_open_work_items = self
            .runtime_db
            .work_items()
            .latest_non_open_for_agent(agent_id, WORK_QUEUE_NON_OPEN_WINDOW)?;
        timer.rows(open_work_items.len() + non_open_work_items.len());
        let external_triggers = self.latest_external_triggers()?;
        let active_wait_conditions = self.raw_active_wait_conditions_for_scope()?;
        let active_continuations = self
            .runtime_db
            .work_item_continuations()
            .active_for_agent(agent_id)?;
        Ok(AgentPostureFacts {
            current_work_item_id,
            // Filled by `agent_posture` after its queued-input check.
            has_queued_input: false,
            open_work_items,
            non_open_work_items,
            external_triggers,
            active_wait_conditions,
            active_continuations,
        })
    }

    /// Active wait conditions for this read model's scope before the
    /// live-scope filter. [`live_scoped_wait_conditions`] applies the
    /// filter from the open work-item set, so query-backed and batched
    /// projections share one filter implementation.
    fn raw_active_wait_conditions_for_scope(&self) -> Result<Vec<WaitConditionRecord>> {
        match self.agent_id.as_deref() {
            Some(agent_id) => {
                let mut timer = attribution::WAIT_QUERY_AGENT.start();
                let records = self
                    .runtime_db
                    .wait_conditions()
                    .active_for_agent(agent_id)?;
                timer.rows(records.len());
                Ok(records)
            }
            None => {
                let mut timer = attribution::WAIT_QUERY.start();
                let records = self.runtime_db.wait_conditions().active_all()?;
                timer.rows(records.len());
                Ok(records)
            }
        }
    }

    pub fn agent_posture(&self, agent: &AgentState) -> Result<AgentPostureProjection> {
        if matches!(agent.status, AgentStatus::Stopped) || agent.current_run_id.is_some() {
            // Decidable from the AgentState alone; no repository reads.
            return Ok(agent_posture_from_facts(
                agent,
                &AgentPostureFacts::default(),
            ));
        }
        if self
            .runtime_db
            .queue_entries()
            .has_queued_for_agent(&agent.id)?
        {
            let mut facts = AgentPostureFacts::default();
            facts.has_queued_input = true;
            return Ok(agent_posture_from_facts(agent, &facts));
        }
        let facts = self.work_queue_facts()?;
        Ok(agent_posture_from_facts(agent, &facts))
    }

    pub fn agent_posture_with_work_queue(
        &self,
        agent: &AgentState,
        work_queue: &WorkQueueReadModel,
    ) -> Result<AgentPostureProjection> {
        if matches!(agent.status, AgentStatus::Stopped) || agent.current_run_id.is_some() {
            return Ok(agent_posture_from_facts(
                agent,
                &AgentPostureFacts::default(),
            ));
        }
        if self
            .runtime_db
            .queue_entries()
            .has_queued_for_agent(&agent.id)?
        {
            let mut facts = AgentPostureFacts::default();
            facts.has_queued_input = true;
            return Ok(agent_posture_from_facts(agent, &facts));
        }
        self.agent_posture_from_work_queue(work_queue)
    }

    fn agent_posture_from_work_queue(
        &self,
        work_queue: &WorkQueueReadModel,
    ) -> Result<AgentPostureProjection> {
        Ok(agent_posture_rules_from_work_queue(
            work_queue,
            &self.active_wait_conditions_for_scope()?,
        ))
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
        let mut timer = attribution::WAIT_QUERY_AGENT.start();
        let records = self
            .runtime_db
            .wait_conditions()
            .active_for_agent(agent_id)?;
        timer.rows(records.len());
        drop(timer);
        self.filter_active_wait_conditions_for_live_scope(records)
    }

    pub(crate) fn active_wait_conditions(&self) -> Result<Vec<WaitConditionRecord>> {
        let mut timer = attribution::WAIT_QUERY.start();
        let records = self.runtime_db.wait_conditions().active_all()?;
        timer.rows(records.len());
        drop(timer);
        self.filter_active_wait_conditions_for_live_scope(records)
    }

    /// Active wait conditions for this read model's agent scope.
    ///
    /// Wait conditions always reference work items owned by the waiting agent
    /// (enforced by `validate_owned_work_item`), so agent-scoped projections
    /// such as the work queue and posture derivation can filter in SQL instead
    /// of scanning every agent's wait rows. Global-scope read models keep the
    /// unfiltered query.
    pub(crate) fn active_wait_conditions_for_scope(&self) -> Result<Vec<WaitConditionRecord>> {
        match self.agent_id.as_deref() {
            Some(agent_id) => self.active_wait_conditions_for_agent(agent_id),
            None => self.active_wait_conditions(),
        }
    }

    fn filter_active_wait_conditions_for_live_scope(
        &self,
        records: Vec<WaitConditionRecord>,
    ) -> Result<Vec<WaitConditionRecord>> {
        let mut timer = attribution::WAIT_FILTER.start();
        timer.rows(records.len());
        let referenced_ids = records
            .iter()
            .filter_map(|record| record.work_item_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let work_item_is_open = {
            let mut lookup = attribution::WAIT_ITEM.start();
            let items = self.runtime_db.work_items().latest_many(&referenced_ids)?;
            lookup.rows(items.len());
            items
                .into_iter()
                .map(|item| (item.id, item.state == WorkItemState::Open))
                .collect::<BTreeMap<_, _>>()
        };
        let mut live = Vec::new();
        for record in records {
            let Some(work_item_id) = record.work_item_id.as_deref() else {
                live.push(record);
                continue;
            };
            if work_item_is_open
                .get(work_item_id)
                .is_some_and(|is_open| *is_open)
            {
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
