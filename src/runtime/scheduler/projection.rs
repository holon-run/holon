use super::*;
use crate::runtime::resident_work::StateWorkRecord;
use std::borrow::Cow;

#[derive(Clone, Copy)]
enum SchedulerReadPurpose {
    Dispatch,
    Observer,
}

/// Preserve State loans instead of deep-copying bodies already held by the response.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(untagged)]
pub(crate) enum SchedulerRunnableWork {
    Owned(Vec<WorkItemRecord>),
    Shared(Vec<StateWorkRecord>),
}

impl SchedulerRunnableWork {
    pub(crate) fn iter(&self) -> impl Iterator<Item = &WorkItemRecord> {
        let (owned, shared): (&[WorkItemRecord], &[StateWorkRecord]) = match self {
            Self::Owned(items) => (items, &[]),
            Self::Shared(items) => (&[], items),
        };
        owned.iter().chain(shared.iter().map(|item| &**item))
    }

    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Owned(items) => items.len(),
            Self::Shared(items) => items.len(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&WorkItemRecord) -> bool) {
        match self {
            Self::Owned(items) => items.retain(keep),
            Self::Shared(items) => items.retain(|item| keep(item)),
        }
    }
}

impl From<Vec<WorkItemRecord>> for SchedulerRunnableWork {
    fn from(items: Vec<WorkItemRecord>) -> Self {
        Self::Owned(items)
    }
}

impl FromIterator<WorkItemRecord> for SchedulerRunnableWork {
    fn from_iter<T: IntoIterator<Item = WorkItemRecord>>(items: T) -> Self {
        Self::Owned(items.into_iter().collect())
    }
}

impl PartialEq for SchedulerRunnableWork {
    fn eq(&self, other: &Self) -> bool {
        self.iter().eq(other.iter())
    }
}

/// Exact Work bodies used by a scheduler decision, not a display inventory.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub(crate) struct SchedulerWorkSelection {
    pub current: Option<WorkItemRecord>,
    pub current_scheduling_state: Option<WorkItemSchedulingState>,
    pub queued_runnable: SchedulerRunnableWork,
    pub waiting: Option<WorkItemRecord>,
    pub waiting_scheduling_state: Option<WorkItemSchedulingState>,
}

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
    pub queued_runnable_work_items: SchedulerRunnableWork,
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

/// Explicit adapter values not yet resident in D4; no storage handle escapes.
pub(crate) struct SchedulerObservations<'a> {
    pub active_tasks: Vec<TaskRecord>,
    pub has_interrupted_replay: bool,
    /// Only counts are retained; the caller keeps the original wait bodies.
    pub active_wait_conditions: &'a [WaitConditionRecord],
    pub activation_waits: Vec<WaitConditionRecord>,
    pub execution_snapshot: Option<crate::domain::execution_protocol::ExecutionProtocolState>,
    pub active_timers: usize,
    pub runtime_error: bool,
}

impl SchedulerProjection {
    pub(crate) fn with_validated_closure_protocol(
        mut self,
        protocol: Option<
            crate::runtime_db::transitions::execution_protocol_repository::ValidatedClosureProtocol,
        >,
    ) -> Result<Self> {
        self.canonical_work_states = Some(
            protocol
                .map(|p| {
                    Ok::<_, anyhow::Error>(
                        p.into_work_items(&self.agent_id)?
                            .into_iter()
                            .map(|(id, record)| {
                                let state = match record.state {
                                    WorkItemExecutionState::Runnable { generation, .. } => {
                                        CanonicalWorkExecutionState::Runnable {
                                            source_revision: record.source_revision,
                                            generation,
                                        }
                                    }
                                    WorkItemExecutionState::Waiting { wait, .. } => {
                                        CanonicalWorkExecutionState::Waiting {
                                            wait_id: wait.wait_id,
                                        }
                                    }
                                    _ => CanonicalWorkExecutionState::Other,
                                };
                                (id, state)
                            })
                            .collect(),
                    )
                })
                .transpose()?
                .unwrap_or_default(),
        );
        Ok(self)
    }

    #[cfg(test)]
    pub(crate) fn activation_waits_for_test(&self) -> &[WaitConditionRecord] {
        &self.activation_waits
    }

    #[cfg(test)]
    pub(crate) fn enable_canonical_authority_for_test(&mut self) {
        self.canonical_work_states.get_or_insert_with(HashMap::new);
    }

