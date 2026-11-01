//! Explicit storage preparation for immutable, request-scoped context input.
//! This adapter still performs I/O; its token is not a coherent owner revision.
use super::inputs::{
    ContextHistoryAssignments, ContextHistoryBrief, ContextHistoryBriefText, ContextHistoryTool,
    ContextHistoryTranscript, ContextHistoryTurn, ContextInputs,
};
use super::resources::{ContextResourceInput, ContextResourceViews};
use super::*;
use crate::runtime::resident_state::CapturedAgentContext;
use crate::{
    runtime::provider_recovery::ProviderRecoverySource,
    runtime_db::task_result_settlement::TaskResultSettlementRecord, storage::WorkQueueReadModel,
    types::WorkItemPlanArtifact,
};
use anyhow::{Context, Result};
use serde::Serialize;
use std::{
    borrow::Cow,
    collections::BTreeMap,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Instant,
};

static NEXT_SNAPSHOT: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Default, Serialize)]
pub struct ContextReadStage {
    pub name: String,
    pub logical_reads: u64,
    pub returned_records: u64,
    pub elapsed_ns: u64,
}

/// Logical adapter operations, not SQL statements, filesystem syscalls, or
/// disjoint end-to-end timings. Composite stages may include nested reads.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ContextAcquisitionReport {
    pub logical_reads: u64,
    pub returned_records: u64,
    pub stages: Vec<ContextReadStage>,
}

