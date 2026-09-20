use super::*;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SchedulerProjection {
    /// Captured once per scheduling decision and included in derived equality.
    pub(super) now: DateTime<Utc>,
    pub(super) agent_id: String,
    pub status: AgentStatus,
    pub queue_len: usize,
    pub has_interrupted_replay: bool,
    pub active_run_id: Option<String>,
    pub active_tasks: Vec<TaskRecord>,
    pub has_blocking_active_tasks: bool,
    pub current_work_item: Option<WorkItemRecord>,
    pub current_work_item_scheduling_state: Option<WorkItemSchedulingState>,
    pub queued_runnable_work_items: Vec<WorkItemRecord>,
    pub queued_work_items: usize,
    pub pending_wake_hint: bool,
    pub active_waiting_intents: usize,
    pub active_work_item_waiting_intents: usize,
    pub active_agent_waiting_intents: usize,
    pub active_timers: usize,
    pub waiting_work_item: Option<WorkItemRecord>,
    pub waiting_work_item_scheduling_state: Option<WorkItemSchedulingState>,
    pub last_turn_terminal: Option<TurnTerminalKind>,
    pub turn_in_progress: bool,
    pub runtime_error: bool,
    pub(super) activation_waits: Vec<WaitConditionRecord>,
    pub(super) canonical_work_states: Option<HashMap<String, CanonicalWorkExecutionState>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CanonicalWorkExecutionState {
    Runnable {
        source_revision: u64,
        generation: u64,
    },
    Waiting {
        wait_id: String,
    },
    Other,
}

pub(crate) struct SchedulerAgentSnapshot {
    id: String,
    status: AgentStatus,
    active_run_id: Option<String>,
    pending_wake_hint: bool,
    last_turn_terminal: Option<TurnTerminalKind>,
}

impl SchedulerAgentSnapshot {
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn from_state(state: &AgentState) -> Self {
        Self {
            id: state.id.clone(),
            status: state.status.clone(),
            active_run_id: state.current_run_id.clone(),
            pending_wake_hint: state.pending_wake_hint.is_some(),
            last_turn_terminal: state
                .last_turn_terminal
                .as_ref()
                .map(|terminal| terminal.kind.clone()),
        }
    }
}

impl SchedulerProjection {
    #[cfg(test)]
    pub(crate) fn enable_canonical_authority_for_test(&mut self) {
        self.canonical_work_states.get_or_insert_with(HashMap::new);
    }

    pub(crate) fn from_state(storage: &AppStorage, state: &AgentState) -> Result<Self> {
        Self::from_state_with_queue_len(storage, state, state.pending)
    }

    pub(crate) fn from_state_with_queue_len(
        storage: &AppStorage,
        state: &AgentState,
        queue_len: usize,
    ) -> Result<Self> {
        Self::from_state_with_queue_len_at(storage, state, queue_len, Utc::now())
    }

    pub(crate) fn from_state_with_queue_len_at(
        storage: &AppStorage,
        state: &AgentState,
        queue_len: usize,
        now: DateTime<Utc>,
    ) -> Result<Self> {
        let snapshot = SchedulerAgentSnapshot::from_state(state);
        Self::from_snapshot_with_queue_len_at(storage, &snapshot, queue_len, now)
    }

    pub(crate) fn from_snapshot_with_queue_len_at(
        storage: &AppStorage,
        snapshot: &SchedulerAgentSnapshot,
        queue_len: usize,
        now: DateTime<Utc>,
    ) -> Result<Self> {
        let work_queue = storage.work_queue_prompt_projection()?;
        Self::from_snapshot_with_queue_len_and_work_queue_at(
            storage, snapshot, queue_len, work_queue, now,
        )
    }

    pub(crate) fn from_state_with_work_queue_at(
        storage: &AppStorage,
        state: &AgentState,
        work_queue: WorkQueueReadModel,
        now: DateTime<Utc>,
    ) -> Result<Self> {
        let snapshot = SchedulerAgentSnapshot::from_state(state);
        Self::from_snapshot_with_queue_len_and_work_queue_at(
            storage,
            &snapshot,
            state.pending,
            work_queue,
            now,
        )
    }

    pub(crate) fn from_snapshot_with_queue_len_and_work_queue_at(
        storage: &AppStorage,
        snapshot: &SchedulerAgentSnapshot,
        queue_len: usize,
        work_queue: WorkQueueReadModel,
        now: DateTime<Utc>,
    ) -> Result<Self> {
        let active_tasks =
            storage.latest_active_task_records_for_agent(&snapshot.id, usize::MAX)?;
        let has_interrupted_replay = storage
            .runtime_db()?
            .map(|runtime_db| {
                runtime_db
                    .queue_entries()
                    .has_interrupted_for_agent(&snapshot.id)
            })
            .transpose()?
            .unwrap_or(false);
        let has_blocking_active_tasks = active_tasks.iter().any(TaskRecord::is_blocking);
        let queued_runnable_work_items = work_queue
            .queued_runnable
            .iter()
            .map(|item| item.work_item.clone())
            .collect::<Vec<_>>();
        let current_work_item_scheduling_state = work_queue
            .items
            .iter()
            .find(|item| item.is_current)
            .map(|item| item.scheduling_state);
        let waiting_work_item_projection = work_queue.items.iter().find(|item| {
            (item.is_current || item.has_active_waits || item.has_active_task_waits)
                && matches!(
                    item.scheduling_state,
                    WorkItemSchedulingState::WaitingOperator
                        | WorkItemSchedulingState::WaitingTask
                        | WorkItemSchedulingState::WaitingExternal
                        | WorkItemSchedulingState::WaitingTimer
                        | WorkItemSchedulingState::WaitingSystem
                )
        });
        let waiting_work_item = waiting_work_item_projection.map(|item| item.work_item.clone());
        let waiting_work_item_scheduling_state =
            waiting_work_item_projection.map(|item| item.scheduling_state);
        let active_wait_conditions = storage.active_wait_conditions_for_agent(&snapshot.id)?;
        let activation_waits = storage
            .latest_wait_conditions_for_agent(&snapshot.id)?
            .into_iter()
            .filter(|condition| {
                condition.status == WaitConditionStatus::Active
                    || condition.status == WaitConditionStatus::Triggered
                    || (condition.status == WaitConditionStatus::Resolved
                        && condition.kind == WaitConditionKind::Task)
            })
            .collect();
        let execution_snapshot = storage
            .runtime_db()?
            .map(|runtime_db| {
                runtime_db
                    .transitions()
                    .load_execution_protocol_state_if_initialized(&snapshot.id)
            })
            .transpose()?
            .flatten();
        let canonical_work_states = Some(
            execution_snapshot
                .as_ref()
                .map(|snapshot| {
                    snapshot
                        .work_items
                        .iter()
                        .map(|(work_item_id, record)| {
                            let state = match &record.state {
                                WorkItemExecutionState::Runnable { generation, .. } => {
                                    CanonicalWorkExecutionState::Runnable {
                                        source_revision: record.source_revision,
                                        generation: *generation,
                                    }
                                }
                                WorkItemExecutionState::Waiting { wait, .. } => {
                                    CanonicalWorkExecutionState::Waiting {
                                        wait_id: wait.wait_id.clone(),
                                    }
                                }
                                _ => CanonicalWorkExecutionState::Other,
                            };
                            (work_item_id.clone(), state)
                        })
                        .collect()
                })
                .unwrap_or_default(),
        );
        let active_work_item_waiting_intents = active_wait_conditions
            .iter()
            .filter(|condition| condition.work_item_id.is_some())
            .count();
        let active_agent_waiting_intents = active_wait_conditions
            .iter()
            .filter(|condition| condition.work_item_id.is_none())
            .filter(|condition| {
                matches!(
                    condition.kind,
                    WaitConditionKind::External
                        | WaitConditionKind::Timer
                        | WaitConditionKind::System
                        | WaitConditionKind::Operator
                )
            })
            .count();
        let active_timers = storage
            .latest_timer_records()?
            .into_iter()
            .filter(|timer| timer.agent_id == snapshot.id && timer.status == TimerStatus::Active)
            .count();
        Ok(Self {
            now,
            agent_id: snapshot.id.clone(),
            status: snapshot.status.clone(),
            queue_len,
            has_interrupted_replay,
            active_run_id: snapshot.active_run_id.clone(),
            active_tasks,
            has_blocking_active_tasks,
            current_work_item: work_queue.current,
            current_work_item_scheduling_state,
            queued_work_items: queued_runnable_work_items.len(),
            queued_runnable_work_items,
            pending_wake_hint: snapshot.pending_wake_hint,
            active_waiting_intents: active_wait_conditions.len(),
            active_work_item_waiting_intents,
            active_agent_waiting_intents,
            active_timers,
            waiting_work_item,
            waiting_work_item_scheduling_state,
            last_turn_terminal: snapshot.last_turn_terminal.clone(),
            turn_in_progress: snapshot.active_run_id.is_some(),
            runtime_error: runtime_error_active(
                &storage.read_recent_events(64)?,
                &storage.read_recent_briefs(64)?,
            ),
            activation_waits,
            canonical_work_states,
        })
    }

    pub(crate) fn work_reactivation_signal(&self) -> Option<WorkReactivationSignal> {
        self.work_reactivation_work_item()
            .map(|(item, reactivation_mode)| WorkReactivationSignal {
                work_item_id: item.id.clone(),
                state: item.state.clone(),
                reactivation_mode,
            })
    }

    pub(crate) fn work_reactivation_work_item(
        &self,
    ) -> Option<(&WorkItemRecord, WorkReactivationMode)> {
        let selection = select_autonomous_continuation(self)?;
        resolve_autonomous_continuation_work_item(self, &selection)
    }

    pub(super) fn execution_authorizes_autonomous(&self, work_item: &WorkItemRecord) -> bool {
        // None means the legacy engine opted out of canonical execution authority.
        self.canonical_work_states.as_ref().is_none_or(|states| {
            matches!(
                states.get(&work_item.id),
                Some(CanonicalWorkExecutionState::Runnable {
                    source_revision,
                    ..
                })
                    if *source_revision == work_item.revision
            )
        })
    }

    pub(super) fn autonomous_execution_generation(
        &self,
        work_item: &WorkItemRecord,
    ) -> Option<u64> {
        match self
            .canonical_work_states
            .as_ref()
            .and_then(|states| states.get(&work_item.id))
        {
            Some(CanonicalWorkExecutionState::Runnable {
                source_revision,
                generation,
            }) if *source_revision == work_item.revision => Some(*generation),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn set_autonomous_execution_generation_for_test(
        &mut self,
        work_item_id: &str,
        generation: u64,
    ) -> bool {
        let Some(CanonicalWorkExecutionState::Runnable {
            generation: current,
            ..
        }) = self
            .canonical_work_states
            .as_mut()
            .and_then(|states| states.get_mut(work_item_id))
        else {
            return false;
        };
        *current = generation;
        true
    }

    pub(crate) fn current_work_item_waits_for_operator(&self) -> bool {
        self.current_work_item_scheduling_state == Some(WorkItemSchedulingState::WaitingOperator)
    }
}
