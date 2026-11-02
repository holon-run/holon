//! Runtime read-model and recovery projections over canonical persistence.

#[cfg(test)]
use crate::runtime_db::agent_list::AgentPostureFacts;
#[cfg(test)]
use std::collections::{BTreeSet, HashMap};
use std::{borrow::Cow, collections::BTreeMap};

mod queue_latest;
pub(crate) use queue_latest::{QueueLatestWork, QueueWorkBuilder};

use anyhow::{Context, Result};

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

#[cfg(test)]
#[path = "read_models/posture_material_tests.rs"]
mod posture_material_tests;

#[cfg(test)]
#[path = "read_models/snapshot_view_tests.rs"]
mod snapshot_view_tests;

#[cfg(test)]
#[path = "read_models/work_queue_ownership_tests.rs"]
mod work_queue_ownership_tests;

#[cfg(test)]
#[path = "read_models/queue_trigger_material_tests.rs"]
mod queue_trigger_material_tests;

#[cfg(test)]
#[path = "read_models/queue_todo_material_tests.rs"]
mod queue_todo_material_tests;

#[cfg(test)]
#[path = "read_models/queue_diagnostics_material_tests.rs"]
mod queue_diagnostics_material_tests;

#[cfg(test)]
#[path = "read_models/queue_wait_summary_material_tests.rs"]
mod queue_wait_summary_material_tests;

#[cfg(test)]
#[path = "read_models/wait_scope_material_tests.rs"]
mod wait_scope_material_tests;

#[cfg(test)]
#[path = "read_models/queue_display_material_tests.rs"]
mod queue_display_material_tests;

#[cfg(test)]
#[path = "read_models/queue_items_material_tests.rs"]
mod queue_items_material_tests;

#[cfg(test)]
#[path = "read_models/queue_latest_key_material_tests.rs"]
mod queue_latest_key_material_tests;

#[cfg(test)]
#[path = "read_models/queue_continuation_material_tests.rs"]
mod queue_continuation_material_tests;

#[cfg(test)]
#[path = "read_models/queue_current_id_tests.rs"]
mod queue_current_id_tests;

#[cfg(test)]
#[path = "read_models/queue_sort_tests.rs"]
mod queue_sort_tests;

#[cfg(test)]
#[path = "read_models/queue_wait_index_tests.rs"]
mod queue_wait_index_tests;

use super::{
    recovery::RecoverySnapshot,
    work_queue::{
        compare_queue_display_order, compare_scheduling_projection_order, WorkItemCandidateClass,
        WorkItemSchedulingProjection, WorkQueueReadModel,
    },
};

/// Terminal work items retained in scheduling projections. Must cover the
/// HTTP state bootstrap slice (`STATE_BOOTSTRAP_WORK_ITEM_LIMIT`) plus the
/// small per-class windows the read model surfaces.
pub(crate) const WORK_QUEUE_NON_OPEN_WINDOW: usize = 128;

/// Keep the owning API independent of its source storage's lifetime.
pub type RuntimeReadModels = RuntimeReadModelView<'static>;

#[derive(Debug, Clone)]
pub struct RuntimeReadModelView<'a> {
    runtime_db: Cow<'a, RuntimeDb>,
    agent_id: Option<Cow<'a, str>>,
}

/// Wait conditions that stay live for posture derivation: conditions with
/// no referenced work item always stay live, and conditions whose
/// referenced work item is still open stay live. Open work items are
/// unbounded above, so open-set membership equals the per-agent
/// `latest_many` openness check in
/// `filter_active_wait_conditions_for_live_scope`.
#[cfg(test)]
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

#[cfg(test)]
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
#[cfg(test)]
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
#[cfg(test)]
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
            runtime_db: Cow::Owned(runtime_db),
            agent_id: agent_id.map(Cow::Owned),
        }
    }

    /// Snapshot posture needs only the existing SQL handle and identity scope.
    pub(crate) fn borrowed_for_agent<'a>(
        runtime_db: &'a RuntimeDb,
        agent_id: &'a str,
    ) -> Result<RuntimeReadModelView<'a>> {
        anyhow::ensure!(
            !agent_id.trim().is_empty(),
            "agent-scoped storage requires a non-empty agent id"
        );
        Ok(RuntimeReadModelView {
            runtime_db: Cow::Borrowed(runtime_db),
            agent_id: Some(Cow::Borrowed(agent_id)),
        })
    }
}