    #[cfg(test)]
    pub(crate) fn set_canonical_waiting_state_for_test(
        &mut self,
        work_item_id: &str,
        wait_id: &str,
    ) {
        self.canonical_work_states
            .get_or_insert_with(HashMap::new)
            .insert(
                work_item_id.to_string(),
                CanonicalWorkExecutionState::Waiting {
                    wait_id: wait_id.to_string(),
                },
            );
    }

    #[cfg(test)]
    pub(crate) fn from_state(storage: &AppStorage, state: &AgentState) -> Result<Self> {
        Self::from_state_with_queue_len(storage, state, state.pending)
    }

    #[cfg(test)]
    pub(crate) fn from_state_with_material(
        storage: &AppStorage,
        state: &AgentState,
        material: &crate::diagnostics::resource_io::ResourceReadMaterial,
    ) -> Result<Self> {
        material.read(|| {
            crate::diagnostics::resource_io::admit_cloned_value(&state.id)?;
            crate::diagnostics::resource_io::admit_cloned_value(&state.current_run_id)?;
            let snapshot = SchedulerAgentSnapshot::from_state(state);
            let queue = storage.work_queue_prompt_projection()?;
            Self::from_snapshot_with_timer_observation(
                storage,
                snapshot,
                state.pending,
                Cow::Owned(queue),
                Utc::now(),
                None,
                Some(material),
            )
        })
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
        Self::from_snapshot_with_queue_len_at(storage, snapshot, queue_len, now)
    }

    pub(crate) fn from_snapshot_with_queue_len_at(
        storage: &AppStorage,
        snapshot: SchedulerAgentSnapshot,
        queue_len: usize,
        now: DateTime<Utc>,
    ) -> Result<Self> {
        let work_queue = storage.work_queue_prompt_projection()?;
        Self::from_snapshot_with_queue_len_and_work_queue_at(
            storage, snapshot, queue_len, work_queue, now,
        )
    }

    pub(crate) fn from_snapshot_with_queue_len_and_work_queue_at(
        storage: &AppStorage,
        snapshot: SchedulerAgentSnapshot,
        queue_len: usize,
        work_queue: WorkQueueReadModel,
        now: DateTime<Utc>,
    ) -> Result<Self> {
        Self::from_snapshot_with_timer_observation(
            storage,
            snapshot,
            queue_len,
            Cow::Owned(work_queue),
            now,
            None,
            None,
        )
    }
    /// Cold named query keeps the original remaining adapters but supplies its
    /// qualified Scheduling count, never another Timer body read.
    #[cfg(test)]
    pub(crate) fn from_state_with_scheduling_observation(
        storage: &AppStorage,
        state: &AgentState,
        work_queue: WorkQueueReadModel,
        active_timers: usize,
        work_material: &crate::diagnostics::resource_io::ResourceReadMaterial,
    ) -> Result<Self> {
        let _entry = work_material.enter();
        crate::diagnostics::resource_io::admit_cloned_value(&(&state.id, &state.current_run_id))?;
        Self::from_snapshot_with_timer_observation(
            storage,
            SchedulerAgentSnapshot::from_state(state),
            state.pending,
            Cow::Owned(work_queue),
            Utc::now(),
            Some(active_timers),
            Some(work_material),
        )
    }

    /// Cold Host responses retain the queue; borrow it without cloning all
    /// display groups just to derive the scheduler's retained fields.
    #[cfg(test)]
    pub(crate) fn from_state_with_borrowed_scheduling_observation(
        storage: &AppStorage,
        state: &AgentState,
        work_queue: &WorkQueueReadModel,
        active_timers: usize,
        work_material: &crate::diagnostics::resource_io::ResourceReadMaterial,
    ) -> Result<Self> {
        let _entry = work_material.enter();
        crate::diagnostics::resource_io::admit_cloned_value(&(&state.id, &state.current_run_id))?;
        Self::from_snapshot_with_timer_observation(
            storage,
            SchedulerAgentSnapshot::from_state(state),
            state.pending,
            Cow::Borrowed(work_queue),
            Utc::now(),
            Some(active_timers),
            Some(work_material),
        )
    }