impl ContextAcquisitionReport {
    fn read<T>(
        &mut self,
        name: &str,
        read: impl FnOnce() -> Result<T>,
        count: impl FnOnce(&T) -> usize,
    ) -> Result<T> {
        let started = Instant::now();
        let value = read();
        let rows = value.as_ref().map(count).unwrap_or_default() as u64;
        self.logical_reads += 1;
        self.returned_records += rows;
        let index = self.stages.iter().position(|stage| stage.name == name);
        let stage = if let Some(index) = index {
            &mut self.stages[index]
        } else {
            self.stages.push(ContextReadStage {
                name: name.to_owned(),
                ..Default::default()
            });
            self.stages.last_mut().expect("inserted stage")
        };
        stage.logical_reads += 1;
        stage.returned_records += rows;
        stage.elapsed_ns = stage
            .elapsed_ns
            .saturating_add(started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64);
        value
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextSourceKind {
    Message,
    Turn,
    Brief,
    ToolExecution,
    Episode,
    WorkItem,
}

/// Read-only descriptor; optional metadata preserves domain meanings. The ref
/// is an alias unless the domain supplies an actual immutable version.
#[derive(Debug, Clone, Serialize)]
pub struct ContextSourceDescriptor {
    pub reference: String,
    pub kind: ContextSourceKind,
    pub agent_id: String,
    pub work_item_id: Option<String>,
    pub revision: Option<u64>,
    pub origin: Option<MessageOrigin>,
    pub authority_class: Option<AuthorityClass>,
    pub delivery_surface: Option<crate::types::MessageDeliverySurface>,
    pub admission_context: Option<crate::types::AdmissionContext>,
    /// Preserve named provenance, including opaque ids that are not runtime refs.
    pub provenance_refs: BTreeMap<String, String>,
    pub content_source: Option<crate::types::BriefContentSource>,
    pub body_digest: Option<String>,
    pub source_refs: Vec<String>,
    pub body_locator: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ContextSnapshot {
    id: u64,
    pub(super) data: Arc<ContextSnapshotData>,
    acquisition: ContextAcquisitionReport,
    descriptors: Arc<[ContextSourceDescriptor]>,
    resources: ContextResourceObservation,
    resident_resources: Option<Arc<crate::runtime::resource_observation::ResourceObservation>>,
    resource_allocation: Option<Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
}
impl ContextSnapshot {
    pub(crate) fn resource_allocation(
        &self,
    ) -> Option<&Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>> {
        self.resource_allocation.as_ref()
    }

    pub fn id(&self) -> u64 {
        self.id
    }
    pub fn acquisition(&self) -> &ContextAcquisitionReport {
        &self.acquisition
    }
    pub fn descriptors(&self) -> &[ContextSourceDescriptor] {
        &self.descriptors
    }
    pub(crate) fn agent(&self) -> &AgentState {
        &self.data.agent
    }
    pub(crate) fn execution(&self) -> &ExecutionSnapshot {
        &self.data.execution
    }
    pub(crate) fn current_message(&self) -> &MessageEnvelope {
        &self.data.current_message
    }
    pub(crate) fn skills(&self) -> &SkillsRuntimeView {
        self.data.skills.view()
    }
    #[cfg(test)]
    pub(crate) fn history_policy(&self) -> ContextHistoryPreparation {
        self.data.history_policy
    }
    pub(crate) fn prepared_budget(&self) -> usize {
        self.data.config.prompt_budget_estimated_tokens
    }
    pub(crate) fn resources(&self) -> &ContextResourceObservation {
        &self.resources
    }
    pub(crate) fn resident_resources(
        &self,
    ) -> Option<&Arc<crate::runtime::resource_observation::ResourceObservation>> {
        self.resident_resources.as_ref()
    }
}

/// Retain the original resource observation and its allocation guard together.
#[derive(Debug)]
pub(crate) enum ContextSnapshotSkills {
    Owned(Arc<SkillsRuntimeView>),
    Observed(Arc<crate::runtime::resource_observation::ResourceObservation>),
}

/// A pre-Claim budget target, never a published State, Attempt or Turn root.
#[derive(Clone, Copy)]
pub(crate) struct ContextAdmissionPlan<'a> {
    pub work_item_id: Option<&'a str>,
    pub token_budget: usize,
}

impl ContextSnapshotSkills {
    fn owned(skills: &SkillsRuntimeView) -> Result<Self> {
        crate::diagnostics::resource_io::admit_cloned_value(&(
            &skills.discovered_roots,
            &skills.active_skills,
            &skills.agent_templates_catalog,
        ))?;
        Ok(Self::Owned(Arc::new(skills.clone())))
    }

    pub(crate) fn view(&self) -> &SkillsRuntimeView {
        match self {
            Self::Owned(skills) => skills,
            Self::Observed(observation) => &observation.body().skills,
        }
    }
}

#[derive(Debug)]
pub(super) struct ContextSnapshotData {
    input_budget: Option<(
        crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        crate::runtime::resident_input_evidence::InputEvidenceScope,
    )>,
    input_budget_guard: Option<Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
    pub agent: Arc<AgentState>,
    pub admission_planning: bool,
    pub execution: ExecutionSnapshot,
    pub skills: ContextSnapshotSkills,
    pub current_message: MessageEnvelope,
    pub continuation: Option<ContinuationResolution>,
    pub config: ContextConfig,
    pub history: Arc<HistorySnapshot>,
    pub scoped_history: Option<PreparedScopedHistory>,
    pub history_policy: ContextHistoryPreparation,
    pub recovery_source: Option<ProviderRecoverySource>,
    pub continuation_anchor_messages: Vec<MessageEnvelope>,
    pub active_wait_conditions: Vec<WaitConditionRecord>,
    pub episodes: Vec<Arc<ContextEpisodeRecord>>,
    pub work_queue_projection: WorkQueueReadModel,
    pub current_work_item: Option<WorkItemRecord>,
    pub default_ingress: Option<ExternalTriggerRecord>,
    pub active_task_count: usize,
    pub active_tasks: Vec<TaskRecord>,
    pub completion_reports: BTreeMap<String, CompletionReportProjection>,
    pub resource_views: ContextResourceViews,
    pub pending_task_results: Vec<PendingTaskResult>,
    pub current_turn: Option<ContextHistoryTurn>,
    pub current_input_projection: Option<crate::types::IdleInputBatchItem>,
    pub operator_inputs: BTreeMap<String, Vec<RecoveredOperatorInput>>,
    pub pending_excluded_messages: BTreeSet<String>,
}

pub(super) type RecoveredOperatorInput = ContextHistoryMessage;

impl ContextSnapshotData {
    fn observe_owned_input_bytes(&mut self) -> Result<()> {
        use crate::runtime::input_evidence_budget::{
            serialized_bytes, InputMemoryCategory, OwnedBytesEstimate,
        };
        let Some((budget, scope)) = &self.input_budget else {
            return Ok(());
        };
        let mut payload_bytes = self.history.input_payload_bytes()?;
        if let Some(scoped) = &self.scoped_history {
            payload_bytes = payload_bytes
                .checked_add(scoped.history.input_payload_bytes()?)
                .ok_or_else(|| anyhow::anyhow!("owned context input byte count overflow"))?;
        }
        let recovery = self.recovery_source.as_ref().map(|source| {
            (
                &source.predecessor,
                &source.predecessors,
                &source.source_turns,
                &source.root_message,
            )
        });
        payload_bytes = payload_bytes
            .checked_add(serialized_bytes(&(
                &self.current_message,
                &self.continuation_anchor_messages,
                recovery,
                &self.current_turn,
                &self.current_input_projection,
                &self.operator_inputs,
                &self.pending_excluded_messages,
            ))?)
            .ok_or_else(|| anyhow::anyhow!("owned context input byte count overflow"))?;
        let queue = &self.work_queue_projection;
        let completion_reports = self
            .completion_reports
            .iter()
            .map(|(id, report)| (id, &report.text, &report.brief_id, &report.source_brief))
            .collect::<Vec<_>>();
        payload_bytes = payload_bytes
            .checked_add(serialized_bytes(&(
                &self.current_work_item,
                &self.active_wait_conditions,
                &queue.current,
                &queue.queued_blocked,
                &queue.items,
                &queue.current_runnable,
                &queue.triggered_blocked,
                &queue.queued_runnable,
                &queue.yielded,
                &queue.waiting_for_operator,
                &queue.blocked,
                &queue.completing,
                &queue.completed_recent,
                completion_reports,
            ))?)
            .ok_or_else(|| anyhow::anyhow!("owned context Work byte count overflow"))?;
        // Owned D3/D4 values, including their distinct by-value queue copies.
        // Task/Trigger/resource and provider/tool temporaries remain separate.
        let metadata_estimate_bytes = std::mem::size_of::<Self>() as u64;
        self.input_budget_guard = Some(budget.register_owned_bytes(
            scope,
            InputMemoryCategory::PreparedPrompt,
            OwnedBytesEstimate {
                payload_bytes,
                metadata_estimate_bytes,
            },
        )?);
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct HistorySnapshot {
    pub turn_records: Vec<ContextHistoryTurn>,
    pub messages: Vec<ContextHistoryMessage>,
    pub briefs: Vec<ContextHistoryBrief>,
    pub tools: Vec<ContextHistoryTool>,
    pub transcript: Vec<ContextHistoryTranscript>,
    pub observations: HistoryObservations,
}
impl HistorySnapshot {
    pub(in crate::context) fn input_payload_bytes(&self) -> Result<u64> {
        use crate::runtime::input_evidence_budget::serialized_bytes;

        // Keep the original tuple/list delimiters without serializing shared
        // Message bodies again. Owned legacy Messages keep their exact charge.
        let message_delimiters = u64::try_from(self.messages.len().saturating_sub(1))?
            .checked_add(3)
            .context("context history list byte count overflow")?;
        let bytes = serialized_bytes(&(
            &self.turn_records,
            &self.briefs,
            &self.tools,
            &self.transcript,
            &self.observations,
        ))?
        .checked_add(message_delimiters)
        .context("context history byte count overflow")?;
        self.messages.iter().try_fold(bytes, |bytes, message| {
            bytes
                .checked_add(message.budget_payload_bytes()?)
                .context("context history Message byte count overflow")
        })
    }
}
#[derive(Debug, Clone, Default, Serialize)]
pub(super) struct HistoryObservations {
    brief_text: BTreeMap<String, ContextHistoryBriefText>,
    valid_trigger_summary: BTreeMap<String, bool>,
    message_turn_ids: ContextHistoryAssignments,
}
impl HistoryObservations {
    #[cfg(test)]
    pub(super) fn capture(
        storage: &AppStorage,
        turns: &[impl std::borrow::Borrow<TurnRecord>],
        briefs: &[BriefRecord],
        messages: &[impl std::borrow::Borrow<MessageEnvelope>],
    ) -> Self {
        // Pure formatter fixtures explicitly model assignment observations in
        // their supplied records. Production capture always uses the typed read.
        let mut message_turn_ids = messages
            .iter()
            .map(std::borrow::Borrow::<MessageEnvelope>::borrow)
            .map(|message| (message.id.clone(), message.turn_id.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut valid_trigger_summary = BTreeMap::new();
        for turn in turns {
            let turn: &TurnRecord = turn.borrow();
            let valid = match turn
                .trigger
                .as_ref()
                .and_then(|trigger| trigger.message_id.as_ref())
            {
                Some(id) => {
                    if !message_turn_ids.contains_key(id) {
                        if let Some(message) =
                            storage.read_message_by_id(id).expect("fixture Message")
                        {
                            message_turn_ids.insert(id.clone(), message.turn_id);
                        }
                    }
                    message_turn_ids
                        .get(id)
                        .is_none_or(|assigned| turn_record_owns_object(turn, assigned.as_deref()))
                }
                None => true,
            };
            valid_trigger_summary.insert(turn.turn_id.clone(), valid);
        }
        let brief_text = briefs
            .iter()
            .map(std::borrow::Borrow::<BriefRecord>::borrow)
            .map(|brief| {
                (
                    brief.id.clone(),
                    ContextHistoryBriefText::owned(
                        resolved_brief_text_from_inputs(storage, brief)
                            .expect("fixture Brief source"),
                    ),
                )
            })
            .collect();
        Self {
            brief_text,
            valid_trigger_summary,
            message_turn_ids: message_turn_ids.into(),
        }
    }
    pub(super) fn capture_inputs(
        inputs: &impl ContextInputs,
        turns: &[impl std::borrow::Borrow<TurnRecord>],
        briefs: &[impl std::borrow::Borrow<BriefRecord>],
        messages: &[impl std::borrow::Borrow<MessageEnvelope>],
    ) -> Result<Self> {
        let brief_text = briefs
            .iter()
            .map(std::borrow::Borrow::<BriefRecord>::borrow)
            .map(|brief| Ok((brief.id.clone(), inputs.history_brief_text(brief)?)))
            .collect::<Result<_>>()?;
        let message_ids = messages
            .iter()
            .map(std::borrow::Borrow::<MessageEnvelope>::borrow)
            .map(|message| message.id.as_str())
            .chain(
                turns
                    .iter()
                    .map(std::borrow::Borrow::<TurnRecord>::borrow)
                    .flat_map(|turn| {
                        turn.input_message_ids.iter().map(String::as_str).chain(
                            turn.trigger
                                .as_ref()
                                .and_then(|trigger| trigger.message_id.as_deref()),
                        )
                    }),
            );
        let message_turn_ids = inputs.history_assignment_view(message_ids)?;
        let valid_trigger_summary = turns
            .iter()
            .map(std::borrow::Borrow::<TurnRecord>::borrow)
            .map(|record| {
                let valid = match record
                    .trigger
                    .as_ref()
                    .and_then(|trigger| trigger.message_id.as_ref())
                {
                    Some(id) => {
                        !inputs.message_exists(id)?
                            || turn_record_owns_object(
                                record,
                                message_turn_ids
                                    .get(id)
                                    .and_then(|assigned| assigned.as_deref()),
                            )
                    }
                    None => true,
                };
                Ok((record.turn_id.clone(), valid))
            })
            .collect::<Result<_>>()?;
        Ok(Self {
            brief_text,
            valid_trigger_summary,
            message_turn_ids,
        })
    }
    pub(super) fn assigned_turn_for_message(&self, id: &str) -> Option<&str> {
        self.message_turn_ids
            .get(id)
            .and_then(|assigned| assigned.as_deref())
    }
    pub(super) fn message_belongs_to_turn(&self, turn: &TurnRecord, id: &str) -> bool {
        turn_record_owns_object(turn, self.assigned_turn_for_message(id))
    }
    pub(super) fn brief_text<'a>(&'a self, brief: &'a BriefRecord) -> &'a str {
        self.brief_text
            .get(&brief.id)
            .map(ContextHistoryBriefText::as_str)
            .unwrap_or(&brief.text)
    }
    pub(super) fn trigger_summary_valid(&self, turn: &TurnRecord) -> bool {
        self.valid_trigger_summary
            .get(&turn.turn_id)
            .copied()
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PendingTaskResult {
    pub record: TaskResultSettlementRecord,
    pub message: Option<super::inputs::ContextHistoryMessage>,
    pub reply: Option<super::inputs::ContextHistoryMessage>,
}

/// Frozen acquisition policy, selected before opening the SQLite view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ContextHistoryPreparation {
    pub selector: crate::projection_eval::HistorySelector,
    pub work_item_scoped_window: Option<usize>,
    pub window_source: &'static str,
}
impl Default for ContextHistoryPreparation {
    fn default() -> Self {
        Self {
            selector: crate::projection_eval::HistorySelector::RecentTurns,
            work_item_scoped_window: None,
            window_source: "context_config",
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct PreparedScopedHistory {
    pub window_limit: usize,
    pub history: Arc<HistorySnapshot>,
}

/// Fully owned durable records. Contains no read capability, root/map or transaction.
pub(crate) struct DurableContextInputs {
    data: ContextSnapshotData,
    acquisition: ContextAcquisitionReport,
}

impl DurableContextInputs {
    /// Loaded plans are immutable values from the same stamped observation.
    pub(crate) fn observe_loaded(
        self,
        observation: Arc<crate::runtime::resource_observation::ResourceObservation>,
        allocation: Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>,
    ) -> Result<ContextSnapshot> {
        crate::diagnostics::resource_io::check_active_budget()?;
        let mut snapshot = observe_context_resource_views(
            self,
            ContextResourceInput::Loaded(observation.clone()),
            observation.epoch(),
        )?;
        snapshot.resident_resources = Some(observation);
        snapshot.resource_allocation = Some(allocation);
        Ok(snapshot)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ContextResourceObservation {
    pub generation: u64,
    pub digest: String,
}

/// Filesystem-only resource capability. Cannot query SQLite or durable records.
pub(crate) trait ContextResources {
    fn plan(&self, work_item: &WorkItemRecord) -> Result<Option<WorkItemPlanArtifact>>;
    fn observes_recorded_plans(&self) -> bool {
        false
    }
    fn notes(&self) -> Option<String>;
}

pub(crate) struct FilesystemContextResources<'a> {
    pub data_dir: &'a Path,
    pub agent_home: &'a Path,
}
impl ContextResources for FilesystemContextResources<'_> {
    fn plan(&self, item: &WorkItemRecord) -> Result<Option<WorkItemPlanArtifact>> {
        Ok(crate::work_item_plan::ensure_plan_artifact(self.data_dir, item, None).ok())
    }
    fn notes(&self) -> Option<String> {
        crate::notes_catalog::render_agent_home_notes_catalog_section(self.agent_home)
    }
}

/// Transitional adapter: sequential persistence reads do not claim the atomic
/// publication guarantee of the future AgentStateStore. Success prepares all
/// I/O dependencies used by the current renderer, including optional history.
#[allow(clippy::too_many_arguments)]
pub fn capture_context_from_storage(
    storage: &AppStorage,
    agent: &AgentState,
    execution: &ExecutionSnapshot,
    skills: &SkillsRuntimeView,
    current_message: &MessageEnvelope,
    continuation: Option<&ContinuationResolution>,
    config: &ContextConfig,
    agent_home: &Path,
    default_external_ingress_override: Option<&ExternalTriggerRecord>,
) -> Result<ContextSnapshot> {
    // Preserve the legacy full acquisition span: Episode reads through resources.
    let _timer = crate::diagnostics::attribution::CONTEXT_ACQUIRE.start();
    let mut report = ContextAcquisitionReport::default();
    let episodes = report
        .read(
            "episodes",
            || storage.read_recent_context_episodes(config.recent_episode_candidates),
            Vec::len,
        )?
        .into_iter()
        .map(Arc::new)
        .collect();
    crate::diagnostics::resource_io::admit_cloned_value(agent)?;
    let durable = acquire_durable_context_inputs(
        storage,
        Arc::new(agent.clone()),
        Cow::Borrowed(execution),
        ContextSnapshotSkills::owned(skills)?,
        current_message,
        continuation,
        config,
        default_external_ingress_override,
        ContextHistoryPreparation::default(),
        episodes,
        report,
        None,
        None,
    )?;
    observe_context_resources(
        durable,
        &super::FilesystemContextResources {
            data_dir: storage.data_dir(),
            agent_home,
        },
        0,
    )
}

/// The caller owns the coherent SQLite snapshot and validates it against the
/// captured owner root. This function has no filesystem or publication capability.
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub(crate) fn acquire_durable_context(
    inputs: &impl ContextInputs,
    agent: &AgentState,
    execution: &ExecutionSnapshot,
    skills: &SkillsRuntimeView,
    current_message: &MessageEnvelope,
    continuation: Option<&ContinuationResolution>,
    config: &ContextConfig,
    default_external_ingress_override: Option<&ExternalTriggerRecord>,
    history_policy: ContextHistoryPreparation,
    resident: &CapturedAgentContext,
) -> Result<DurableContextInputs> {
    acquire_durable_context_with_work_queue(
        inputs,
        agent,
        Cow::Borrowed(execution),
        ContextSnapshotSkills::owned(skills)?,
        current_message,
        continuation,
        config,
        default_external_ingress_override,
        history_policy,
        resident,
        None,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn acquire_durable_context_with_work_queue(
    inputs: &impl ContextInputs,
    agent: &AgentState,
    execution: Cow<'_, ExecutionSnapshot>,
    skills: ContextSnapshotSkills,
    current_message: &MessageEnvelope,
    continuation: Option<&ContinuationResolution>,
    config: &ContextConfig,
    default_external_ingress_override: Option<&ExternalTriggerRecord>,
    history_policy: ContextHistoryPreparation,
    resident: &CapturedAgentContext,
    prepared_work_queue: Option<WorkQueueReadModel>,
    admission: Option<ContextAdmissionPlan<'_>>,
) -> Result<DurableContextInputs> {
    anyhow::ensure!(
        resident.state.as_ref() == agent,
        "context posture differs from captured owner publication"
    );
    anyhow::ensure!(
        resident.token.identity_incarnation > 0,
        "context capture has no durable identity"
    );
    anyhow::ensure!(
        config.recent_episode_candidates == resident.episode_candidate_limit,
        "Episode policy differs from resident capture; explicit target preparation required"
    );
    anyhow::ensure!(
        admission.is_none_or(|plan| config.prompt_budget_estimated_tokens <= plan.token_budget),
        "admission budget expansion requires a newly prepared plan"
    );
    let episodes = resident.episodes.clone();
    acquire_durable_context_inputs(
        inputs,
        resident.state.clone(),
        execution,
        skills,
        current_message,
        continuation,
        config,
        default_external_ingress_override,
        history_policy,
        episodes,
        ContextAcquisitionReport::default(),
        prepared_work_queue,
        admission,
    )
}

#[allow(clippy::too_many_arguments)]
fn acquire_durable_context_inputs(
    inputs: &impl ContextInputs,
    agent: Arc<AgentState>,
    execution: Cow<'_, ExecutionSnapshot>,
    skills: ContextSnapshotSkills,
    current_message: &MessageEnvelope,
    continuation: Option<&ContinuationResolution>,
    config: &ContextConfig,
    default_external_ingress_override: Option<&ExternalTriggerRecord>,
    history_policy: ContextHistoryPreparation,
    episodes: Vec<Arc<ContextEpisodeRecord>>,
    mut report: ContextAcquisitionReport,
    prepared_work_queue: Option<WorkQueueReadModel>,
    admission: Option<ContextAdmissionPlan<'_>>,
) -> Result<DurableContextInputs> {
    anyhow::ensure!(
        history_policy.selector != crate::projection_eval::HistorySelector::WorkItemScoped
            || history_policy.work_item_scoped_window.is_some(),
        "work-item history policy requires a frozen window before acquisition"
    );
    // State and loaded skills retain their original qualified holders. Admit
    // only the remaining owning Current clones before any reader.
    match &execution {
        Cow::Borrowed(execution) => {
            crate::diagnostics::resource_io::admit_cloned_value(&(
                execution,
                current_message,
                continuation,
                &config.callback_base_url,
                default_external_ingress_override,
            ))?;
        }
        Cow::Owned(_) => {
            crate::diagnostics::resource_io::admit_cloned_value(&(
                current_message,
                continuation,
                &config.callback_base_url,
                default_external_ingress_override,
            ))?;
        }
    }
    macro_rules! read {
        ($name:literal, $read:expr, $count:expr) => {
            report.read($name, || $read, $count)?
        };
    }
    let recovery_source = read!("recovery", inputs.recovery_source(current_message), |v| {
        usize::from(v.is_some())
    });
    let continuation_anchor_messages =
        if recovery_source.is_some() || super::is_trusted_operator_input(current_message) {
            Vec::new()
        } else {
            read!("anchor_messages", inputs.anchor_messages(), Vec::len)
        };
    let mut briefs = read!(
        "briefs",
        inputs.history_briefs(config.recent_briefs),
        Vec::len
    );
    let mut tools = read!(
        "tools",
        inputs.history_tools(config.recent_messages),
        Vec::len
    );
    let mut turn_records = read!(
        "turns",
        inputs.history_turns(None, &current_message.id, config.recent_messages),
        Vec::len
    );
    for source_turn in recovery_source
        .iter()
        .flat_map(|source| &source.source_turns)
    {
        if !turn_records
            .iter()
            .any(|record| record.turn_id == source_turn.turn_id)
        {
            turn_records.push(ContextHistoryTurn::copy_owned(source_turn)?);
        }
    }
    let mut messages = Vec::new();
    read!(
        "history_references",
        hydrate_recent_turn_references(
            inputs,
            &turn_records,
            &mut messages,
            &mut briefs,
            &mut tools
        ),
        |_| 0
    );
    let mut transcript = read!(
        "transcript",
        inputs.history_transcript(config.recent_messages),
        Vec::len
    );
    for source_turn in recovery_source
        .iter()
        .flat_map(|source| &source.source_turns)
    {
        for entry in read!(
            "recovery_transcript",
            inputs.history_transcript_for_turns(&[source_turn.turn_id.clone()]),
            Vec::len
        ) {
            if !transcript.iter().any(|existing| existing == &entry) {
                transcript.push(entry);
            }
        }
    }
    let active_wait_conditions = read!("waits", inputs.active_waits(&agent.id), Vec::len)
        .into_iter()
        .filter(|condition| condition.work_item_id.is_some())
        .collect();
    let work_queue_projection = read!(
        "work_queue",
        prepared_work_queue.map_or_else(|| inputs.work_queue(), Ok),
        |_| 0
    );
    let execution_work_item = admission
        .map(|plan| plan.work_item_id)
        .unwrap_or_else(|| {
            agent
                .current_execution_binding
                .as_ref()
                .and_then(|binding| binding.work_item_id.as_deref())
        })
        .map(|id| {
            report.read(
                "execution_work_item",
                || inputs.work_item(id),
                |v| usize::from(v.is_some()),
            )
        })
        .transpose()?
        .flatten();
    let current_work_item = if admission.is_some()
        || agent.current_execution_binding.is_some()
        || recovery_source.is_some()
    {
        execution_work_item
    } else {
        work_queue_projection.current.clone()
    };
    let default_ingress = match default_external_ingress_override {
        Some(record) => Some(record.clone()),
        None => read!(
            "default_ingress",
            default_external_ingress(inputs, &agent.id),
            |v| usize::from(v.is_some())
        ),
    };
    let all_active_tasks = read!("active_tasks", inputs.active_tasks(&agent.id), Vec::len);
    let active_task_count = all_active_tasks.len();
    // Canonical active_for_agent is newest-first; preserve its display slice.
    let active_tasks = all_active_tasks
        .into_iter()
        .take(ACTIVE_TASKS_CONTEXT_LIMIT)
        .collect();
    let completion_work_item_ids =
        completion_work_item_ids(current_work_item.as_ref(), &work_queue_projection)?;
    let completion_reports = read!(
        "completion_reports",
        latest_completion_report_by_work_item(inputs, &agent.id, &completion_work_item_ids),
        BTreeMap::len
    );
    let observations = read!(
        "history_observations",
        HistoryObservations::capture_inputs(inputs, &turn_records, &briefs, &messages),
        |_| 0
    );
    let mut pending_task_results = read!(
        "pending_task_results",
        // A new Claim has no admitted settlements or started Turn yet. Keep
        // recovery evidence rooted in its actual sources, not a proposed id.
        if admission.is_some() {
            Ok(Vec::new())
        } else {
            load_pending_task_results(inputs, &agent, current_message)
        },
        Vec::len
    );
    let current_turn = match agent
        .current_turn_id
        .as_deref()
        .filter(|_| admission.is_none())
    {
        Some(id) => inputs.history_turn_by_id(id)?,
        None => None,
    };
    let input_turns = current_turn
        .as_deref()
        .into_iter()
        .chain(
            recovery_source
                .iter()
                .flat_map(|source| &source.source_turns),
        )
        .collect::<Vec<_>>();
    let live_identities = input_turns
        .iter()
        .flat_map(|turn| &turn.live_input_batches)
        .flat_map(|batch| &batch.result_identities)
        .collect::<BTreeSet<_>>();
    pending_task_results.retain(|entry| !live_identities.contains(&entry.record.result_identity));
    let pending_excluded_messages = input_turns
        .iter()
        .flat_map(|turn| {
            turn.idle_input_batch
                .iter()
                .flat_map(|batch| &batch.items)
                .chain(
                    turn.live_input_batches
                        .iter()
                        .flat_map(|batch| &batch.items),
                )
        })
        .map(|item| item.message_id.clone())
        .chain(
            recovery_source
                .iter()
                .map(|source| source.root_message.id.clone()),
        )
        .collect();
    let operator_inputs = acquire_operator_inputs(inputs, &input_turns)?;
    let current_input_projection = if admission.is_some() {
        None
    } else {
        inputs.current_input_projection(&agent.id, &current_message.id)?
    };
    let history = Arc::new(HistorySnapshot {
        turn_records,
        messages,
        briefs,
        tools,
        transcript,
        observations,
    });
    let scoped_history = match (
        history_policy.work_item_scoped_window,
        current_work_item.as_ref(),
    ) {
        (Some(window), Some(work_item)) => Some(PreparedScopedHistory {
            window_limit: window,
            history: Arc::new(acquire_scoped_history(
                inputs,
                current_message,
                work_item,
                window,
                recovery_source.as_ref(),
            )?),
        }),
        _ => None,
    };
    let mut data = ContextSnapshotData {
        input_budget: inputs.input_budget(),
        input_budget_guard: None,
        agent,
        admission_planning: admission.is_some(),
        execution: execution.into_owned(),
        skills,
        current_message: current_message.clone(),
        continuation: continuation.cloned(),
        config: config.clone(),
        history,
        scoped_history,
        history_policy,
        recovery_source,
        continuation_anchor_messages,
        active_wait_conditions,
        episodes,
        work_queue_projection,
        current_work_item,
        default_ingress,
        active_task_count,
        active_tasks,
        completion_reports,
        resource_views: ContextResourceViews::default(),
        pending_task_results,
        current_turn,
        current_input_projection,
        operator_inputs,
        pending_excluded_messages,
    };
    data.observe_owned_input_bytes()?;
    Ok(DurableContextInputs {
        data,
        acquisition: report,
    })
}

/// Runs after the read transaction has ended. Observed bytes are frozen here;
/// the caller must reject an obsolete owner/resource generation before use.
pub(crate) fn observe_context_resources(
    durable: DurableContextInputs,
    resources: &impl ContextResources,
    resource_generation: u64,
) -> Result<ContextSnapshot> {
    observe_context_resource_views(
        durable,
        ContextResourceInput::Filesystem(resources),
        resource_generation,
    )
}

fn observe_context_resource_views(
    durable: DurableContextInputs,
    resources: ContextResourceInput<'_>,
    resource_generation: u64,
) -> Result<ContextSnapshot> {
    let DurableContextInputs {
        mut data,
        acquisition: mut report,
    } = durable;
    macro_rules! read {
        ($name:literal, $read:expr, $count:expr) => {
            report.read($name, || $read, $count)?
        };
    }
    let current_work_item = data.current_work_item.as_ref();
    let work_queue_projection = &data.work_queue_projection;
    let completion_reports = &data.completion_reports;
    // Cold compatibility keeps its creation policy; loaded reads only values.
    let mut resource_views = ContextResourceViews::new(&resources);
    if let Some(item) = current_work_item {
        let artifact = read!("plan_observations", resources.plan(item), |v| {
            usize::from(v.is_some())
        });
        if let Some(artifact) = artifact {
            resource_views.insert_plan(&item.id, artifact)?;
        }
    }
    for item in candidate_items(work_queue_projection) {
        if current_work_item
            .as_ref()
            .is_some_and(|current| current.id == item.id)
            || (item.plan_artifact.is_some() && !resources.observes_recorded_plans())
            || resource_views.plan(&item.id).is_some()
        {
            continue;
        }
        if item.state == crate::types::WorkItemState::Completed
            && completion_report_for_work_item(&completion_reports, item).is_none()
        {
            continue;
        }
        let artifact = read!("plan_observations", resources.plan(item), |v| {
            usize::from(v.is_some())
        });
        if let Some(artifact) = artifact {
            resource_views.insert_plan(&item.id, artifact)?;
        }
    }
    let notes_content = read!("notes_observation", Ok(resources.notes()), |v| usize::from(
        v.is_some()
    ));

    resource_views.set_notes(notes_content);
    let digest = resource_views.digest()?;
    data.resource_views = resource_views;
    let descriptors = descriptors(&data, resources.observes_recorded_plans()).into();
    Ok(ContextSnapshot {
        id: NEXT_SNAPSHOT.fetch_add(1, Ordering::Relaxed),
        data: Arc::new(data),
        acquisition: report,
        descriptors,
        resources: ContextResourceObservation {
            generation: resource_generation,
            digest,
        },
        resident_resources: None,
        resource_allocation: None,
    })
}

/// Shared with explicit resident preparation, preserving the original bounded
/// candidate selector rather than loading every non-open Work's result body.
pub(crate) fn completion_work_item_ids(
    current_work_item: Option<&WorkItemRecord>,
    projection: &WorkQueueReadModel,
) -> Result<BTreeSet<String>> {
    crate::diagnostics::resource_io::check_active_budget()?;
    let mut ids = BTreeSet::new();
    for work in current_work_item
        .into_iter()
        .chain(projection.current.iter())
        .chain(candidate_items(projection))
    {
        if ids.contains(work.id.as_str()) {
            continue;
        }
        // Only an owned ID and its set slot are allocated, not a JSON resolver.
        let bytes = work
            .id
            .len()
            .checked_add(std::mem::size_of::<String>() + 128)
            .ok_or_else(|| anyhow::anyhow!("ResourceExhausted: completion selector bytes"))?;
        crate::diagnostics::resource_io::charge_owned_metadata(u64::try_from(bytes)?, 2)?;
        ids.insert(work.id.clone());
    }
    Ok(ids)
}

fn candidate_items(projection: &WorkQueueReadModel) -> impl Iterator<Item = &WorkItemRecord> {
    projection
        .triggered_blocked
        .iter()
        .chain(projection.current_runnable.iter())
        .chain(projection.queued_runnable.iter())
        .chain(projection.yielded.iter())
        .chain(projection.waiting_for_operator.iter())
        .chain(projection.blocked.iter())
        .chain(projection.completing.iter())
        .chain(projection.completed_recent.iter())
        .map(|item| item.record())
}

pub(super) fn load_pending_task_results(
    storage: &impl ContextInputs,
    agent: &AgentState,
    current_message: &MessageEnvelope,
) -> Result<Vec<PendingTaskResult>> {
    let Some(activation_id) = agent
        .current_execution_binding
        .as_ref()
        .and_then(|binding| binding.activation_id.as_deref())
    else {
        return Ok(Vec::new());
    };
    Ok(storage
        .admitted_settlement_inputs(&agent.id, activation_id, &current_message.id, usize::MAX)?
        .into_iter()
        .map(|input| PendingTaskResult {
            record: input.record,
            message: input.message,
            reply: input.reply,
        })
        .collect())
}

fn acquire_operator_inputs(
    inputs: &impl ContextInputs,
    turns: &[&TurnRecord],
) -> Result<BTreeMap<String, Vec<RecoveredOperatorInput>>> {
    let mut recovered = BTreeMap::new();
    for turn in turns {
        let batched = turn
            .idle_input_batch
            .iter()
            .flat_map(|batch| &batch.items)
            .chain(
                turn.live_input_batches
                    .iter()
                    .flat_map(|batch| &batch.items),
            )
            .map(|item| &item.message_id)
            .collect::<BTreeSet<_>>();
        let mut projections = Vec::new();
        for id in &turn.input_message_ids {
            if batched.contains(id)
                || turn
                    .trigger
                    .as_ref()
                    .and_then(|trigger| trigger.message_id.as_ref())
                    == Some(id)
            {
                continue;
            }
            let Some(message) = inputs.history_message_by_id(id)? else {
                continue;
            };
            if message.agent_id != turn.agent_id
                || message.kind != MessageKind::OperatorPrompt
                || !super::is_trusted_operator_input(&message)
                || message.priority != crate::types::Priority::Interject
            {
                continue;
            }
            projections.push(message);
        }
        recovered.insert(turn.turn_id.clone(), projections);
    }
    Ok(recovered)
}

fn acquire_scoped_history(
    inputs: &impl ContextInputs,
    current_message: &MessageEnvelope,
    work_item: &WorkItemRecord,
    window_limit: usize,
    recovery_source: Option<&ProviderRecoverySource>,
) -> Result<HistorySnapshot> {
    let owner = crate::types::TurnOwner::WorkItem {
        work_item_id: work_item.id.clone(),
    };
    let mut turn_records = inputs.history_turns(Some(&owner), &current_message.id, window_limit)?;
    for source_turn in recovery_source
        .into_iter()
        .flat_map(|source| &source.source_turns)
    {
        if !turn_records
            .iter()
            .any(|record| record.turn_id == source_turn.turn_id)
        {
            turn_records.push(ContextHistoryTurn::copy_owned(source_turn)?);
        }
    }
    let (mut messages, mut briefs, mut tools) = (Vec::new(), Vec::new(), Vec::new());
    hydrate_recent_turn_references(
        inputs,
        &turn_records,
        &mut messages,
        &mut briefs,
        &mut tools,
    )?;
    let ids = turn_records
        .iter()
        .map(|record| record.turn_id.clone())
        .collect::<Vec<_>>();
    let transcript = inputs.history_transcript_for_turns(&ids)?;
    let observations =
        HistoryObservations::capture_inputs(inputs, &turn_records, &briefs, &messages)?;
    Ok(HistorySnapshot {
        turn_records,
        messages,
        briefs,
        tools,
        transcript,
        observations,
    })
}

/// Legacy explicit I/O adapter retained for fixtures and old preview callers.
/// The coherent runtime path prepares both windows in acquire_durable_context.
#[cfg(test)]
pub(crate) fn prepare_work_item_scoped_history(
    storage: &AppStorage,
    reprojection: &RecentTurnsReprojection,
    window_limit: usize,
) -> Result<RecentTurnsReprojection> {
    let Some(work_item) = reprojection.current_work_item.as_ref() else {
        return Ok(reprojection.clone());
    };
    if reprojection
        .prepared_scoped
        .as_ref()
        .is_some_and(|scope| scope.window_limit == window_limit)
    {
        return reprojection.select_scoped(window_limit);
    }
    let _timer = crate::diagnostics::attribution::CONTEXT_HISTORY_PREPARE.start();
    let recovery = crate::runtime::provider_recovery::resolve_for_context(
        storage,
        &reprojection.current_message,
    )?;
    let history = Arc::new(acquire_scoped_history(
        storage,
        &reprojection.current_message,
        work_item,
        window_limit,
        recovery.as_ref(),
    )?);
    let mut prepared = reprojection.clone();
    prepared.prepared_scoped = Some(PreparedScopedHistory {
        window_limit,
        history,
    });
    prepared.snapshot_id = NEXT_SNAPSHOT.fetch_add(1, Ordering::Relaxed);
    prepared.select_scoped(window_limit)
}

fn descriptors(
    data: &ContextSnapshotData,
    observed_recorded_plans: bool,
) -> Vec<ContextSourceDescriptor> {
    let mut out = BTreeMap::new();
    let base = |reference: String, kind, agent_id: &str, work_item_id: Option<String>| {
        ContextSourceDescriptor {
            reference,
            kind,
            agent_id: agent_id.to_owned(),
            work_item_id,
            revision: None,
            origin: None,
            authority_class: None,
            delivery_surface: None,
            admission_context: None,
            provenance_refs: BTreeMap::new(),
            content_source: None,
            body_digest: None,
            source_refs: Vec::new(),
            body_locator: None,
        }
    };
    for message in data
        .history
        .messages
        .iter()
        .chain(
            data.scoped_history
                .iter()
                .flat_map(|scope| scope.history.messages.iter()),
        )
        .map(std::borrow::Borrow::<MessageEnvelope>::borrow)
        .chain(
            data.operator_inputs
                .values()
                .flatten()
                .map(std::borrow::Borrow::<MessageEnvelope>::borrow),
        )
        .chain(data.continuation_anchor_messages.iter())
        .chain(std::iter::once(&data.current_message))
        .chain(
            data.pending_task_results
                .iter()
                .filter_map(|entry| entry.message.as_deref()),
        )
        .chain(
            data.pending_task_results
                .iter()
                .filter_map(|entry| entry.reply.as_deref()),
        )
        .chain(
            data.recovery_source
                .iter()
                .map(|source| &source.root_message),
        )
    {
        let mut item = base(
            format!("message:{}", message.id),
            ContextSourceKind::Message,
            &message.agent_id,
            message.work_item_id.clone(),
        );
        item.origin = Some(message.origin.clone());
        item.authority_class = Some(message.authority_class);
        item.delivery_surface = message.delivery_surface;
        item.admission_context = message.admission_context;
        item.provenance_refs = message.source_refs.clone();
        item.source_refs = message
            .source_refs
            .values()
            .filter(|reference| crate::memory::refs::RuntimeRef::parse(reference).is_ok())
            .cloned()
            .collect();
        item.body_locator = Some(item.reference.clone());
        out.insert(item.reference.clone(), item);
    }
    for turn in data
        .history
        .turn_records
        .iter()
        .chain(
            data.scoped_history
                .iter()
                .flat_map(|scope| scope.history.turn_records.iter()),
        )
        .chain(data.current_turn.iter())
    {
        let mut item = base(
            format!("turn:{}", turn.turn_id),
            ContextSourceKind::Turn,
            &turn.agent_id,
            turn.current_work_item_id.clone(),
        );
        item.source_refs = turn
            .input_message_ids
            .iter()
            .map(|id| format!("message:{id}"))
            .chain(
                turn.produced_brief_ids
                    .iter()
                    .map(|id| format!("brief:{id}")),
            )
            .chain(
                turn.tool_execution_ids
                    .iter()
                    .map(|id| format!("tool_execution:{id}:output")),
            )
            .collect();
        out.insert(item.reference.clone(), item);
    }
    for brief in data
        .history
        .briefs
        .iter()
        .map(std::borrow::Borrow::<BriefRecord>::borrow)
        .chain(data.scoped_history.iter().flat_map(|scope| {
            scope
                .history
                .briefs
                .iter()
                .map(std::borrow::Borrow::<BriefRecord>::borrow)
        }))
        .chain(
            data.completion_reports
                .values()
                .filter_map(|report| report.source_brief.as_ref()),
        )
    {
        let mut item = base(
            format!("brief:{}", brief.id),
            ContextSourceKind::Brief,
            &brief.agent_id,
            brief.work_item_id.clone(),
        );
        item.source_refs = brief
            .related_message_id
            .iter()
            .map(|id| format!("message:{id}"))
            .collect();
        item.content_source = Some(brief.content_source.clone());
        item.body_locator = Some(item.reference.clone());
        out.insert(item.reference.clone(), item);
    }
    for tool in data.history.tools.iter().chain(
        data.scoped_history
            .iter()
            .flat_map(|scope| scope.history.tools.iter()),
    ) {
        let mut item = base(
            format!("tool_execution:{}:output", tool.id),
            ContextSourceKind::ToolExecution,
            &tool.agent_id,
            tool.work_item_id.clone(),
        );
        item.authority_class = Some(tool.authority_class);
        item.body_locator = Some(item.reference.clone());
        out.insert(item.reference.clone(), item);
    }
    for episode in &data.episodes {
        let mut item = base(
            format!("episode:{}", episode.id),
            ContextSourceKind::Episode,
            &episode.agent_id,
            episode.current_work_item_id.clone(),
        );
        item.source_refs = episode.source_refs.clone();
        item.body_locator = Some(item.reference.clone());
        out.insert(item.reference.clone(), item);
    }
    for work in data
        .current_work_item
        .iter()
        .chain(candidate_items(&data.work_queue_projection))
    {
        let mut item = base(
            format!("work_item:{}", work.id),
            ContextSourceKind::WorkItem,
            &work.agent_id,
            Some(work.id.clone()),
        );
        item.revision = Some(work.revision);
        item.source_refs = work
            .work_refs
            .iter()
            .map(|source| source.ref_id.clone())
            .collect();
        // A plan path is an observed mutable resource, not a new runtime namespace
        // or a promise that the historical bytes can be retrieved later.
        let plan = data.resource_views.plan(&work.id).or_else(|| {
            if observed_recorded_plans {
                None
            } else {
                work.plan_artifact.as_ref()
            }
        });
        if let Some(plan) = plan {
            item.body_locator = Some(plan.path.display().to_string());
            item.body_digest = Some(plan.hash.clone());
        }
        // Current execution's record is inserted first and owns the rendered
        // current WorkItem, even if the earlier queue observation has the same id.
        out.entry(item.reference.clone()).or_insert(item);
    }
    out.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        BriefContentSource, BriefContentSourceRelation, BriefKind, Priority, WorkItemState,
    };

    #[test]
    fn history_message_budget_preserves_legacy_json_bytes() -> Result<()> {
        use crate::runtime::input_evidence_budget::serialized_bytes;
        use crate::types::{AuthorityClass, MessageBody, MessageKind, MessageOrigin};

        for texts in [
            Vec::new(),
            vec![""],
            vec!["ascii", "汉🦀\n\"\\", "last\tmessage"],
        ] {
            let history = HistorySnapshot {
                messages: texts
                    .into_iter()
                    .map(|text| {
                        ContextHistoryMessage::owned(MessageEnvelope::new(
                            "default",
                            MessageKind::OperatorPrompt,
                            MessageOrigin::Operator {
                                actor_id: None,
                                actor_display_name: None,
                            },
                            AuthorityClass::OperatorInstruction,
                            Priority::Normal,
                            MessageBody::Text { text: text.into() },
                        ))
                    })
                    .collect::<Result<_>>()?,
                ..Default::default()
            };
            assert_eq!(
                history.input_payload_bytes()?,
                serialized_bytes(&(
                    &history.turn_records,
                    &history.messages,
                    &history.briefs,
                    &history.tools,
                    &history.transcript,
                    &history.observations,
                ))?
            );
        }
        Ok(())
    }

    #[test]
    fn completion_selector_admits_unique_allocations_and_preserves_both_axis_refusals() -> Result<()>
    {
        use crate::{
            config::StateMemoryLimits,
            diagnostics::resource_io::ResourceReadMaterial,
            runtime::{
                input_evidence_budget::InputEvidenceBudgetLedger,
                resident_input_evidence::InputEvidenceScope,
            },
        };
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let baseline = ledger.report()?.observed_bytes;
        let scope = InputEvidenceScope {
            agent_id: "default".into(),
            identity_incarnation: 1,
        };
        let work = WorkItemRecord::new("default", "selector", WorkItemState::Open);
        let projection = WorkQueueReadModel {
            current: Some(work.clone()),
            ..Default::default()
        };
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, 1_000_000, 80)?;
        let (_, header_records) = phase.finish()?;
        {
            let _active = phase.enter();
            let ids = completion_work_item_ids(Some(&work), &projection)?;
            assert_eq!(ids, BTreeSet::from([work.id.clone()]));
        }
        let (bytes, records) = phase.finish()?;
        assert_eq!(
            records - header_records,
            16,
            "one ID and set slot, at the original 8x factor"
        );
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, baseline);
        for (bytes, records) in [(bytes - 1, records), (bytes, records - 1)] {
            let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, bytes, records)?;
            {
                let _active = phase.enter();
                assert!(completion_work_item_ids(Some(&work), &projection).is_err());
                assert!(completion_work_item_ids(None, &WorkQueueReadModel::default()).is_err());
            }
            assert!(phase.finish().is_err());
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, baseline);
        }
        Ok(())
    }

    #[test]
    fn copied_brief_source_is_metered_until_last_snapshot_clone_drops() {
        use crate::runtime::{
            input_evidence_budget::{InputEvidenceBudgetLedger, InputMemoryCategory},
            resident_input_evidence::InputEvidenceScope,
        };
        let dir = tempfile::tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
        let agent = AgentState::new("default");
        storage.write_agent_for_fixture(&agent).unwrap();
        let source_text = "outside-window-source".repeat(16_384);
        let source = TranscriptEntry::new(
            "default",
            TranscriptEntryKind::AssistantRound,
            None,
            None,
            serde_json::json!({"blocks":[{"type":"text","text":source_text}]}),
        );
        storage.append_transcript_entry(&source).unwrap();
        let mut brief =
            BriefRecord::new("default", BriefKind::Result, "short fallback", None, None);
        brief.content_source = BriefContentSource::TranscriptEntry {
            entry_id: source.id.clone(),
            relation: BriefContentSourceRelation::Finalizes,
        };
        storage.append_brief(&brief).unwrap();
        let message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "continue".into(),
            },
        );
        let config = ContextConfig {
            recent_messages: 0,
            recent_briefs: 1,
            ..Default::default()
        };
        let execution = super::super::tests::execution_snapshot_for(&agent);
        let mut snapshot = capture_context_from_storage(
            &storage,
            &agent,
            &execution,
            &SkillsRuntimeView::default(),
            &message,
            None,
            &config,
            dir.path(),
            None,
        )
        .unwrap();
        let budget =
            InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default()).unwrap();
        let data = Arc::get_mut(&mut snapshot.data).unwrap();
        let history = Arc::get_mut(&mut data.history).unwrap();
        assert!(history.observations.brief_text[&brief.id]
            .as_str()
            .contains(&source_text));
        // The resolved holder retains its source even when the transcript
        // record is no longer in the selected history.
        history.transcript.clear();
        data.input_budget = Some((
            budget.clone(),
            InputEvidenceScope {
                agent_id: "default".into(),
                identity_incarnation: 1,
            },
        ));
        data.observe_owned_input_bytes().unwrap();
        let usage = budget.report().unwrap();
        assert!(usage.owned.payload_bytes >= source_text.len() as u64);
        assert_eq!(
            usage.categories[&InputMemoryCategory::PreparedPrompt].allocations,
            1
        );
        let clone = snapshot.clone();
        assert!(Arc::ptr_eq(&snapshot.data, &clone.data));
        drop((snapshot, storage));
        assert_eq!(
            budget.report().unwrap().owned.payload_bytes,
            usage.owned.payload_bytes
        );
        drop(clone);
        assert_eq!(budget.report().unwrap().observed_bytes, 0);
    }

    #[test]
    fn copied_work_payload_is_metered_until_the_snapshot_releases_it() {
        use crate::runtime::{
            input_evidence_budget::InputEvidenceBudgetLedger,
            resident_input_evidence::InputEvidenceScope,
        };
        let dir = tempfile::tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
        let objective = "resident-work-body".repeat(8192);
        let work = WorkItemRecord::new("default", objective.clone(), WorkItemState::Open);
        storage.append_work_item(&work).unwrap();
        let mut agent = AgentState::new("default");
        agent.current_work_item_id = Some(work.id.clone());
        storage.write_agent_for_fixture(&agent).unwrap();
        let message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "continue".into(),
            },
        );
        let config = ContextConfig::default();
        let execution = super::super::tests::execution_snapshot_for(&agent);
        let mut snapshot = capture_context_from_storage(
            &storage,
            &agent,
            &execution,
            &SkillsRuntimeView::default(),
            &message,
            None,
            &config,
            dir.path(),
            None,
        )
        .unwrap();
        let budget =
            InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default()).unwrap();
        let data = Arc::get_mut(&mut snapshot.data).unwrap();
        assert_eq!(
            data.current_work_item.as_ref().unwrap().objective,
            objective
        );
        data.input_budget = Some((
            budget.clone(),
            InputEvidenceScope {
                agent_id: "default".into(),
                identity_incarnation: 1,
            },
        ));
        data.observe_owned_input_bytes().unwrap();
        assert!(budget.report().unwrap().owned.payload_bytes >= objective.len() as u64);
        drop((storage, snapshot));
        assert_eq!(budget.report().unwrap().observed_bytes, 0);
    }

    #[test]
    fn descriptors_keep_current_revision_and_completion_sources_outside_history() {
        let dir = tempfile::tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
        let work = WorkItemRecord::new("default", "current", WorkItemState::Open);
        let mut completed = WorkItemRecord::new("default", "finished", WorkItemState::Completed);
        let mut result = BriefRecord::new(
            "default",
            BriefKind::Result,
            "cold completion",
            Some("message_completion_source".into()),
            None,
        );
        result.work_item_id = Some(completed.id.clone());
        result.content_source = BriefContentSource::TranscriptEntry {
            entry_id: "transcript_completion_source".into(),
            relation: BriefContentSourceRelation::Finalizes,
        };
        completed.result_brief_id = Some(result.id.clone());
        storage.append_work_item(&completed).unwrap();
        storage.append_brief(&result).unwrap();
        storage.append_work_item(&work).unwrap();
        let mut agent = AgentState::new("default");
        agent.current_work_item_id = Some(work.id.clone());
        storage.write_agent_for_fixture(&agent).unwrap();
        let execution = super::super::tests::execution_snapshot_for(&agent);
        let message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "continue".into(),
            },
        );
        let config = ContextConfig {
            recent_briefs: 0,
            ..ContextConfig::default()
        };
        let mut snapshot = capture_context_from_storage(
            &storage,
            &agent,
            &execution,
            &SkillsRuntimeView::default(),
            &message,
            None,
            &config,
            dir.path(),
            None,
        )
        .unwrap();
        assert!(snapshot.data.history.briefs.is_empty());
        let full =
            build_context_from_snapshot(&snapshot, config.prompt_budget_estimated_tokens).unwrap();
        assert!(full.sections.iter().any(|section| section
            .content
            .contains(&format!("Result ref: brief:{}", result.id))));
        let descriptor = snapshot
            .descriptors()
            .iter()
            .find(|item| item.reference == format!("brief:{}", result.id))
            .unwrap();
        assert_eq!(descriptor.content_source, Some(result.content_source));
        assert_eq!(
            descriptor.source_refs,
            vec!["message:message_completion_source"]
        );
        assert_eq!(descriptor.work_item_id, Some(completed.id));