impl RuntimeReadModelView<'_> {
    pub(crate) fn work_queue_snapshot(
        &self,
        expected_incarnation: Option<u64>,
    ) -> Result<
        Option<(
            crate::runtime_db::conversation::ConversationIdentity,
            WorkQueueReadModel,
        )>,
    > {
        let _timer = attribution::WORK_QUEUE.start();
        let Some(agent_id) = self.agent_id.as_deref() else {
            return Ok(None);
        };
        let Some((identity, row)) = self
            .runtime_db
            .agent_work_queue_snapshot(agent_id, expected_incarnation)?
        else {
            return Ok(None);
        };
        let mut facts = row.facts.map_err(anyhow::Error::msg)?;
        facts.current_work_item_id = row
            .agent_state_json
            .map(|payload| serde_json::from_str::<AgentState>(&payload))
            .transpose()?
            .and_then(|agent| agent.current_work_item_id);
        let mut latest = QueueWorkBuilder::default();
        for record in facts
            .open_work_items
            .into_iter()
            .chain(facts.non_open_work_items)
        {
            latest.push(clone_queue_work_item_key(&record.id)?, record)?;
        }
        let latest = latest.finish();
        facts.active_wait_conditions.retain(|condition| {
            condition.work_item_id.as_deref().is_none_or(|id| {
                latest
                    .get(id)
                    .is_some_and(|item| item.state == WorkItemState::Open)
            })
        });
        let work_queue = derive_context_work_queue(
            facts.current_work_item_id.as_deref(),
            latest,
            facts.external_triggers,
            facts.active_wait_conditions,
            facts.active_continuations,
        )?;
        Ok(Some((identity, work_queue)))
    }

    pub(crate) fn agent_tasks_snapshot(
        &self,
        expected_incarnation: Option<u64>,
        terminal: bool,
        limit: usize,
    ) -> Result<
        Option<(
            crate::runtime_db::conversation::ConversationIdentity,
            Vec<crate::types::TaskRecord>,
        )>,
    > {
        let Some(agent_id) = self.agent_id.as_deref() else {
            return Ok(None);
        };
        self.runtime_db
            .agent_tasks_snapshot(agent_id, expected_incarnation, terminal, limit)
    }

    pub(crate) fn agent_timers_snapshot(
        &self,
        expected_incarnation: Option<u64>,
        limit: usize,
    ) -> Result<
        Option<(
            crate::runtime_db::conversation::ConversationIdentity,
            Vec<TimerRecord>,
        )>,
    > {
        let Some(agent_id) = self.agent_id.as_deref() else {
            return Ok(None);
        };
        self.runtime_db
            .agent_timers_snapshot(agent_id, expected_incarnation, limit)
    }

    /// One committed view of this agent's identity fence plus the latest
    /// task record, mirroring [`work_queue_snapshot`] admission.
    pub(crate) fn agent_task_snapshot(
        &self,
        task_id: &str,
        expected_incarnation: Option<u64>,
    ) -> Result<
        Option<(
            crate::runtime_db::conversation::ConversationIdentity,
            Option<crate::types::TaskRecord>,
        )>,
    > {
        let Some(agent_id) = self.agent_id.as_deref() else {
            return Ok(None);
        };
        self.runtime_db
            .agent_task_snapshot(agent_id, task_id, expected_incarnation)
    }

    /// One committed view of this agent's identity fence plus one tool
    /// execution record, mirroring [`work_queue_snapshot`] admission.
    pub(crate) fn agent_tool_execution_snapshot(
        &self,
        tool_execution_id: &str,
        expected_incarnation: Option<u64>,
    ) -> Result<
        Option<(
            crate::runtime_db::conversation::ConversationIdentity,
            Option<crate::types::ToolExecutionRecord>,
        )>,
    > {
        let Some(agent_id) = self.agent_id.as_deref() else {
            return Ok(None);
        };
        self.runtime_db.agent_tool_execution_snapshot(
            agent_id,
            tool_execution_id,
            expected_incarnation,
        )
    }

    pub fn work_queue(&self) -> Result<WorkQueueReadModel> {
        let current_work_item_id = self
            .read_agent()?
            .and_then(|agent| agent.current_work_item_id);
        self.work_queue_with_trigger_observation(
            current_work_item_id.as_deref(),
            None::<std::slice::Iter<'_, crate::types::ExternalTriggerRecord>>,
        )
    }

    pub(crate) fn work_queue_with_observed_scheduling<'a, I>(
        &self,
        state: &AgentState,
        triggers: I,
    ) -> Result<WorkQueueReadModel>
    where
        I: IntoIterator<Item = &'a crate::types::ExternalTriggerRecord>,
        I::IntoIter: Clone,
    {
        let triggers = triggers.into_iter();
        anyhow::ensure!(
            self.agent_id.as_deref() == Some(state.id.as_str())
                && triggers.clone().all(|row| row.target_agent_id == state.id),
            "WorkQueue Scheduling observation scope differs"
        );
        self.work_queue_with_trigger_observation(
            state.current_work_item_id.as_deref(),
            Some(triggers),
        )
    }

    fn work_queue_with_trigger_observation<'a>(
        &self,
        current_work_item_id: Option<&str>,
        observed: Option<impl Iterator<Item = &'a crate::types::ExternalTriggerRecord> + Clone>,
    ) -> Result<WorkQueueReadModel> {
        let _timer = attribution::WORK_QUEUE.start();
        let mut latest = QueueWorkBuilder::default();
        let latest = if let Some(agent_id) = self.agent_id.as_deref() {
            let mut timer = attribution::WORK_ITEMS.start();
            for record in self.runtime_db.work_items().open_for_agent(agent_id)? {
                latest.push(clone_queue_work_item_key(&record.id)?, record)?;
            }
            for record in self
                .runtime_db
                .work_items()
                .latest_non_open_for_agent(agent_id, WORK_QUEUE_NON_OPEN_WINDOW)?
            {
                latest.push(clone_queue_work_item_key(&record.id)?, record)?;
            }
            let latest = latest.finish();
            timer.rows(latest.len());
            latest
        } else {
            latest.finish()
        };

        let fetched = if observed.is_none() {
            Some(self.latest_external_triggers()?)
        } else {
            None
        };
        let waits = self.active_wait_conditions_for_scope()?;
        let continuations = self
            .runtime_db
            .work_item_continuations()
            .active_for_agent(self.agent_id.as_deref().unwrap_or_default())?;
        match observed {
            Some(triggers) => derive_observed_context_work_queue(
                current_work_item_id,
                latest,
                triggers,
                waits,
                continuations,
            ),
            None => derive_context_work_queue(
                current_work_item_id,
                latest,
                fetched.expect("unobserved Trigger query"),
                waits,
                continuations,
            ),
        }
    }

    pub fn agent_posture(&self, agent: &AgentState) -> Result<AgentPostureProjection> {
        self.agent_posture_with_material(agent, false)
    }

    pub(crate) fn agent_posture_for_snapshot(
        &self,
        agent: &AgentState,
    ) -> Result<AgentPostureProjection> {
        self.agent_posture_with_material(agent, true)
    }

    fn agent_posture_with_material(
        &self,
        agent: &AgentState,
        admit_material: bool,
    ) -> Result<AgentPostureProjection> {
        if let Some(posture) = self.agent_posture_before_work_queue(agent, admit_material)? {
            return Ok(posture);
        }
        // The caller already selected State. Do not replace its focus with
        // another read (or decode the same body twice under its allowance).
        let work_queue = self.work_queue_with_trigger_observation(
            agent.current_work_item_id.as_deref(),
            None::<std::slice::Iter<'_, crate::types::ExternalTriggerRecord>>,
        )?;
        self.agent_posture_from_work_queue(&work_queue, admit_material)
    }

    pub fn agent_posture_with_work_queue(
        &self,
        agent: &AgentState,
        work_queue: &WorkQueueReadModel,
    ) -> Result<AgentPostureProjection> {
        if let Some(posture) = self.agent_posture_before_work_queue(agent, false)? {
            return Ok(posture);
        }
        self.agent_posture_from_work_queue(work_queue, false)
    }

    fn agent_posture_before_work_queue(
        &self,
        agent: &AgentState,
        admit_material: bool,
    ) -> Result<Option<AgentPostureProjection>> {
        if let Some(posture) = derive_agent_posture_before_work_queue(agent, false) {
            return posture.materialize(admit_material).map(Some);
        }
        derive_agent_posture_before_work_queue(
            agent,
            self.runtime_db
                .queue_entries()
                .has_queued_for_agent(&agent.id)?,
        )
        .map(|posture| posture.materialize(admit_material))
        .transpose()
    }

    fn agent_posture_from_work_queue(
        &self,
        work_queue: &WorkQueueReadModel,
        admit_material: bool,
    ) -> Result<AgentPostureProjection> {
        let needs_task_wait = work_queue.current_runnable.is_none()
            && work_queue.queued_runnable.is_empty()
            && work_queue.items.iter().any(|item| {
                item.is_current && item.scheduling_state == WorkItemSchedulingState::WaitingTask
            });
        let active_waits = if needs_task_wait {
            self.active_wait_conditions_for_scope()?
        } else {
            Vec::new()
        };
        derive_agent_posture_from_work_queue(work_queue, &active_waits).materialize(admit_material)
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
        mut records: Vec<WaitConditionRecord>,
    ) -> Result<Vec<WaitConditionRecord>> {
        let mut timer = attribution::WAIT_FILTER.start();
        timer.rows(records.len());
        let count = records
            .iter()
            .filter(|record| record.work_item_id.is_some())
            .count();
        let mut referenced_ids = Vec::<&str>::new();
        admit_wait_scope_slots::<&str>(count)?;
        referenced_ids.try_reserve_exact(count)?;
        referenced_ids.extend(
            records
                .iter()
                .filter_map(|record| record.work_item_id.as_deref()),
        );
        referenced_ids.sort_unstable();
        referenced_ids.dedup();
        let items = {
            let mut lookup = attribution::WAIT_ITEM.start();
            let items = self
                .runtime_db
                .work_items()
                .latest_many_borrowed(&referenced_ids)?;
            lookup.rows(items.len());
            items
        };
        drop(referenced_ids);
        let mut states = Vec::<(String, usize, bool)>::new();
        admit_wait_scope_slots::<(String, usize, bool)>(items.len())?;
        states.try_reserve_exact(items.len())?;
        states.extend(
            items
                .into_iter()
                .enumerate()
                .map(|(index, item)| (item.id, index, item.state == WorkItemState::Open)),
        );
        // Payload IDs may differ from SQL keys: preserve the old map's last-wins rule.
        states.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.cmp(&a.1)));
        states.dedup_by(|a, b| a.0 == b.0);
        records.retain(|record| {
            record.work_item_id.as_ref().is_none_or(|id| {
                states
                    .binary_search_by(|state| state.0.cmp(id))
                    .is_ok_and(|index| states[index].2)
            })
        });
        Ok(records)
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