    /// State retains shared runnable bodies; only current/waiting copies are owned.
    pub(crate) fn from_state_with_shared_scheduling_observation(
        storage: &AppStorage,
        state: &AgentState,
        work_queue: &crate::runtime::resident_work::StateWorkQueue,
        active_timers: usize,
        work_material: &crate::diagnostics::resource_io::ResourceReadMaterial,
    ) -> Result<Self> {
        use crate::diagnostics::resource_io::{admit_cloned_value, charge_owned_metadata};
        let _entry = work_material.enter();
        admit_cloned_value(&(&state.id, &state.current_run_id))?;
        let snapshot = SchedulerAgentSnapshot::from_state(state);
        let now = Utc::now();
        Self::with_loaded_observations(
            storage,
            snapshot,
            Some(active_timers),
            Some(work_material),
            SchedulerReadPurpose::Observer,
            |snapshot, observations| {
                let count = work_queue.queued_runnable.len();
                let bytes = count
                    .checked_mul(std::mem::size_of::<StateWorkRecord>())
                    .context("scheduler retained Work slots overflow")?;
                charge_owned_metadata(u64::try_from(bytes)?, count)?;
                if let Some(current) = &work_queue.current {
                    admit_cloned_value(&**current)?;
                }
                let waiting_index = Self::waiting_work_item_index(work_queue);
                if let Some(index) = waiting_index {
                    admit_cloned_value(&*work_queue.items[index].work_item)?;
                }
                work_material.finish()?;
                Self::admit_canonical_slots(
                    observations.execution_snapshot.as_ref(),
                    work_material,
                )?;
                let current_scheduling_state = work_queue
                    .items
                    .iter()
                    .find(|item| item.is_current)
                    .map(|item| item.scheduling_state);
                let waiting_scheduling_state =
                    waiting_index.map(|index| work_queue.items[index].scheduling_state);
                let mut queued_runnable = Vec::with_capacity(count);
                for item in &work_queue.queued_runnable {
                    queued_runnable.push(item.work_item.clone());
                }
                Ok(Self::from_selected_observations(
                    snapshot,
                    state.pending,
                    SchedulerWorkSelection {
                        current: work_queue.current.as_ref().map(|item| (**item).clone()),
                        current_scheduling_state,
                        queued_runnable: SchedulerRunnableWork::Shared(queued_runnable),
                        waiting: waiting_index
                            .map(|index| (*work_queue.items[index].work_item).clone()),
                        waiting_scheduling_state,
                    },
                    observations,
                    now,
                ))
            },
        )
    }

    fn from_snapshot_with_timer_observation(
        storage: &AppStorage,
        snapshot: SchedulerAgentSnapshot,
        queue_len: usize,
        work_queue: Cow<'_, WorkQueueReadModel>,
        now: DateTime<Utc>,
        observed_timers: Option<usize>,
        work_material: Option<&crate::diagnostics::resource_io::ResourceReadMaterial>,
    ) -> Result<Self> {
        Self::with_loaded_observations(
            storage,
            snapshot,
            observed_timers,
            work_material,
            SchedulerReadPurpose::Dispatch,
            |snapshot, observations| {
                if let Some(material) = work_material {
                    Self::admit_retained_work(&work_queue, material)?;
                    Self::admit_canonical_slots(
                        observations.execution_snapshot.as_ref(),
                        material,
                    )?;
                }
                Ok(match work_queue {
                    Cow::Owned(queue) => {
                        Self::from_observations(snapshot, queue_len, queue, observations, now)
                    }
                    Cow::Borrowed(queue) => Self::from_borrowed_observations(
                        snapshot,
                        queue_len,
                        queue,
                        observations,
                        now,
                    ),
                })
            },
        )
    }