        // Reproduce sequential reads returning an older queue and newer current record.
        let data = Arc::get_mut(&mut snapshot.data).unwrap();
        assert!(candidate_items(&data.work_queue_projection).any(|item| item.id == work.id));
        let current = data.current_work_item.as_mut().unwrap();
        assert_eq!(current.id, work.id);
        current.revision += 1;
        let revision = current.revision;
        let result = descriptors(data, false);
        let descriptor = result
            .iter()
            .find(|item| item.reference == format!("work_item:{}", work.id))
            .unwrap();
        assert_eq!(descriptor.revision, Some(revision));
        let plan = data
            .resource_views
            .owned_plans_mut()
            .remove(&work.id)
            .unwrap();
        data.current_work_item.as_mut().unwrap().plan_artifact = Some(plan.clone());
        let cold = descriptors(data, false);
        let loaded = descriptors(data, true);
        assert_eq!(
            cold.iter()
                .find(|item| item.reference == format!("work_item:{}", work.id))
                .unwrap()
                .body_digest
                .as_deref(),
            Some(plan.hash.as_str())
        );
        let unobserved = loaded
            .iter()
            .find(|item| item.reference == format!("work_item:{}", work.id))
            .unwrap();
        assert!(unobserved.body_locator.is_none());
        assert!(unobserved.body_digest.is_none());
    }
}