fn admit_wait_scope_slots<T>(count: usize) -> Result<()> {
    if count > 0 {
        crate::diagnostics::resource_io::charge_owned_metadata(
            u64::try_from(
                count
                    .checked_mul(std::mem::size_of::<T>())
                    .ok_or_else(|| anyhow::anyhow!("wait scope capacity overflow"))?,
            )?,
            count,
        )?;
    }
    Ok(())
}

fn queue_continuation_suspended_ids(
    frames: Vec<crate::types::WorkItemContinuationFrame>,
) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    if !frames.is_empty() {
        let bytes = frames
            .len()
            .checked_mul(std::mem::size_of::<String>())
            .and_then(|bytes| u64::try_from(bytes).ok())
            .ok_or_else(|| std::io::Error::other("ResourceExhausted: queue continuation slots"))?;
        crate::diagnostics::resource_io::charge_owned_metadata(bytes, frames.len())
            .context("queue continuation slots admission")?;
        ids.try_reserve_exact(frames.len()).map_err(|_| {
            std::io::Error::other("ResourceExhausted: queue continuation allocation")
        })?;
    }
    for frame in frames {
        ids.push(frame.suspended_work_item_id);
    }
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

/// Pure scheduling/display selection shared by legacy and coherent context reads.
fn group_queue_waits(mut waits: Vec<WaitConditionRecord>) -> Vec<WaitConditionRecord> {
    waits.retain(|condition| condition.work_item_id.is_some());
    // Canonical callers provide unique wait IDs, making this a total order.
    waits.sort_unstable_by(|left, right| {
        left.work_item_id
            .cmp(&right.work_item_id)
            .then_with(|| left.created_at.cmp(&right.created_at))
            .then_with(|| left.id.cmp(&right.id))
    });
    waits
}