    fn with_loaded_observations(
        storage: &AppStorage,
        snapshot: SchedulerAgentSnapshot,
        observed_timers: Option<usize>,
        work_material: Option<&crate::diagnostics::resource_io::ResourceReadMaterial>,
        purpose: SchedulerReadPurpose,
        build: impl FnOnce(SchedulerAgentSnapshot, SchedulerObservations<'_>) -> Result<Self>,
    ) -> Result<Self> {
        let include_dispatch_history = matches!(purpose, SchedulerReadPurpose::Dispatch);
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
        let active_wait_conditions = storage.active_wait_conditions_for_agent(&snapshot.id)?;
        // Observer State/closure does not route incoming Messages. Historical
        // activation waits belong only to the dispatch projection.
        let mut activation_waits = if !include_dispatch_history {
            Vec::new()
        } else {
            match work_material {
                Some(material) => storage
                    .latest_wait_conditions_for_agent_with_material(&snapshot.id, material)?,
                None => storage.latest_wait_conditions_for_agent(&snapshot.id)?,
            }
        };
        activation_waits.retain(|condition| {
            condition.status == WaitConditionStatus::Active
                || condition.status == WaitConditionStatus::Triggered
                || (condition.status == WaitConditionStatus::Resolved
                    && condition.kind == WaitConditionKind::Task)
        });
        let execution_snapshot = if !include_dispatch_history {
            None
        } else {
            storage
                .runtime_db()?
                .map(|runtime_db| {
                    runtime_db
                        .transitions()
                        .load_execution_protocol_state_if_initialized(&snapshot.id)
                })
                .transpose()?
                .flatten()
        };
        let active_timers = match observed_timers {
            Some(count) => count,
            None => storage
                .latest_timer_records()?
                .into_iter()
                .filter(|timer| {
                    timer.agent_id == snapshot.id && timer.status == TimerStatus::Active
                })
                .count(),
        };
        let latest_error = storage
            .runtime_db()?
            .map(|runtime_db| {
                let connection = runtime_db.connection()?;
                crate::runtime_db::repositories::scheduler_latest_runtime_error_on(
                    &connection,
                    &snapshot.id,
                )
            })
            .transpose()?
            .flatten();
        // Briefs can clear an observed error, but cannot create one.
        let runtime_error = match latest_error {
            Some(error_at) => {
                runtime_error_active_at(Some(error_at), &storage.read_recent_briefs(64)?)
            }
            None => false,
        };
        let observations = SchedulerObservations {
            active_tasks,
            has_interrupted_replay,
            active_wait_conditions: &active_wait_conditions,
            activation_waits,
            execution_snapshot,
            active_timers,
            runtime_error,
        };
        let agent_id = &snapshot.id;
        let observer_work = if !include_dispatch_history {
            storage
                .runtime_db()?
                .map(|db| db.transitions().observer_live_work_execution(agent_id))
                .transpose()?
        } else {
            None
        };
        let mut projection = build(snapshot, observations)?;
        if let Some(work) = observer_work {
            crate::diagnostics::resource_io::charge_owned_metadata(
                u64::try_from(
                    work.len()
                        .checked_mul(std::mem::size_of::<(String, CanonicalWorkExecutionState)>())
                        .context("observer Work authority slots overflow")?,
                )?,
                work.len(),
            )?;
            projection.canonical_work_states = Some(
                work.into_iter()
                    .map(|(id, record)| {
                        let state = match record.state {
                            WorkItemExecutionState::Runnable { generation, .. } => {
                                CanonicalWorkExecutionState::Runnable {
                                    source_revision: record.source_revision,
                                    generation,
                                }
                            }
                            WorkItemExecutionState::Waiting { wait, .. } => {
                                CanonicalWorkExecutionState::Waiting {
                                    wait_id: wait.wait_id,
                                }
                            }
                            _ => CanonicalWorkExecutionState::Other,
                        };
                        (id, state)
                    })
                    .collect(),
            );
        }
        Ok(projection)
    }

    /// Pure scheduler derivation. Loaded callers supply one owner capture and
    /// explicitly observed D5/D6/D7 adapters; this opens no storage connection.
    pub(crate) fn from_observations(
        snapshot: SchedulerAgentSnapshot,
        queue_len: usize,
        work_queue: WorkQueueReadModel,
        observations: SchedulerObservations<'_>,
        now: DateTime<Utc>,
    ) -> Self {
        Self::from_queue_observations(
            snapshot,
            queue_len,
            Cow::Owned(work_queue),
            observations,
            now,
        )
    }

    /// Loaded callers retain their queue for later consumers; only scheduler
    /// fields are copied, not every nested display group.
    pub(crate) fn from_borrowed_observations(
        snapshot: SchedulerAgentSnapshot,
        queue_len: usize,
        work_queue: &WorkQueueReadModel,
        observations: SchedulerObservations<'_>,
        now: DateTime<Utc>,
    ) -> Self {
        Self::from_queue_observations(
            snapshot,
            queue_len,
            Cow::Borrowed(work_queue),
            observations,
            now,
        )
    }

    fn waiting_work_item_index<W>(queue: &WorkQueueReadModel<W>) -> Option<usize> {
        queue.items.iter().position(|item| {
            (item.is_current || item.has_active_waits || item.has_active_task_waits)
                && matches!(
                    item.scheduling_state,
                    WorkItemSchedulingState::WaitingOperator
                        | WorkItemSchedulingState::WaitingTask
                        | WorkItemSchedulingState::WaitingExternal
                        | WorkItemSchedulingState::WaitingTimer
                        | WorkItemSchedulingState::WaitingSystem
                )
        })
    }

    /// Sources, retained copies and output slots use the caller's same phase.
    pub(crate) fn admit_retained_work(
        queue: &Cow<'_, WorkQueueReadModel>,
        material: &crate::diagnostics::resource_io::ResourceReadMaterial,
    ) -> Result<()> {
        use crate::diagnostics::resource_io::{admit_cloned_value, charge_owned_metadata};
        let _entry = material.enter();
        let count = queue.queued_runnable.len();
        let bytes = count
            .checked_mul(std::mem::size_of::<WorkItemRecord>())
            .context("scheduler retained Work slots overflow")?;
        charge_owned_metadata(u64::try_from(bytes)?, count)?;
        if matches!(queue, Cow::Borrowed(_)) {
            if let Some(current) = &queue.current {
                admit_cloned_value(current)?;
            }
            for item in &queue.queued_runnable {
                admit_cloned_value(&item.work_item)?;
            }
            if let Some(index) = Self::waiting_work_item_index(queue) {
                admit_cloned_value(&queue.items[index].work_item)?;
            }
        }
        material.finish()?;
        Ok(())
    }

    /// Keys and wait IDs move from the source; only output table slots are new.
    pub(crate) fn admit_canonical_slots(
        execution: Option<&crate::domain::execution_protocol::ExecutionProtocolState>,
        material: &crate::diagnostics::resource_io::ResourceReadMaterial,
    ) -> Result<()> {
        let count = execution.map_or(0, |state| state.work_items.len());
        let bytes = count
            .checked_mul(std::mem::size_of::<(String, CanonicalWorkExecutionState)>())
            .context("scheduler canonical slots overflow")?;
        let _entry = material.enter();
        // The existing 8x metadata allowance conservatively covers table slack.
        crate::diagnostics::resource_io::charge_owned_metadata(u64::try_from(bytes)?, count)?;
        material.finish()?;
        Ok(())
    }

    fn from_queue_observations(
        snapshot: SchedulerAgentSnapshot,
        queue_len: usize,
        work_queue: Cow<'_, WorkQueueReadModel>,
        observations: SchedulerObservations<'_>,
        now: DateTime<Utc>,
    ) -> Self {
        let current_scheduling_state = work_queue
            .items
            .iter()
            .find(|item| item.is_current)
            .map(|item| item.scheduling_state);
        let waiting_index = Self::waiting_work_item_index(&work_queue);
        let waiting_scheduling_state =
            waiting_index.map(|index| work_queue.items[index].scheduling_state);
        let (current, queued_runnable, waiting) = match work_queue {
            Cow::Owned(mut queue) => {
                let waiting = waiting_index.map(|index| queue.items.swap_remove(index).work_item);
                let mut runnable = Vec::with_capacity(queue.queued_runnable.len());
                for item in queue.queued_runnable {
                    runnable.push(item.work_item);
                }
                (queue.current, runnable, waiting)
            }
            Cow::Borrowed(queue) => {
                let mut runnable = Vec::with_capacity(queue.queued_runnable.len());
                for item in &queue.queued_runnable {
                    runnable.push(item.work_item.clone());
                }
                (
                    queue.current.clone(),
                    runnable,
                    waiting_index.map(|index| queue.items[index].work_item.clone()),
                )
            }
        };
        Self::from_selected_observations(
            snapshot,
            queue_len,
            SchedulerWorkSelection {
                current,
                current_scheduling_state,
                queued_runnable: queued_runnable.into(),
                waiting,
                waiting_scheduling_state,
            },
            observations,
            now,
        )
    }

    pub(crate) fn from_selected_observations(
        snapshot: SchedulerAgentSnapshot,
        queue_len: usize,
        work: SchedulerWorkSelection,
        observations: SchedulerObservations<'_>,
        now: DateTime<Utc>,
    ) -> Self {
        let SchedulerObservations {
            active_tasks,
            has_interrupted_replay,
            active_wait_conditions,
            activation_waits,
            execution_snapshot,
            active_timers,
            runtime_error,
        } = observations;
        let has_blocking_active_tasks = active_tasks.iter().any(TaskRecord::is_blocking);
        let canonical_work_states = Some(
            execution_snapshot
                .map(|snapshot| {
                    let mut states = HashMap::with_capacity(snapshot.work_items.len());
                    for (work_item_id, record) in snapshot.work_items {
                        let state = match record.state {
                            WorkItemExecutionState::Runnable { generation, .. } => {
                                CanonicalWorkExecutionState::Runnable {
                                    source_revision: record.source_revision,
                                    generation,
                                }
                            }
                            WorkItemExecutionState::Waiting { wait, .. } => {
                                CanonicalWorkExecutionState::Waiting {
                                    wait_id: wait.wait_id,
                                }
                            }
                            _ => CanonicalWorkExecutionState::Other,
                        };
                        states.insert(work_item_id, state);
                    }
                    states
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
        let SchedulerWorkSelection {
            current: current_work_item,
            current_scheduling_state: current_work_item_scheduling_state,
            queued_runnable: queued_runnable_work_items,
            waiting: waiting_work_item,
            waiting_scheduling_state: waiting_work_item_scheduling_state,
        } = work;
        let turn_in_progress = snapshot.active_run_id.is_some();
        Self {
            now,
            agent_id: snapshot.id,
            status: snapshot.status,
            queue_len,
            has_interrupted_replay,
            active_run_id: snapshot.active_run_id,
            active_tasks,
            has_blocking_active_tasks,
            current_work_item,
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
            last_turn_terminal: snapshot.last_turn_terminal,
            turn_in_progress,
            runtime_error,
            activation_waits,
            canonical_work_states,
        }
    }

    pub(crate) fn work_reactivation_signal(&self) -> Option<WorkReactivationSignal> {
        self.work_reactivation_work_item()
            .map(|(item, reactivation_mode)| WorkReactivationSignal {
                work_item_id: item.id.clone(),
                state: item.state.clone(),
                reactivation_mode,
            })
    }

    pub(crate) fn try_work_reactivation_signal(&self) -> Result<Option<WorkReactivationSignal>> {
        self.work_reactivation_work_item()
            .map(|(item, reactivation_mode)| {
                crate::diagnostics::resource_io::admit_cloned_value(&(&item.id, &item.state))?;
                Ok(WorkReactivationSignal {
                    work_item_id: item.id.clone(),
                    state: item.state.clone(),
                    reactivation_mode,
                })
            })
            .transpose()
    }

    pub(crate) fn work_reactivation_work_item(
        &self,
    ) -> Option<(&WorkItemRecord, WorkReactivationMode)> {
        self.work_reactivation_work_item_with_generation()
            .map(|(item, reactivation_mode, _)| (item, reactivation_mode))
    }

    pub(crate) fn work_reactivation_work_item_with_generation(
        &self,
    ) -> Option<(&WorkItemRecord, WorkReactivationMode, Option<u64>)> {
        // The static hook always proposes the first legal candidate.
        super::semantic_selection::baseline_autonomous_continuation_work_item(self)
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

#[cfg(holon_lib_test_shard = "a")]
#[cfg(test)]
#[path = "projection_queue_tests.rs"]
mod queue_tests;

#[cfg(test)]
#[path = "projection_canonical_ownership_tests.rs"]
mod canonical_ownership_tests;

#[cfg(holon_lib_test_shard = "a")]
#[cfg(test)]
#[path = "projection_agent_ownership_tests.rs"]
mod agent_ownership_tests;

#[cfg(holon_lib_test_shard = "a")]
#[cfg(test)]
#[path = "projection_wait_borrow_tests.rs"]
mod wait_borrow_tests;

#[cfg(holon_lib_test_shard = "a")]
#[cfg(test)]
#[path = "projection_cold_material_tests.rs"]
mod cold_material_tests;

#[cfg(holon_lib_test_shard = "a")]
#[cfg(test)]
#[path = "projection_canonical_material_tests.rs"]
mod canonical_material_tests;