#[cfg(test)]
mod coherent_acquisition_tests {
    use super::*;
    use crate::{
        domain::execution_protocol::ExecutionProtocolState,
        projection_eval::HistorySelector,
        runtime::mutation_owner::PublicationToken,
        runtime_db::context_inputs::ContextDurableRead,
        types::{
            BriefContentSource, BriefContentSourceRelation, BriefKind, DeliverySummaryRecord,
            Priority, TurnOwner,
        },
    };
    use std::cell::{Cell, RefCell};

    fn message(text: &str) -> MessageEnvelope {
        MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text { text: text.into() },
        )
    }
    fn captured(agent: &AgentState, limit: usize) -> CapturedAgentContext {
        CapturedAgentContext {
            input_evidence: None,
            work: None,
            tasks: None,
            scheduling: None,
            token: PublicationToken {
                identity_incarnation: 1,
                owner_epoch: 1,
                generation: 1,
            },
            state: Arc::new(agent.clone()),
            episode_candidate_limit: limit,
            episodes: vec![],
        }
    }
    fn append_turn(storage: &AppStorage, id: &str, index: u64, work: &WorkItemRecord, text: &str) {
        let mut input = message(text);
        input.turn_id = Some(id.into());
        input.work_item_id = Some(work.id.clone());
        storage.append_message(&input).unwrap();
        let mut entry = TranscriptEntry::new(
            "default",
            TranscriptEntryKind::AssistantRound,
            Some(1),
            None,
            serde_json::json!({"turn_id":id,"blocks":[{"type":"text","text":text}]}),
        );
        entry.created_at =
            chrono::DateTime::from_timestamp(i64::try_from(index).unwrap(), 0).unwrap();
        storage.append_transcript_entry(&entry).unwrap();
        let mut brief =
            BriefRecord::new("default", BriefKind::Result, "stored fallback", None, None);
        brief.turn_id = Some(id.into());
        brief.work_item_id = Some(work.id.clone());
        brief.content_source = BriefContentSource::TranscriptEntry {
            entry_id: entry.id.clone(),
            relation: BriefContentSourceRelation::Finalizes,
        };
        storage.append_brief(&brief).unwrap();
        let mut turn = TurnRecord::new("default", id, index);
        turn.created_at = entry.created_at;
        turn.owner = Some(TurnOwner::WorkItem {
            work_item_id: work.id.clone(),
        });
        turn.current_work_item_id = Some(work.id.clone());
        turn.input_message_ids.push(input.id.clone());
        turn.produced_brief_ids.push(brief.id);
        turn.trigger = Some(crate::types::TurnTriggerSummary::from_message(&input));
        storage.append_turn(&turn).unwrap();
    }
    struct Resources {
        calls: Cell<usize>,
        notes: RefCell<String>,
    }
    impl ContextResources for Resources {
        fn plan(&self, _: &WorkItemRecord) -> Result<Option<WorkItemPlanArtifact>> {
            self.calls.set(self.calls.get() + 1);
            Ok(None)
        }
        fn notes(&self) -> Option<String> {
            self.calls.set(self.calls.get() + 1);
            Some(self.notes.borrow().clone())
        }
    }

    #[tokio::test]
    async fn completion_snapshot_handoff_keeps_phase_through_pure_builder_and_final_clone(
    ) -> Result<()> {
        use crate::{
            config::{AppConfig, StateMemoryLimits},
            diagnostics::resource_io::ResourceReadMaterial,
            host::RuntimeHost,
            runtime::{
                input_evidence_budget::InputEvidenceBudgetLedger,
                resident_input_evidence::InputEvidenceScope, tests::support::StubProvider,
            },
        };
        let home = tempfile::tempdir()?;
        std::fs::write(
            home.path().join("config.json"),
            r#"{"model":{"default":"openai/gpt-5.4"}}"#,
        )?;
        let mut host_config = AppConfig::load_with_home(Some(home.path().into()))?;
        host_config.user_home_dir = None;
        host_config.default_agent_id = "default".into();
        let host =
            RuntimeHost::new_with_provider(host_config, Arc::new(StubProvider::new("unused")))?;
        let runtime = host.default_runtime().await?;
        let work = runtime
            .create_work_item(
                "snapshot completion".into(),
                Some(crate::types::WorkItemPlanStatus::NeedsInput),
                None,
                Vec::new(),
            )
            .await?;
        let completed = runtime
            .complete_work_item_with_report(
                work.id.clone(),
                crate::runtime::WorkItemCompletionAuthority::Control,
                "ORIGINAL_SNAPSHOT_COMPLETION".into(),
                Vec::new(),
                None,
                None,
                None,
                None,
                None,
                None,
                Vec::new(),
            )
            .await?
            .into_record();
        let observation = runtime.refresh_context_resources().await?;
        let dir = tempfile::tempdir()?;
        let storage = AppStorage::new_for_test(dir.path())?;
        let agent = AgentState::new("default");
        storage.write_agent_for_fixture(&agent)?;
        storage.append_work_item(&work)?;
        storage.append_work_item(&completed)?;
        let mut brief = BriefRecord::new(
            "default",
            BriefKind::Result,
            "ORIGINAL_SNAPSHOT_COMPLETION",
            None,
            None,
        );
        brief.id = completed.result_brief_id.clone().unwrap();
        brief.work_item_id = Some(completed.id.clone());
        storage.append_brief(&brief)?;
        let message = message("inspect snapshot");
        let execution = super::super::tests::execution_snapshot_for(&agent);
        let config = ContextConfig::default();
        let captured = captured(&agent, config.recent_episode_candidates);
        let db = storage.runtime_db()?.unwrap();
        let mut connection = db.connection()?;
        let tx = connection.transaction()?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::new(
            &ledger,
            &InputEvidenceScope {
                agent_id: "default".into(),
                identity_incarnation: 1,
            },
        )?;
        let snapshot = {
            let _active = phase.enter();
            crate::diagnostics::resource_io::admit_cloned_value(&execution)?;
            let owned_execution = execution.clone();
            let original_cwd = owned_execution.cwd.as_os_str().as_encoded_bytes().as_ptr();
            let durable = acquire_durable_context_with_work_queue(
                &ContextDurableRead::new(&tx, "default"),
                &agent,
                Cow::Owned(owned_execution),
                ContextSnapshotSkills::Observed(observation.clone()),
                &message,
                None,
                &config,
                None,
                ContextHistoryPreparation::default(),
                &captured,
                None,
                None,
            )?;
            assert_eq!(
                durable
                    .data
                    .execution
                    .cwd
                    .as_os_str()
                    .as_encoded_bytes()
                    .as_ptr(),
                original_cwd,
                "loaded execution moves its admitted allocation into Context"
            );
            assert!(Arc::ptr_eq(&durable.data.agent, &captured.state));
            assert!(std::ptr::eq(
                durable.data.skills.view(),
                &observation.body().skills
            ));
            assert_eq!(
                durable.data.completion_reports[&completed.id].text,
                "ORIGINAL_SNAPSHOT_COMPLETION"
            );
            durable.observe_loaded(observation, phase.guard()?)?
        };
        phase.finish()?;
        tx.commit()?;
        let original = Arc::downgrade(snapshot.resource_allocation().unwrap());
        let original_skills = match &snapshot.data.skills {
            ContextSnapshotSkills::Observed(observation) => Arc::downgrade(observation),
            _ => panic!("loaded snapshot copied its resource skills"),
        };
        let identity = crate::types::AgentIdentityView::from_record(
            &crate::types::AgentIdentityRecord::new(
                "default",
                crate::types::AgentKind::Default,
                crate::types::AgentVisibility::Public,
                crate::types::AgentOwnership::SelfOwned,
                crate::types::AgentProfilePreset::PublicNamed,
                None,
                None,
            ),
            "default",
        );
        let prompt = crate::prompt::build_effective_prompt_from_context_snapshot(
            &snapshot,
            dir.path(),
            dir.path(),
            &identity,
            crate::types::LoadedAgentsMd::default(),
            crate::types::LoadedAgentMemory::default(),
            &[],
            crate::prompt::ToolPromptContext::default(),
        )?;
        let clone = prompt.clone();
        host.shutdown().await?;
        drop((snapshot, phase, storage, connection, runtime, host));
        assert!(original.upgrade().is_some());
        assert!(original_skills.upgrade().is_some());
        let before = ledger.report()?.observed_bytes;
        drop(prompt);
        assert!(original.upgrade().is_none());
        assert!(ledger.report()?.observed_bytes < before);
        assert!(clone
            .rendered_context_attachment
            .contains("ORIGINAL_SNAPSHOT_COMPLETION"));
        drop(clone);
        assert!(original_skills.upgrade().is_none());
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }

    #[test]
    fn both_history_windows_and_brief_dependencies_use_one_view_then_select_without_capabilities() {
        let directory = tempfile::tempdir().unwrap();
        let storage = AppStorage::new_for_test(directory.path()).unwrap();
        let work_a = WorkItemRecord::new("default", "owner A", crate::types::WorkItemState::Open);
        let work_b = WorkItemRecord::new("default", "owner B", crate::types::WorkItemState::Open);
        storage.append_work_item(&work_a).unwrap();
        storage.append_work_item(&work_b).unwrap();
        let current_message = message("current mandatory root");
        let mut agent = AgentState::new("default");
        agent.current_work_item_id = Some(work_a.id.clone());
        agent.current_execution_binding = Some(crate::types::WorkItemExecutionBinding {
            activation_id: Some("fixture-activation".into()),
            admission_provenance: None,
            source_message_id: current_message.id.clone(),
            turn_id: "fixture-current-turn".into(),
            owner: Some(TurnOwner::WorkItem {
                work_item_id: work_a.id.clone(),
            }),
            work_item_id: Some(work_a.id.clone()),
            claimed_work_revision: Some(work_a.revision),
        });
        storage.write_agent_for_fixture(&agent).unwrap();
        append_turn(&storage, "turn-a", 1, &work_a, "owner-A-original-body");
        append_turn(&storage, "turn-b", 2, &work_b, "agent-tail-B-original-body");
        let config = ContextConfig {
            recent_messages: 1,
            recent_briefs: 0,
            recent_episode_candidates: 2,
            prompt_budget_estimated_tokens: 64_000,
            ..ContextConfig::default()
        };
        let history_policy = ContextHistoryPreparation {
            selector: HistorySelector::WorkItemScoped,
            work_item_scoped_window: Some(64),
            window_source: "env",
        };
        let execution = super::super::tests::execution_snapshot_for(&agent);
        let resident = captured(&agent, 2);
        let db = storage.runtime_db().unwrap().unwrap();
        let mut connection = db.connection().unwrap();
        let read = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)
            .unwrap();
        // The next commit changes canonical turn/brief/transcript evidence while
        // the reader's first SELECT pins the older WAL view.
        let _: i64 = read
            .query_row("SELECT COUNT(*) FROM agent_states", [], |row| row.get(0))
            .unwrap();
        append_turn(
            &storage,
            "turn-a-late",
            3,
            &work_a,
            "late-A-must-not-enter-snapshot",
        );
        let durable = acquire_durable_context(
            &ContextDurableRead::new(&read, "default"),
            &agent,
            &execution,
            &SkillsRuntimeView::default(),
            &current_message,
            None,
            &config,
            None,
            history_policy,
            &resident,
        )
        .unwrap();
        assert_eq!(durable.data.history.turn_records[0].turn_id, "turn-b");
        assert_eq!(
            durable
                .data
                .scoped_history
                .as_ref()
                .unwrap()
                .history
                .turn_records[0]
                .turn_id,
            "turn-a"
        );
        read.commit().unwrap();
        drop(connection);
        let resources = Resources {
            calls: Cell::new(0),
            notes: RefCell::new("notes-first-observation".into()),
        };
        let snapshot = observe_context_resources(durable, &resources, 7).unwrap();
        let calls = resources.calls.get();
        *resources.notes.borrow_mut() = "late-file-must-not-enter-snapshot".into();
        let built =
            build_context_from_snapshot(&snapshot, config.prompt_budget_estimated_tokens).unwrap();
        let reprojection = built.recent_turns_reprojection.unwrap();
        let scoped = reprojection.select_scoped(64).unwrap();
        let scoped_render = reproject_recent_turns(&scoped, 64_000).unwrap().content;
        assert!(scoped_render.contains("owner-A-original-body"));
        assert!(!scoped_render.contains("late-A-must-not-enter-snapshot"));
        let recent = scoped.select_recent();
        let recent_render = reproject_recent_turns(&recent, 64_000).unwrap().content;
        assert!(recent_render.contains("agent-tail-B-original-body"));
        assert!(!recent_render.contains("late-A-must-not-enter-snapshot"));
        assert_eq!(scoped.snapshot_id(), recent.snapshot_id());
        assert_eq!(snapshot.history_policy(), history_policy);
        assert_eq!(snapshot.resources().generation, 7);
        assert!(!snapshot.resources().digest.is_empty());
        assert!(built
            .sections
            .iter()
            .any(|section| section.content.contains("notes-first-observation")));
        assert!(!built.sections.iter().any(|section| section
            .content
            .contains("late-file-must-not-enter-snapshot")));
        assert_eq!(resources.calls.get(), calls);
        assert!(scoped.select_scoped(128).is_err());
        let identity = crate::types::AgentIdentityView::from_record(
            &crate::types::AgentIdentityRecord::new(
                "default",
                crate::types::AgentKind::Default,
                crate::types::AgentVisibility::Public,
                crate::types::AgentOwnership::SelfOwned,
                crate::types::AgentProfilePreset::PublicNamed,
                None,
                None,
            ),
            "default",
        );
        let prompt = crate::prompt::build_effective_prompt_from_context_snapshot(
            &snapshot,
            directory.path(),
            directory.path(),
            &identity,
            crate::types::LoadedAgentsMd::default(),
            crate::types::LoadedAgentMemory::default(),
            &[],
            crate::prompt::ToolPromptContext::default(),
        )
        .unwrap();
        assert_eq!(prompt.history_policy(), history_policy);
        assert_eq!(
            prompt.frozen_projection_budget(),
            Some(config.turn_projection_budget())
        );
        assert_eq!(prompt.frozen_recent_message_limit(), Some(1));
        let selected_prompt = |outcome| match outcome {
            crate::prompt::HistoryReprojectionOutcome::Adopted { prompt, .. }
            | crate::prompt::HistoryReprojectionOutcome::IdenticalRenderNoop { prompt, .. } => {
                prompt
            }
            _ => panic!("the fixture has both history windows"),
        };
        let scoped_prompt = selected_prompt(
            prompt
                .reproject_for_history_selector(64_000, &[], HistorySelector::WorkItemScoped, 64)
                .unwrap(),
        );
        assert!(scoped_prompt
            .rendered_context_attachment
            .contains("owner-A-original-body"));
        let recent_prompt = selected_prompt(
            scoped_prompt
                .reproject_for_history_selector(64_000, &[], HistorySelector::RecentTurns, 64)
                .unwrap(),
        );
        assert!(recent_prompt
            .rendered_context_attachment
            .contains("agent-tail-B-original-body"));
        let evidence = recent_prompt
            .projection_evidence
            .get("recent_turns")
            .unwrap();
        assert!(evidence
            .iter()
            .any(|entry| entry.reference == "turn:turn-b"));
        assert!(!evidence
            .iter()
            .any(|entry| entry.reference == "turn:turn-a"));
        assert_eq!(
            scoped_prompt.context_snapshot_id(),
            recent_prompt.context_snapshot_id()
        );
        let retry = [512, 128, 64, 16, 1]
            .into_iter()
            .find_map(|budget| scoped_prompt.reproject_recent_turns(budget, &[]))
            .expect("a smaller budget eventually changes the history attachment");
        let selected_retry = retry.recent_turns_reprojection.as_ref().unwrap();
        let frozen_retry = reproject_recent_turns(selected_retry, 64_000)
            .unwrap()
            .content;
        assert!(frozen_retry.contains("owner-A-original-body"));
        assert!(!frozen_retry.contains("agent-tail-B-original-body"));
        assert!(!frozen_retry.contains("late-A-must-not-enter-snapshot"));
        assert_eq!(
            retry.context_snapshot_id(),
            scoped_prompt.context_snapshot_id()
        );
        assert_eq!(resources.calls.get(), calls);
        // A subsequent explicit acquisition, unlike a selector/budget retry,
        // sees the new evidence and remains the only place with SQL capability.
        let mut connection = db.connection().unwrap();
        let read = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)
            .unwrap();
        let fresh = acquire_durable_context(
            &ContextDurableRead::new(&read, "default"),
            &agent,
            &execution,
            &SkillsRuntimeView::default(),
            &current_message,
            None,
            &config,
            None,
            history_policy,
            &resident,
        )
        .unwrap();
        assert_eq!(fresh.data.history.turn_records[0].turn_id, "turn-a-late");
        assert_eq!(
            fresh
                .data
                .scoped_history
                .as_ref()
                .unwrap()
                .history
                .turn_records
                .len(),
            2
        );
    }

    struct NoReads;
    #[allow(unused_variables)]
    impl ContextInputs for NoReads {
        fn anchor_messages(&self) -> Result<Vec<MessageEnvelope>> {
            panic!("durable reads must not precede resident scope validation")
        }
        fn message_by_id(&self, id: &str) -> Result<Option<MessageEnvelope>> {
            panic!("durable reads must not precede resident scope validation")
        }
        fn assigned_turn_for_message(&self, id: &str) -> Result<Option<String>> {
            panic!("durable reads must not precede resident scope validation")
        }
        fn briefs(&self, limit: usize) -> Result<Vec<BriefRecord>> {
            panic!("durable reads must not precede resident scope validation")
        }
        fn brief_by_id(&self, id: &str) -> Result<Option<BriefRecord>> {
            panic!("durable reads must not precede resident scope validation")
        }
        fn latest_completion_brief_for_work_item(
            &self,
            id: &str,
        ) -> Result<Option<crate::runtime_db::context_inputs::CompletionBriefInput<'_>>> {
            panic!("durable reads must not precede resident scope validation")
        }
        fn admitted_settlement_inputs(
            &self,
            agent_id: &str,
            activation_id: &str,
            current_message_id: &str,
            reply_limit: usize,
        ) -> Result<Vec<crate::context::inputs::AdmittedSettlementInput>> {
            panic!("durable reads must not precede resident scope validation")
        }
        fn tools(&self, limit: usize) -> Result<Vec<ToolContextProjection>> {
            panic!("durable reads must not precede resident scope validation")
        }
        fn tool_by_id(&self, id: &str) -> Result<Option<ToolContextProjection>> {
            panic!("durable reads must not precede resident scope validation")
        }
        fn turns(&self, limit: usize) -> Result<Vec<TurnRecord>> {
            panic!("durable reads must not precede resident scope validation")
        }
        fn turns_for_owner(&self, owner: &TurnOwner, limit: usize) -> Result<Vec<TurnRecord>> {
            panic!("durable reads must not precede resident scope validation")
        }
        fn turn_by_id(&self, id: &str) -> Result<Option<TurnRecord>> {
            panic!("durable reads must not precede resident scope validation")
        }
        fn transcript(&self, limit: usize) -> Result<Vec<TranscriptEntry>> {
            panic!("durable reads must not precede resident scope validation")
        }
        fn transcript_for_turns(&self, ids: &[String]) -> Result<Vec<TranscriptEntry>> {
            panic!("durable reads must not precede resident scope validation")
        }
        fn transcript_by_id(&self, id: &str) -> Result<Option<TranscriptEntry>> {
            panic!("durable reads must not precede resident scope validation")
        }
        fn execution_protocol_state(
            &self,
            agent_id: &str,
        ) -> Result<Option<ExecutionProtocolState>> {
            panic!("durable reads must not precede resident scope validation")
        }
        fn active_waits(&self, agent_id: &str) -> Result<Vec<WaitConditionRecord>> {
            panic!("durable reads must not precede resident scope validation")
        }
        fn work_queue(&self) -> Result<WorkQueueReadModel> {
            panic!("durable reads must not precede resident scope validation")
        }
        fn work_item(&self, id: &str) -> Result<Option<WorkItemRecord>> {
            panic!("durable reads must not precede resident scope validation")
        }
        fn external_triggers(&self) -> Result<Vec<ExternalTriggerRecord>> {
            panic!("durable reads must not precede resident scope validation")
        }
        /// Existing active_for_agent ordering, unbounded so count and display share a view.
        fn active_tasks(&self, agent_id: &str) -> Result<Vec<TaskRecord>> {
            panic!("durable reads must not precede resident scope validation")
        }
        fn delivery_summaries(&self, limit: usize) -> Result<Vec<DeliverySummaryRecord>> {
            panic!("durable reads must not precede resident scope validation")
        }
        fn admitted_settlements(
            &self,
            agent_id: &str,
            activation_id: &str,
        ) -> Result<Vec<TaskResultSettlementRecord>> {
            panic!("durable reads must not precede resident scope validation")
        }
    }

    #[test]
    fn owned_current_input_admission_precedes_all_durable_reads() -> Result<()> {
        use crate::{
            config::StateMemoryLimits,
            diagnostics::resource_io::ResourceReadMaterial,
            runtime::{
                input_evidence_budget::InputEvidenceBudgetLedger,
                resident_input_evidence::InputEvidenceScope,
            },
        };
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let scope = InputEvidenceScope {
            agent_id: "default".into(),
            identity_incarnation: 1,
        };
        for field in 0..3 {
            let agent = AgentState::new("default");
            let mut execution = super::super::tests::execution_snapshot_for(&agent);
            let mut config = ContextConfig::default();
            let mut current = message("current");
            let large = "x".repeat(65_536);
            match field {
                0 => current.body = MessageBody::Text { text: large },
                1 => execution.cwd = large.into(),
                _ => config.callback_base_url = large,
            }
            let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, 1_000_000, 65_536)?;
            {
                let _active = phase.enter();
                let result = acquire_durable_context(
                    &NoReads,
                    &agent,
                    &execution,
                    &SkillsRuntimeView::default(),
                    &current,
                    None,
                    &config,
                    None,
                    ContextHistoryPreparation::default(),
                    &captured(&agent, config.recent_episode_candidates),
                );
                assert!(result
                    .err()
                    .unwrap()
                    .to_string()
                    .contains("ResourceExhausted"));
            }
            assert!(phase.finish().is_err());
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        Ok(())
    }

    #[test]
    fn scoped_selector_without_frozen_window_fails_before_any_durable_query() {
        let agent = AgentState::new("default");
        let execution = super::super::tests::execution_snapshot_for(&agent);
        let config = ContextConfig::default();
        let result = acquire_durable_context(
            &NoReads,
            &agent,
            &execution,
            &SkillsRuntimeView::default(),
            &message("root"),
            None,
            &config,
            None,
            ContextHistoryPreparation {
                selector: HistorySelector::WorkItemScoped,
                work_item_scoped_window: None,
                window_source: "invalid",
            },
            &captured(&agent, config.recent_episode_candidates),
        );
        assert!(result.is_err());
    }

    #[test]
    fn both_resident_policy_limit_mismatches_fail_before_any_durable_query() {
        let agent = AgentState::new("default");
        let execution = super::super::tests::execution_snapshot_for(&agent);
        let current_message = message("root");
        for (capture_limit, request_limit) in [(12, 4), (4, 12)] {
            let resident = captured(&agent, capture_limit);
            let config = ContextConfig {
                recent_episode_candidates: request_limit,
                ..ContextConfig::default()
            };
            let result = acquire_durable_context(
                &NoReads,
                &agent,
                &execution,
                &SkillsRuntimeView::default(),
                &current_message,
                None,
                &config,
                None,
                ContextHistoryPreparation::default(),
                &resident,
            );
            assert!(result.is_err());
        }
    }
}