fn queue_waits_for_item<'a>(
    waits: &'a [WaitConditionRecord],
    work_item_id: &str,
) -> &'a [WaitConditionRecord] {
    let start =
        waits.partition_point(|condition| condition.work_item_id.as_deref() < Some(work_item_id));
    let end = start
        + waits[start..]
            .partition_point(|condition| condition.work_item_id.as_deref() == Some(work_item_id));
    &waits[start..end]
}

type QueueTriggerDelivery<K = String> = (K, chrono::DateTime<chrono::Utc>, usize);

fn queue_trigger_deliveries(
    triggers: Vec<crate::types::ExternalTriggerRecord>,
) -> Result<Vec<QueueTriggerDelivery>> {
    let count = triggers
        .iter()
        .filter(|row| {
            row.status == ExternalTriggerStatus::Active && row.last_delivered_at.is_some()
        })
        .count();
    queue_trigger_index(
        count,
        triggers
            .into_iter()
            .enumerate()
            .filter_map(|(ordinal, trigger)| {
                (trigger.status == ExternalTriggerStatus::Active)
                    .then_some(trigger.last_delivered_at)
                    .flatten()
                    .map(|time| (trigger.external_trigger_id, time, ordinal))
            }),
    )
}

fn queue_observed_trigger_deliveries<'a>(
    triggers: impl Iterator<Item = &'a crate::types::ExternalTriggerRecord> + Clone,
) -> Result<Vec<QueueTriggerDelivery<&'a str>>> {
    let rows = triggers.enumerate().filter_map(|(ordinal, trigger)| {
        (trigger.status == ExternalTriggerStatus::Active)
            .then_some(trigger.last_delivered_at)
            .flatten()
            .map(|time| (trigger.external_trigger_id.as_str(), time, ordinal))
    });
    queue_trigger_index(rows.clone().count(), rows)
}

fn queue_trigger_index<K: AsRef<str>>(
    count: usize,
    rows: impl Iterator<Item = QueueTriggerDelivery<K>>,
) -> Result<Vec<QueueTriggerDelivery<K>>> {
    let mut deliveries = Vec::new();
    if count != 0 {
        let bytes = count
            .checked_mul(std::mem::size_of::<QueueTriggerDelivery<K>>())
            .and_then(|bytes| u64::try_from(bytes).ok())
            .ok_or_else(|| std::io::Error::other("ResourceExhausted: queue trigger slots"))?;
        crate::diagnostics::resource_io::charge_owned_metadata(bytes, count)
            .context("queue trigger slots admission")?;
        deliveries
            .try_reserve_exact(count)
            .map_err(|_| std::io::Error::other("ResourceExhausted: queue trigger allocation"))?;
    }
    deliveries.extend(rows);
    // Descending ordinal preserves the old map's last effective input per ID.
    deliveries.sort_unstable_by(|left, right| {
        left.0
            .as_ref()
            .cmp(right.0.as_ref())
            .then_with(|| right.2.cmp(&left.2))
    });
    deliveries.dedup_by(|left, right| left.0.as_ref() == right.0.as_ref());
    Ok(deliveries)
}

fn queue_trigger_delivery<K: AsRef<str>>(
    deliveries: &[QueueTriggerDelivery<K>],
    id: &str,
) -> Option<chrono::DateTime<chrono::Utc>> {
    deliveries
        .binary_search_by(|row| row.0.as_ref().cmp(id))
        .ok()
        .map(|index| deliveries[index].1)
}

pub(crate) fn clone_queue_work_item_key(id: &str) -> Result<String> {
    let bytes = id
        .len()
        .checked_add(std::mem::size_of::<String>())
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or_else(|| std::io::Error::other("ResourceExhausted: queue latest key size"))?;
    crate::diagnostics::resource_io::charge_owned_metadata(bytes, 1)
        .context("queue latest key admission")?;
    Ok(id.to_owned())
}

pub(crate) fn derive_context_work_queue(
    current_work_item_id: Option<&str>,
    latest: QueueLatestWork,
    external_triggers: Vec<crate::types::ExternalTriggerRecord>,
    active_waits: Vec<WaitConditionRecord>,
    continuations: Vec<crate::types::WorkItemContinuationFrame>,
) -> Result<WorkQueueReadModel> {
    derive_work_queue_with_trigger_index(
        current_work_item_id,
        latest,
        || queue_trigger_deliveries(external_triggers),
        active_waits,
        continuations,
    )
}

fn derive_observed_context_work_queue<'a, I>(
    current_work_item_id: Option<&str>,
    latest: QueueLatestWork,
    external_triggers: I,
    active_waits: Vec<WaitConditionRecord>,
    continuations: Vec<crate::types::WorkItemContinuationFrame>,
) -> Result<WorkQueueReadModel>
where
    I: IntoIterator<Item = &'a crate::types::ExternalTriggerRecord>,
    I::IntoIter: Clone,
{
    let external_triggers = external_triggers.into_iter();
    derive_work_queue_with_trigger_index(
        current_work_item_id,
        latest,
        || queue_observed_trigger_deliveries(external_triggers),
        active_waits,
        continuations,
    )
}

fn derive_work_queue_with_trigger_index<K: AsRef<str>>(
    current_work_item_id: Option<&str>,
    latest: QueueLatestWork,
    trigger_index: impl FnOnce() -> Result<Vec<QueueTriggerDelivery<K>>>,
    active_waits: Vec<WaitConditionRecord>,
    continuations: Vec<crate::types::WorkItemContinuationFrame>,
) -> Result<WorkQueueReadModel> {
    let current = current_work_item_id
        .and_then(|id| latest.get(id))
        .filter(|item| item.state == WorkItemState::Open)
        .map(clone_queue_display_value)
        .transpose()?;

    let trigger_deliveries = trigger_index()?;
    let active_wait_conditions = group_queue_waits(active_waits);
    let active_continuation_suspended_ids = queue_continuation_suspended_ids(continuations)?;
    // Build the separate display records before consuming the canonical values.
    let mut queued_blocked = clone_queue_display_rows(latest.values().filter(|item| {
        item.state == WorkItemState::Open
            && Some(item.id.as_str()) != current_work_item_id
            && active_continuation_suspended_ids
                .binary_search(&item.id)
                .is_err()
            && !queue_waits_for_item(&active_wait_conditions, &item.id)
                .iter()
                .any(|condition| condition.kind == WaitConditionKind::Task)
    }))?;
    // Callers key latest by record.id; the ID tie-breaker makes this a total order.
    queued_blocked.sort_unstable_by(compare_queue_display_order);

    let mut items = Vec::new();
    if !latest.is_empty() {
        let bytes = latest
            .len()
            .checked_mul(std::mem::size_of::<WorkItemSchedulingProjection>())
            .and_then(|bytes| u64::try_from(bytes).ok())
            .ok_or_else(|| std::io::Error::other("ResourceExhausted: queue items slots"))?;
        crate::diagnostics::resource_io::charge_owned_metadata(bytes, latest.len())
            .context("queue items slots admission")?;
        items
            .try_reserve_exact(latest.len())
            .map_err(|_| std::io::Error::other("ResourceExhausted: queue items allocation"))?;
    }
    for item in latest.into_values() {
        let is_current =
            current_work_item_id == Some(item.id.as_str()) && item.state == WorkItemState::Open;
        let yielded = item.state == WorkItemState::Open
            && active_continuation_suspended_ids
                .binary_search(&item.id)
                .is_ok();
        let waits = queue_waits_for_item(&active_wait_conditions, &item.id);
        items.push(
            crate::work_item_scheduling::derive_queue_work_item_scheduling(
                item,
                is_current,
                yielded,
                waits,
                &|id| queue_trigger_delivery(&trigger_deliveries, id),
            )?,
        );
    }

    items.sort_unstable_by(compare_scheduling_projection_order);
    let current_runnable = items
        .iter()
        .find(|item| item.candidate_class == WorkItemCandidateClass::CurrentRunnable)
        .map(clone_queue_display_value)
        .transpose()?;
    let triggered_blocked = clone_queue_display_rows(
        items
            .iter()
            .filter(|item| item.candidate_class == WorkItemCandidateClass::TriggeredBlocked)
            .take(3),
    )?;
    let queued_runnable = clone_queue_display_rows(
        items
            .iter()
            .filter(|item| item.candidate_class == WorkItemCandidateClass::QueuedRunnable)
            .take(5),
    )?;
    let yielded = clone_queue_display_rows(
        items
            .iter()
            .filter(|item| item.candidate_class == WorkItemCandidateClass::Yielded)
            .take(5),
    )?;
    let waiting_for_operator = clone_queue_display_rows(
        items
            .iter()
            .filter(|item| item.candidate_class == WorkItemCandidateClass::WaitingForOperator)
            .take(3),
    )?;
    let blocked = clone_queue_display_rows(
        items
            .iter()
            .filter(|item| item.candidate_class == WorkItemCandidateClass::Blocked)
            .filter(|item| item.scheduling_state != WorkItemSchedulingState::WaitingTask)
            .take(3),
    )?;
    let completing = clone_queue_display_rows(
        items
            .iter()
            .filter(|item| item.candidate_class == WorkItemCandidateClass::Completing)
            .take(3),
    )?;
    let completed_recent = clone_queue_display_rows(
        items
            .iter()
            .filter(|item| item.candidate_class == WorkItemCandidateClass::CompletedRecent)
            .take(3),
    )?;

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
        completing,
        completed_recent,
    })
}

fn clone_queue_display_value<T: Clone + serde::Serialize>(value: &T) -> Result<T> {
    crate::diagnostics::resource_io::admit_cloned_value(value)
        .context("queue display clone admission")?;
    Ok(value.clone())
}

fn clone_queue_display_rows<'a, T: Clone + serde::Serialize + 'a>(
    rows: impl Iterator<Item = &'a T> + Clone,
) -> Result<Vec<T>> {
    let len = rows.clone().count();
    if len == 0 {
        return Ok(Vec::new());
    }
    let bytes = len
        .checked_mul(std::mem::size_of::<T>())
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or_else(|| std::io::Error::other("ResourceExhausted: queue display slots"))?;
    crate::diagnostics::resource_io::charge_owned_metadata(bytes, len)?;
    let mut values = Vec::new();
    values
        .try_reserve_exact(len)
        .map_err(|_| std::io::Error::other("ResourceExhausted: queue display allocation"))?;
    for row in rows {
        values.push(clone_queue_display_value(row)?);
    }
    Ok(values)
}

pub(crate) fn filter_context_active_waits(
    records: Vec<WaitConditionRecord>,
    work_items: Vec<WorkItemRecord>,
) -> Vec<WaitConditionRecord> {
    let work_item_is_open = work_items
        .into_iter()
        .map(|item| (item.id, item.state == WorkItemState::Open))
        .collect::<BTreeMap<_, _>>();
    records
        .into_iter()
        .filter(|record| {
            record
                .work_item_id
                .as_ref()
                .is_none_or(|id| work_item_is_open.get(id).is_some_and(|open| *open))
        })
        .collect()
}

/// Pure posture projection from one frozen State/queue/Work observation.
pub(crate) fn derive_agent_posture(
    agent: &AgentState,
    has_queued_input: bool,
    work_queue: &WorkQueueReadModel,
    active_waits: &[WaitConditionRecord],
) -> Result<AgentPostureProjection> {
    derive_agent_posture_before_work_queue(agent, has_queued_input)
        .unwrap_or_else(|| derive_agent_posture_from_work_queue(work_queue, active_waits))
        .materialize(true)
}

pub(crate) fn derive_agent_posture_from_state(
    agent: &AgentState,
    has_queued_input: bool,
) -> Result<Option<AgentPostureProjection>> {
    derive_agent_posture_before_work_queue(agent, has_queued_input)
        .map(|posture| posture.materialize(true))
        .transpose()
}

enum PostureReason<'a> {
    Text(&'static str),
    WorkItem(&'a super::work_queue::WorkItemSchedulingProjection),
    WorkItemFacts(crate::work_item_scheduling::WorkItemPostureReason<'a>),
}

impl std::fmt::Display for PostureReason<'_> {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text(text) => output.write_str(text),
            Self::WorkItem(item) => std::fmt::Display::fmt(&item.posture_reason_display(), output),
            Self::WorkItemFacts(item) => std::fmt::Display::fmt(item, output),
        }
    }
}

pub(crate) fn derive_agent_posture_from_selected_work(
    agent: &AgentState,
    has_queued_input: bool,
    selected: Option<(
        AgentSchedulingPosture,
        &WorkItemRecord,
        WorkItemSchedulingState,
        bool,
    )>,
    active_waits: &[WaitConditionRecord],
) -> Result<AgentPostureProjection> {
    let fields =
        derive_agent_posture_before_work_queue(agent, has_queued_input).unwrap_or_else(|| {
            match selected {
                Some((posture, work, scheduling_state, is_current)) => AgentPostureFields {
                    task_id: (posture == AgentSchedulingPosture::WaitingForTask)
                        .then(|| {
                            active_waits
                                .iter()
                                .find(|wait| {
                                    wait.status == WaitConditionStatus::Active
                                        && wait.kind == WaitConditionKind::Task
                                        && wait.work_item_id.as_deref() == Some(work.id.as_str())
                                })
                                .and_then(|wait| wait.subject_ref.as_deref())
                        })
                        .flatten(),
                    posture,
                    reason: PostureReason::WorkItemFacts(
                        crate::work_item_scheduling::WorkItemPostureReason {
                            id: &work.id,
                            is_current,
                            scheduling_state,
                        },
                    ),
                    work_item_id: Some(&work.id),
                    run_id: None,
                },
                None => AgentPostureFields {
                    posture: AgentSchedulingPosture::Idle,
                    reason: PostureReason::Text(
                        "no queued input, active turn, runnable work, or active waits",
                    ),
                    work_item_id: None,
                    task_id: None,
                    run_id: None,
                },
            }
        });
    fields.materialize(true)
}

struct AgentPostureFields<'a> {
    posture: AgentSchedulingPosture,
    reason: PostureReason<'a>,
    work_item_id: Option<&'a str>,
    task_id: Option<&'a str>,
    run_id: Option<&'a str>,
}

impl AgentPostureFields<'_> {
    fn into_owned(self) -> AgentPostureProjection {
        AgentPostureProjection {
            posture: self.posture,
            reason: self.reason.to_string(),
            work_item_id: self.work_item_id.map(str::to_owned),
            task_id: self.task_id.map(str::to_owned),
            run_id: self.run_id.map(str::to_owned),
        }
    }

    fn materialize(self, admit_material: bool) -> Result<AgentPostureProjection> {
        if !admit_material {
            return Ok(self.into_owned());
        }
        use std::fmt::Write;
        struct TextLength(usize);
        impl Write for TextLength {
            fn write_str(&mut self, text: &str) -> std::fmt::Result {
                self.0 = self.0.checked_add(text.len()).ok_or(std::fmt::Error)?;
                Ok(())
            }
        }
        // Count the same formatter on the stack, before allocating any field.
        let mut reason_len = TextLength(0);
        write!(&mut reason_len, "{}", self.reason)?;
        let ids = [self.work_item_id, self.task_id, self.run_id];
        let records = 1 + ids.iter().flatten().count();
        let bytes = ids.iter().flatten().fold(
            reason_len
                .0
                .saturating_add(records.saturating_mul(std::mem::size_of::<String>())),
            |bytes, id| bytes.saturating_add(id.len()),
        );
        crate::diagnostics::resource_io::charge_owned_metadata(bytes as u64, records)?;
        let mut reason = String::with_capacity(reason_len.0);
        write!(&mut reason, "{}", self.reason)?;
        Ok(AgentPostureProjection {
            posture: self.posture,
            reason,
            work_item_id: self.work_item_id.map(str::to_owned),
            task_id: self.task_id.map(str::to_owned),
            run_id: self.run_id.map(str::to_owned),
        })
    }
}

fn derive_agent_posture_before_work_queue(
    agent: &AgentState,
    has_queued_input: bool,
) -> Option<AgentPostureFields<'_>> {
    if matches!(agent.status, AgentStatus::Stopped) {
        return Some(AgentPostureFields {
            posture: AgentSchedulingPosture::Stopped,
            reason: PostureReason::Text("agent lifecycle is stopped"),
            work_item_id: None,
            task_id: None,
            run_id: None,
        });
    }
    if let Some(run_id) = agent.current_run_id.as_deref() {
        return Some(AgentPostureFields {
            posture: AgentSchedulingPosture::ActiveTurn,
            reason: PostureReason::Text("agent has an active turn"),
            work_item_id: agent.current_turn_work_item_id.as_deref(),
            task_id: None,
            run_id: Some(run_id),
        });
    }
    has_queued_input.then(|| AgentPostureFields {
        posture: AgentSchedulingPosture::HasQueuedInput,
        reason: PostureReason::Text("agent has queued input"),
        work_item_id: None,
        task_id: None,
        run_id: None,
    })
}

fn derive_agent_posture_from_work_queue<'a>(
    work_queue: &'a WorkQueueReadModel,
    active_waits: &'a [WaitConditionRecord],
) -> AgentPostureFields<'a> {
    if let Some(item) = work_queue
        .current_runnable
        .as_ref()
        .or_else(|| work_queue.queued_runnable.first())
    {
        return AgentPostureFields {
            posture: AgentSchedulingPosture::HasRunnableWork,
            reason: PostureReason::WorkItem(item),
            work_item_id: Some(item.work_item.id.as_str()),
            task_id: None,
            run_id: None,
        };
    }

    if let Some(item) = work_queue.items.iter().find(|item| {
        item.is_current && item.scheduling_state == WorkItemSchedulingState::WaitingTask
    }) {
        let task_id = active_waits
            .iter()
            .find(|condition| {
                condition.status == WaitConditionStatus::Active
                    && condition.kind == WaitConditionKind::Task
                    && condition.work_item_id.as_deref() == Some(item.work_item.id.as_str())
            })
            .and_then(|condition| condition.subject_ref.as_deref());
        return AgentPostureFields {
            posture: AgentSchedulingPosture::WaitingForTask,
            reason: PostureReason::WorkItem(item),
            work_item_id: Some(item.work_item.id.as_str()),
            task_id,
            run_id: None,
        };
    }

    if let Some(item) = work_queue.items.iter().find(|item| {
        item.is_current && item.scheduling_state == WorkItemSchedulingState::WaitingExternal
    }) {
        return AgentPostureFields {
            posture: AgentSchedulingPosture::WaitingForExternal,
            reason: PostureReason::WorkItem(item),
            work_item_id: Some(item.work_item.id.as_str()),
            task_id: None,
            run_id: None,
        };
    }

    if let Some(item) = work_queue
        .waiting_for_operator
        .iter()
        .find(|item| item.is_current)
    {
        return AgentPostureFields {
            posture: AgentSchedulingPosture::WaitingForOperator,
            reason: PostureReason::WorkItem(item),
            work_item_id: Some(item.work_item.id.as_str()),
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
        return AgentPostureFields {
            posture: AgentSchedulingPosture::Blocked,
            reason: PostureReason::WorkItem(item),
            work_item_id: Some(item.work_item.id.as_str()),
            task_id: None,
            run_id: None,
        };
    }

    AgentPostureFields {
        posture: AgentSchedulingPosture::Idle,
        reason: PostureReason::Text("no queued input, active turn, runnable work, or active waits"),
        work_item_id: None,
        task_id: None,
        run_id: None,
    }
}
