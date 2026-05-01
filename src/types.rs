use chrono::{DateTime, Utc};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use uuid::Uuid;

use crate::config::ModelRef;
use crate::model_catalog::{BuiltInModelMetadata, ResolvedRuntimeModelPolicy};
use crate::system::{
    ExecutionProfile, ExecutionSnapshot, WorkspaceAccessMode, WorkspaceProjectionKind,
};

pub const AGENT_HOME_WORKSPACE_ID: &str = "agent_home";

fn default_agent_home_workspace_id() -> String {
    AGENT_HOME_WORKSPACE_ID.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceEntry {
    pub workspace_id: String,
    pub workspace_anchor: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl WorkspaceEntry {
    pub fn new(
        workspace_id: impl Into<String>,
        workspace_anchor: PathBuf,
        repo_name: Option<String>,
    ) -> Self {
        let now = Utc::now();
        Self {
            workspace_id: workspace_id.into(),
            workspace_anchor,
            repo_name,
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActiveWorkspaceEntry {
    pub workspace_id: String,
    pub workspace_anchor: PathBuf,
    pub execution_root_id: String,
    pub execution_root: PathBuf,
    pub projection_kind: WorkspaceProjectionKind,
    pub access_mode: WorkspaceAccessMode,
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occupancy_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection_metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceOccupancyRecord {
    pub occupancy_id: String,
    pub execution_root_id: String,
    pub workspace_id: String,
    pub holder_agent_id: String,
    pub access_mode: WorkspaceAccessMode,
    pub acquired_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Default,
    Named,
    Child,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentVisibility {
    Public,
    Private,
}

impl AgentVisibility {
    pub fn label(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Private => "private",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentOwnership {
    ParentSupervised,
    SelfOwned,
}

impl AgentOwnership {
    pub fn label(self) -> &'static str {
        match self {
            Self::ParentSupervised => "parent_supervised",
            Self::SelfOwned => "self_owned",
        }
    }

    pub fn phrase(self) -> &'static str {
        match self {
            Self::ParentSupervised => "parent-supervised",
            Self::SelfOwned => "self-owned",
        }
    }

    pub fn cleanup_summary(self) -> &'static str {
        match self {
            Self::ParentSupervised => "cleanup is owned by the parent supervision tree",
            Self::SelfOwned => "cleanup is owned by the agent's own lifecycle surface",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentProfilePreset {
    PrivateChild,
    PublicNamed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ToolCapabilityFamily {
    CoreAgent,
    LocalEnvironment,
    AgentCreation,
    AuthorityExpanding,
    ExternalTrigger,
}

impl ToolCapabilityFamily {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::CoreAgent => "core_agent",
            Self::LocalEnvironment => "local_environment",
            Self::AgentCreation => "agent_creation",
            Self::AuthorityExpanding => "authority_expanding",
            Self::ExternalTrigger => "external_trigger",
        }
    }
}

impl AgentProfilePreset {
    pub fn label(self) -> &'static str {
        match self {
            Self::PrivateChild => "private_child",
            Self::PublicNamed => "public_named",
        }
    }

    pub fn spawn_surface_summary(self) -> &'static str {
        match self {
            Self::PrivateChild => {
                "SpawnAgent returns both `agent_id` and a supervising `task_handle`"
            }
            Self::PublicNamed => "SpawnAgent returns `agent_id` only",
        }
    }

    pub(crate) fn allows_tool_capability_family(self, family: ToolCapabilityFamily) -> bool {
        match self {
            Self::PrivateChild => matches!(
                family,
                ToolCapabilityFamily::CoreAgent
                    | ToolCapabilityFamily::LocalEnvironment
                    | ToolCapabilityFamily::ExternalTrigger
            ),
            Self::PublicNamed => matches!(
                family,
                ToolCapabilityFamily::CoreAgent
                    | ToolCapabilityFamily::LocalEnvironment
                    | ToolCapabilityFamily::AgentCreation
                    | ToolCapabilityFamily::AuthorityExpanding
                    | ToolCapabilityFamily::ExternalTrigger
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentDurability {
    Persistent,
    Ephemeral,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentRegistryStatus {
    Active,
    Archived,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentIdentityRecord {
    pub agent_id: String,
    pub kind: AgentKind,
    pub visibility: AgentVisibility,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ownership: Option<AgentOwnership>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_preset: Option<AgentProfilePreset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub durability: Option<AgentDurability>,
    pub status: AgentRegistryStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineage_parent_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_from_task_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<DateTime<Utc>>,
}

impl AgentIdentityRecord {
    pub fn new(
        agent_id: impl Into<String>,
        kind: AgentKind,
        visibility: AgentVisibility,
        ownership: AgentOwnership,
        profile_preset: AgentProfilePreset,
        parent_agent_id: Option<String>,
        delegated_from_task_id: Option<String>,
    ) -> Self {
        let now = Utc::now();
        Self {
            agent_id: agent_id.into(),
            kind,
            visibility,
            ownership: Some(ownership),
            profile_preset: Some(profile_preset),
            durability: None,
            status: AgentRegistryStatus::Active,
            parent_agent_id,
            lineage_parent_agent_id: None,
            delegated_from_task_id,
            created_at: now,
            updated_at: now,
            archived_at: None,
        }
    }

    pub fn with_lineage_parent_agent_id(mut self, lineage_parent_agent_id: Option<String>) -> Self {
        self.lineage_parent_agent_id = lineage_parent_agent_id;
        self
    }

    pub fn ownership(&self) -> AgentOwnership {
        self.ownership
            .or_else(|| {
                self.durability.map(|durability| match durability {
                    AgentDurability::Persistent => AgentOwnership::SelfOwned,
                    AgentDurability::Ephemeral => AgentOwnership::ParentSupervised,
                })
            })
            .unwrap_or_else(|| {
                if self.kind == AgentKind::Child
                    || self.parent_agent_id.is_some()
                    || self.delegated_from_task_id.is_some()
                {
                    AgentOwnership::ParentSupervised
                } else {
                    AgentOwnership::SelfOwned
                }
            })
    }

    pub fn profile_preset(&self) -> AgentProfilePreset {
        self.profile_preset.unwrap_or_else(|| {
            match (self.kind, self.visibility, self.ownership()) {
                (_, AgentVisibility::Public, AgentOwnership::SelfOwned) => {
                    AgentProfilePreset::PublicNamed
                }
                _ => AgentProfilePreset::PrivateChild,
            }
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentIdentityView {
    pub agent_id: String,
    pub kind: AgentKind,
    pub visibility: AgentVisibility,
    pub ownership: AgentOwnership,
    pub profile_preset: AgentProfilePreset,
    pub status: AgentRegistryStatus,
    pub is_default_agent: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineage_parent_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_from_task_id: Option<String>,
}

impl AgentIdentityView {
    pub fn from_record(record: &AgentIdentityRecord, default_agent_id: &str) -> Self {
        Self {
            agent_id: record.agent_id.clone(),
            kind: record.kind,
            visibility: record.visibility,
            ownership: record.ownership(),
            profile_preset: record.profile_preset(),
            status: record.status,
            is_default_agent: record.agent_id == default_agent_id,
            parent_agent_id: record.parent_agent_id.clone(),
            lineage_parent_agent_id: record.lineage_parent_agent_id.clone(),
            delegated_from_task_id: record.delegated_from_task_id.clone(),
        }
    }

    pub fn contract_badge(&self) -> String {
        format!(
            "{}/{} ({})",
            self.visibility.label(),
            self.ownership.label(),
            self.profile_preset.label()
        )
    }

    pub fn contract_summary(&self) -> String {
        match (
            self.visibility,
            self.ownership,
            self.profile_preset,
            self.kind,
        ) {
            (
                AgentVisibility::Public,
                AgentOwnership::SelfOwned,
                AgentProfilePreset::PublicNamed,
                _,
            ) => "public self-owned agent addressed directly by `agent_id`".into(),
            (
                AgentVisibility::Private,
                AgentOwnership::ParentSupervised,
                AgentProfilePreset::PrivateChild,
                AgentKind::Child,
            ) => "private parent-supervised child that remains under a parent task handle".into(),
            _ => format!(
                "{} {} agent with `{}` profile",
                self.visibility.label(),
                self.ownership.phrase(),
                self.profile_preset.label()
            ),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChildAgentSummary {
    pub identity: AgentIdentityView,
    pub status: AgentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_run_id: Option<String>,
    pub pending: usize,
    pub active_task_count: usize,
    pub observability: ChildAgentObservabilitySnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentsMdScope {
    Agent,
    Workspace,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentsMdKind {
    AgentsMd,
    ClaudeMdFallback,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentsMdSource {
    pub scope: AgentsMdScope,
    pub kind: AgentsMdKind,
    pub path: PathBuf,
    #[serde(default, skip_serializing, skip_deserializing)]
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct LoadedAgentsMd {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_source: Option<AgentsMdSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_source: Option<AgentsMdSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentsMdSourceView {
    pub scope: AgentsMdScope,
    pub kind: AgentsMdKind,
    pub path: PathBuf,
}

impl From<&AgentsMdSource> for AgentsMdSourceView {
    fn from(value: &AgentsMdSource) -> Self {
        Self {
            scope: value.scope.clone(),
            kind: value.kind.clone(),
            path: value.path.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct LoadedAgentsMdView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_source: Option<AgentsMdSourceView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_source: Option<AgentsMdSourceView>,
}

impl From<&LoadedAgentsMd> for LoadedAgentsMdView {
    fn from(value: &LoadedAgentsMd) -> Self {
        Self {
            agent_source: value.agent_source.as_ref().map(AgentsMdSourceView::from),
            workspace_source: value
                .workspace_source
                .as_ref()
                .map(AgentsMdSourceView::from),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SkillScope {
    User,
    Agent,
    Workspace,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillCatalogEntry {
    pub skill_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub path: PathBuf,
    pub scope: SkillScope,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillRootView {
    pub scope: SkillScope,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SkillActivationSource {
    Explicit,
    ImplicitFromCatalog,
    Restored,
    Inherited,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SkillActivationState {
    TurnActive,
    SessionActive,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClosureOutcome {
    Completed,
    Continuable,
    Failed,
    Waiting,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkReactivationMode {
    ContinueActive,
    ActivateQueued,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkReactivationSignal {
    pub work_item_id: String,
    pub state: WorkItemState,
    pub reactivation_mode: WorkReactivationMode,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WaitingReason {
    AwaitingOperatorInput,
    AwaitingExternalChange,
    AwaitingTaskResult,
    AwaitingTimer,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimePosture {
    Awake,
    Sleeping,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationTriggerKind {
    OperatorInput,
    TaskResult,
    ExternalEvent,
    TimerFire,
    InternalFollowup,
    SystemTick,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationClass {
    ResumeExpectedWait,
    ResumeOverride,
    LocalContinuation,
    LivenessOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContinuationResolution {
    pub trigger_kind: ContinuationTriggerKind,
    pub class: ContinuationClass,
    pub model_visible: bool,
    pub prior_closure_outcome: ClosureOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_waiting_reason: Option<WaitingReason>,
    pub matched_waiting_reason: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClosureDecision {
    pub outcome: ClosureOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_reason: Option<WaitingReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_signal: Option<WorkReactivationSignal>,
    pub runtime_posture: RuntimePosture,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskWaitPolicy {
    Blocking,
    Background,
}

pub const CHILD_AGENT_TASK_KIND: &str = "child_agent_task";
pub const LEGACY_SUBAGENT_TASK_KIND: &str = "subagent_task";
pub const LEGACY_WORKTREE_SUBAGENT_TASK_KIND: &str = "worktree_subagent_task";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    CommandTask,
    ChildAgentTask,
    SleepJob,
    SubagentTask,
    WorktreeSubagentTask,
}

impl TaskKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CommandTask => "command_task",
            Self::ChildAgentTask => CHILD_AGENT_TASK_KIND,
            Self::SleepJob => "sleep_job",
            Self::SubagentTask => LEGACY_SUBAGENT_TASK_KIND,
            Self::WorktreeSubagentTask => LEGACY_WORKTREE_SUBAGENT_TASK_KIND,
        }
    }

    pub fn is_child_agent(self) -> bool {
        matches!(
            self,
            Self::ChildAgentTask | Self::SubagentTask | Self::WorktreeSubagentTask
        )
    }
}

impl std::fmt::Display for TaskKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChildAgentWorkspaceMode {
    Inherit,
    Worktree,
}

impl ChildAgentWorkspaceMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Inherit => "inherit",
            Self::Worktree => "worktree",
        }
    }

    pub fn is_worktree(self) -> bool {
        self == Self::Worktree
    }

    pub fn from_label(value: &str) -> Option<Self> {
        match value {
            "inherit" => Some(Self::Inherit),
            "worktree" => Some(Self::Worktree),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActiveSkillRecord {
    pub skill_id: String,
    pub name: String,
    pub path: PathBuf,
    pub scope: SkillScope,
    pub agent_id: String,
    pub activation_source: SkillActivationSource,
    pub activation_state: SkillActivationState,
    pub activated_at_turn: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SkillsRuntimeView {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discovered_roots: Vec<SkillRootView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discoverable_skills: Vec<SkillCatalogEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attached_skills: Vec<SkillCatalogEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_skills: Vec<ActiveSkillRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    OperatorPrompt,
    ChannelEvent,
    WebhookEvent,
    CallbackEvent,
    TimerTick,
    SystemTick,
    TaskResult,
    TaskStatus,
    Control,
    BriefAck,
    BriefResult,
    InternalFollowup,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    TrustedOperator,
    TrustedSystem,
    TrustedIntegration,
    UntrustedExternal,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Interrupt,
    Next,
    Normal,
    Background,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageOrigin {
    Operator {
        actor_id: Option<String>,
    },
    Channel {
        channel_id: String,
        sender_id: Option<String>,
    },
    Webhook {
        source: String,
        event_type: Option<String>,
    },
    Callback {
        descriptor_id: String,
        source: Option<String>,
    },
    Timer {
        timer_id: String,
    },
    System {
        subsystem: String,
    },
    Task {
        task_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageBody {
    Text {
        text: String,
    },
    Json {
        value: Value,
    },
    Brief {
        title: Option<String>,
        text: String,
        attachments: Option<Vec<BriefAttachment>>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BriefAttachment {
    pub kind: String,
    pub name: String,
    pub uri: Option<String>,
    pub value: Option<Value>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityClass {
    OperatorInstruction,
    RuntimeInstruction,
    IntegrationSignal,
    ExternalEvidence,
}

impl AuthorityClass {
    pub fn from_trust(trust: &TrustLevel) -> Self {
        match trust {
            TrustLevel::TrustedOperator => Self::OperatorInstruction,
            TrustLevel::TrustedSystem => Self::RuntimeInstruction,
            TrustLevel::TrustedIntegration => Self::IntegrationSignal,
            TrustLevel::UntrustedExternal => Self::ExternalEvidence,
        }
    }
}

impl From<&TrustLevel> for AuthorityClass {
    fn from(value: &TrustLevel) -> Self {
        Self::from_trust(value)
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MessageEnvelope {
    pub id: String,
    #[serde(alias = "session_id")]
    pub agent_id: String,
    pub created_at: DateTime<Utc>,
    pub kind: MessageKind,
    pub origin: MessageOrigin,
    pub trust: TrustLevel,
    pub authority_class: AuthorityClass,
    pub priority: Priority,
    pub body: MessageBody,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_surface: Option<MessageDeliverySurface>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_context: Option<AdmissionContext>,
    pub metadata: Option<Value>,
    pub correlation_id: Option<String>,
    pub causation_id: Option<String>,
}

impl MessageEnvelope {
    pub fn new(
        agent_id: impl Into<String>,
        kind: MessageKind,
        origin: MessageOrigin,
        trust: TrustLevel,
        priority: Priority,
        body: MessageBody,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            agent_id: agent_id.into(),
            created_at: Utc::now(),
            kind,
            origin,
            authority_class: AuthorityClass::from_trust(&trust),
            trust,
            priority,
            body,
            delivery_surface: None,
            admission_context: None,
            metadata: None,
            correlation_id: None,
            causation_id: None,
        }
    }

    pub fn with_admission(
        mut self,
        delivery_surface: MessageDeliverySurface,
        admission_context: AdmissionContext,
    ) -> Self {
        self.delivery_surface = Some(delivery_surface);
        self.admission_context = Some(admission_context);
        self
    }
}

impl<'de> Deserialize<'de> for MessageEnvelope {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct MessageEnvelopeCompat {
            id: String,
            #[serde(alias = "session_id")]
            agent_id: String,
            created_at: DateTime<Utc>,
            kind: MessageKind,
            origin: MessageOrigin,
            trust: TrustLevel,
            #[serde(default)]
            authority_class: Option<AuthorityClass>,
            priority: Priority,
            body: MessageBody,
            #[serde(default)]
            delivery_surface: Option<MessageDeliverySurface>,
            #[serde(default)]
            admission_context: Option<AdmissionContext>,
            metadata: Option<Value>,
            correlation_id: Option<String>,
            causation_id: Option<String>,
        }

        let compat = MessageEnvelopeCompat::deserialize(deserializer)?;
        let authority_class = compat
            .authority_class
            .unwrap_or_else(|| AuthorityClass::from_trust(&compat.trust));
        Ok(Self {
            id: compat.id,
            agent_id: compat.agent_id,
            created_at: compat.created_at,
            kind: compat.kind,
            origin: compat.origin,
            trust: compat.trust,
            authority_class,
            priority: compat.priority,
            body: compat.body,
            delivery_surface: compat.delivery_surface,
            admission_context: compat.admission_context,
            metadata: compat.metadata,
            correlation_id: compat.correlation_id,
            causation_id: compat.causation_id,
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageDeliverySurface {
    CliPrompt,
    RunOnce,
    HttpPublicEnqueue,
    HttpWebhook,
    HttpCallbackEnqueue,
    HttpCallbackWake,
    HttpControlPrompt,
    RemoteOperatorTransport,
    TimerScheduler,
    RuntimeSystem,
    TaskRejoin,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionContext {
    PublicUnauthenticated,
    ControlAuthenticated,
    OperatorTransportAuthenticated,
    ExternalTriggerCapability,
    LocalProcess,
    RuntimeOwned,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Booting,
    AwakeIdle,
    AwakeRunning,
    AwaitingTask,
    Asleep,
    Paused,
    Stopped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeFailurePhase {
    Startup,
    Shutdown,
    RuntimeTurn,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FailureArtifactCategory {
    Transport,
    Protocol,
    Runtime,
    Task,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FailureArtifact {
    pub category: FailureArtifactCategory,
    pub kind: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_status: Option<i32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_chain: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeFailureSummary {
    pub occurred_at: DateTime<Utc>,
    pub summary: String,
    pub phase: RuntimeFailurePhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_artifact: Option<FailureArtifact>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TurnTerminalKind {
    Completed,
    Aborted,
    BaselineOverBudget,
}

impl TurnTerminalKind {
    pub fn is_failure(self) -> bool {
        !matches!(self, Self::Completed)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TurnTerminalRecord {
    pub turn_index: u64,
    pub kind: TurnTerminalKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_assistant_message: Option<String>,
    pub completed_at: DateTime<Utc>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct WorkingMemorySnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope_hints: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub current_plan: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub working_set_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_decisions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_followups: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waiting_on: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkingMemoryUpdateReason {
    TerminalTurnCompleted,
    TaskRejoined,
    WakeResumed,
    ActiveWorkChanged,
    ScopeHintsChanged,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkingMemoryDelta {
    pub from_revision: u64,
    pub to_revision: u64,
    pub created_at_turn: u64,
    pub reason: WorkingMemoryUpdateReason,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_fields: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub summary_lines: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TurnMemoryDelta {
    pub turn_index: u64,
    #[serde(default)]
    pub active_work_changed: bool,
    #[serde(default)]
    pub work_plan_changed: bool,
    #[serde(default)]
    pub scope_hints_changed: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub touched_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verification: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_followups: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waiting_on: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EpisodeBoundaryReason {
    ActiveWorkSwitched,
    WaitBoundary,
    #[serde(rename = "result_checkpoint")]
    LegacyResultCheckpoint,
    TaskRejoined,
    HardTurnCap,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActiveEpisodeBuilder {
    pub id: String,
    pub started_at: DateTime<Utc>,
    pub start_turn_index: u64,
    pub latest_turn_index: u64,
    pub start_message_count: usize,
    pub latest_message_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope_hints: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub working_set_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verification: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub carry_forward: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waiting_on: Vec<String>,
}

impl ActiveEpisodeBuilder {
    pub fn new(agent: &AgentState, snapshot: &WorkingMemorySnapshot, message_count: usize) -> Self {
        Self::new_with_start(
            agent.id.clone(),
            snapshot,
            message_count,
            agent.turn_index.max(1),
        )
    }

    pub fn new_with_start(
        _agent_id: impl Into<String>,
        snapshot: &WorkingMemorySnapshot,
        message_count: usize,
        start_turn_index: u64,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: format!("ep_{}", Uuid::new_v4().simple()),
            started_at: now,
            start_turn_index,
            latest_turn_index: start_turn_index,
            start_message_count: message_count,
            latest_message_count: message_count,
            current_work_item_id: snapshot.current_work_item_id.clone(),
            delivery_target: snapshot.delivery_target.clone(),
            work_summary: snapshot.work_summary.clone(),
            scope_hints: Vec::new(),
            working_set_files: snapshot.working_set_files.clone(),
            commands: Vec::new(),
            verification: Vec::new(),
            decisions: Vec::new(),
            carry_forward: snapshot.pending_followups.clone(),
            waiting_on: snapshot.waiting_on.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextEpisodeRecord {
    pub id: String,
    pub agent_id: String,
    #[serde(default = "default_agent_home_workspace_id")]
    pub workspace_id: String,
    pub created_at: DateTime<Utc>,
    pub finalized_at: DateTime<Utc>,
    pub start_turn_index: u64,
    pub end_turn_index: u64,
    pub start_message_count: usize,
    pub end_message_count: usize,
    pub boundary_reason: EpisodeBoundaryReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope_hints: Vec<String>,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub working_set_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verification: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub carry_forward: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waiting_on: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct WorkingMemoryState {
    #[serde(default)]
    pub working_memory_revision: u64,
    #[serde(default)]
    pub compression_epoch: u64,
    #[serde(default)]
    pub current_working_memory: WorkingMemorySnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_working_memory_delta: Option<WorkingMemoryDelta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_prompted_working_memory_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_episode_id: Option<String>,
    #[serde(default)]
    pub archived_episode_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_episode_builder: Option<ActiveEpisodeBuilder>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentState {
    pub id: String,
    pub status: AgentStatus,
    pub sleeping_until: Option<DateTime<Utc>>,
    pub current_run_id: Option<String>,
    pub pending: usize,
    pub active_task_ids: Vec<String>,
    pub last_wake_reason: Option<String>,
    pub last_brief_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub working_memory: WorkingMemoryState,
    pub context_summary: Option<String>,
    pub compacted_message_count: usize,
    pub total_message_count: usize,
    #[serde(default)]
    pub total_input_tokens: u64,
    #[serde(default)]
    pub total_output_tokens: u64,
    #[serde(default)]
    pub total_model_rounds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_turn_token_usage: Option<TokenUsage>,
    #[serde(default)]
    pub tool_latency: Vec<ToolLatencyMetrics>,
    #[serde(default)]
    pub execution_profile: ExecutionProfile,
    #[serde(default)]
    pub pending_wake_hint: Option<PendingWakeHint>,
    #[serde(default)]
    pub attached_workspaces: Vec<String>,
    #[serde(default)]
    pub active_workspace_entry: Option<ActiveWorkspaceEntry>,
    #[serde(default)]
    pub worktree_session: Option<WorktreeSession>,
    #[serde(default)]
    pub turn_index: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_turn_work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_turn_operator_binding_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_turn_operator_reply_route_id: Option<String>,
    #[serde(default)]
    pub active_skills: Vec<ActiveSkillRecord>,
    #[serde(default)]
    pub last_continuation: Option<ContinuationResolution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_override: Option<ModelRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_requested_model: Option<ModelRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_active_model: Option<ModelRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_turn_terminal: Option<TurnTerminalRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_runtime_failure: Option<RuntimeFailureSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

impl TokenUsage {
    pub fn new(input_tokens: u64, output_tokens: u64) -> Self {
        Self {
            input_tokens,
            output_tokens,
            total_tokens: input_tokens.saturating_add(output_tokens),
        }
    }

    pub fn is_zero(&self) -> bool {
        self.input_tokens == 0 && self.output_tokens == 0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTokenUsageSummary {
    pub total: TokenUsage,
    pub total_model_rounds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_turn: Option<TokenUsage>,
}

impl AgentState {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            status: AgentStatus::Booting,
            sleeping_until: None,
            current_run_id: None,
            pending: 0,
            active_task_ids: Vec::new(),
            last_wake_reason: None,
            last_brief_at: None,
            working_memory: WorkingMemoryState::default(),
            context_summary: None,
            compacted_message_count: 0,
            total_message_count: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_model_rounds: 0,
            last_turn_token_usage: None,
            tool_latency: Vec::new(),
            execution_profile: ExecutionProfile::default(),
            pending_wake_hint: None,
            attached_workspaces: Vec::new(),
            active_workspace_entry: None,
            worktree_session: None,
            turn_index: 0,
            current_turn_work_item_id: None,
            current_work_item_id: None,
            current_turn_operator_binding_id: None,
            current_turn_operator_reply_route_id: None,
            active_skills: Vec::new(),
            last_continuation: None,
            model_override: None,
            last_requested_model: None,
            last_active_model: None,
            last_turn_terminal: None,
            last_runtime_failure: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingWakeHint {
    pub reason: String,
    pub source: Option<String>,
    pub resource: Option<String>,
    pub body: Option<MessageBody>,
    pub content_type: Option<String>,
    pub correlation_id: Option<String>,
    pub causation_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallbackDeliveryMode {
    EnqueueMessage,
    WakeOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WaitingIntentStatus {
    Active,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WaitingIntentRecord {
    pub id: String,
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    pub summary: String,
    pub source: String,
    pub resource: Option<String>,
    pub condition: String,
    pub delivery_mode: CallbackDeliveryMode,
    pub status: WaitingIntentStatus,
    pub external_trigger_id: String,
    pub created_at: DateTime<Utc>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub last_triggered_at: Option<DateTime<Utc>>,
    pub trigger_count: u64,
    pub correlation_id: Option<String>,
    pub causation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExternalTriggerStatus {
    Active,
    Revoked,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExternalTriggerRecord {
    pub external_trigger_id: String,
    pub target_agent_id: String,
    pub waiting_intent_id: String,
    pub delivery_mode: CallbackDeliveryMode,
    pub token_hash: String,
    pub status: ExternalTriggerStatus,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub last_delivered_at: Option<DateTime<Utc>>,
    pub delivery_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExternalTriggerStateSnapshot {
    pub external_trigger_id: String,
    pub target_agent_id: String,
    pub waiting_intent_id: String,
    pub delivery_mode: CallbackDeliveryMode,
    pub status: ExternalTriggerStatus,
    pub delivery_count: u64,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub last_delivered_at: Option<DateTime<Utc>>,
}

impl From<ExternalTriggerRecord> for ExternalTriggerStateSnapshot {
    fn from(record: ExternalTriggerRecord) -> Self {
        Self {
            external_trigger_id: record.external_trigger_id,
            target_agent_id: record.target_agent_id,
            waiting_intent_id: record.waiting_intent_id,
            delivery_mode: record.delivery_mode,
            status: record.status,
            delivery_count: record.delivery_count,
            created_at: record.created_at,
            revoked_at: record.revoked_at,
            last_delivered_at: record.last_delivered_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WaitingIntentSummary {
    pub id: String,
    pub source: String,
    pub resource: Option<String>,
    pub condition: String,
    pub delivery_mode: CallbackDeliveryMode,
    pub status: WaitingIntentStatus,
    pub trigger_count: u64,
    pub created_at: DateTime<Utc>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub last_triggered_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExternalTriggerSummary {
    pub external_trigger_id: String,
    pub target_agent_id: String,
    pub waiting_intent_id: String,
    pub delivery_mode: CallbackDeliveryMode,
    pub status: ExternalTriggerStatus,
    pub delivery_count: u64,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub last_delivered_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExternalTriggerCapability {
    pub waiting_intent_id: String,
    pub external_trigger_id: String,
    pub trigger_url: String,
    pub target_agent_id: String,
    pub delivery_mode: CallbackDeliveryMode,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CancelWaitingResult {
    pub waiting_intent_id: String,
    pub external_trigger_id: String,
    pub status: WaitingIntentStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallbackIngressDisposition {
    Enqueued,
    Triggered,
    Coalesced,
    Ignored,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CallbackDeliveryResult {
    pub agent_id: String,
    pub waiting_intent_id: String,
    pub external_trigger_id: String,
    pub delivery_mode: CallbackDeliveryMode,
    pub disposition: CallbackIngressDisposition,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CallbackDeliveryPayload {
    pub body: Option<MessageBody>,
    pub content_type: Option<String>,
    pub correlation_id: Option<String>,
    pub causation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperatorNotificationBoundary {
    PrimaryOperator,
    ParentSupervisor,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperatorNotificationRecord {
    pub notification_id: String,
    pub agent_id: String,
    pub requested_by_agent_id: String,
    pub target_operator_boundary: OperatorNotificationBoundary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_parent_agent_id: Option<String>,
    pub message: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub causation_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NotifyOperatorResult {
    pub notification: OperatorNotificationRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnqueueResult {
    pub enqueued: bool,
    pub priority: Priority,
    pub follow_up_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperatorTransportBindingStatus {
    Active,
    Revoked,
    Expired,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperatorTransportDeliveryAuthKind {
    Bearer,
    Hmac,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperatorTransportDeliveryAuth {
    pub kind: OperatorTransportDeliveryAuthKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bearer_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperatorTransportCapabilities {
    pub text: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub markdown: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OperatorTransportBinding {
    pub binding_id: String,
    pub transport: String,
    pub operator_actor_id: String,
    pub target_agent_id: String,
    pub default_route_id: String,
    pub delivery_callback_url: String,
    pub delivery_auth: OperatorTransportDeliveryAuth,
    pub capabilities: OperatorTransportCapabilities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_identity_ref: Option<String>,
    pub status: OperatorTransportBindingStatus,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperatorDeliveryStatus {
    Pending,
    AcceptedByTransport,
    FailedToSubmit,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperatorDeliveryTriggerKind {
    OperatorNotification,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OperatorDeliveryRecord {
    pub delivery_intent_id: String,
    pub output_event_id: String,
    pub agent_id: String,
    pub route_id: String,
    pub binding_id: String,
    pub trigger_kind: OperatorDeliveryTriggerKind,
    pub status: OperatorDeliveryStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport_delivery_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_summary: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Queued,
    Running,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskHandle {
    pub task_id: String,
    pub task_kind: String,
    pub status: TaskStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_output: Option<String>,
}

impl TaskHandle {
    pub fn new(
        task_id: impl Into<String>,
        task_kind: impl Into<String>,
        status: TaskStatus,
        initial_output: Option<String>,
    ) -> Self {
        Self {
            task_id: task_id.into(),
            task_kind: task_kind.into(),
            status,
            initial_output,
        }
    }

    pub fn from_task_record(task: &TaskRecord, initial_output: Option<String>) -> Self {
        Self::new(
            task.id.clone(),
            task.kind.as_str(),
            task.status.clone(),
            initial_output,
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChildAgentPhase {
    Running,
    Blocked,
    Waiting,
    Terminal,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChildAgentBlockedReason {
    ManagedTaskQueued,
    ManagedTaskRunning,
    ManagedTaskCancelling,
    AwaitingManagedTask,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChildAgentObservabilitySnapshot {
    pub phase: ChildAgentPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<ChildAgentBlockedReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_reason: Option<WaitingReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_progress_brief: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_result_brief: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskRecord {
    pub id: String,
    #[serde(alias = "session_id")]
    pub agent_id: String,
    pub kind: TaskKind,
    pub status: TaskStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub parent_message_id: Option<String>,
    pub summary: Option<String>,
    #[serde(default)]
    pub detail: Option<Value>,
    #[serde(default)]
    pub recovery: Option<TaskRecoverySpec>,
}

impl TaskRecord {
    pub fn wait_policy(&self) -> TaskWaitPolicy {
        self.detail
            .as_ref()
            .and_then(|detail| detail.get("wait_policy"))
            .and_then(|value| value.as_str())
            .map(|value| match value {
                "blocking" => TaskWaitPolicy::Blocking,
                _ => TaskWaitPolicy::Background,
            })
            .or_else(|| self.recovery.as_ref().map(TaskRecoverySpec::wait_policy))
            .unwrap_or(TaskWaitPolicy::Background)
    }

    pub fn is_blocking(&self) -> bool {
        self.wait_policy() == TaskWaitPolicy::Blocking
    }

    pub fn is_child_agent_task(&self) -> bool {
        self.kind.is_child_agent()
    }

    pub fn child_agent_workspace_mode(&self) -> Option<ChildAgentWorkspaceMode> {
        self.detail
            .as_ref()
            .and_then(|detail| detail.get("workspace_mode"))
            .and_then(Value::as_str)
            .and_then(ChildAgentWorkspaceMode::from_label)
            .or_else(|| {
                self.recovery
                    .as_ref()
                    .and_then(TaskRecoverySpec::child_agent_workspace_mode)
            })
            .or_else(|| match self.kind {
                TaskKind::SubagentTask => Some(ChildAgentWorkspaceMode::Inherit),
                TaskKind::WorktreeSubagentTask => Some(ChildAgentWorkspaceMode::Worktree),
                _ => None,
            })
    }

    pub fn is_worktree_child_agent_task(&self) -> bool {
        self.is_child_agent_task()
            && self
                .child_agent_workspace_mode()
                .is_some_and(ChildAgentWorkspaceMode::is_worktree)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskListEntry {
    pub id: String,
    pub kind: String,
    pub status: TaskStatus,
    pub summary: Option<String>,
    pub updated_at: DateTime<Utc>,
    pub wait_policy: TaskWaitPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CommandTaskStatusSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tty: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_status: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continue_on_result: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promoted_from_exec_command: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepts_input: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_target: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskStatusSnapshot {
    pub task_id: String,
    pub kind: String,
    pub status: TaskStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub wait_policy: TaskWaitPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<CommandTaskStatusSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_observability: Option<ChildAgentObservabilitySnapshot>,
}

impl TaskStatusSnapshot {
    pub fn from_task_record(task: &TaskRecord) -> Self {
        let command = if task.kind == TaskKind::CommandTask {
            Some(CommandTaskStatusSnapshot {
                tty: task_detail_bool(&task.detail, "tty"),
                output_path: task_detail_string(&task.detail, "output_path"),
                result_summary: task_detail_string(&task.detail, "output_summary"),
                exit_status: task_detail_i32(&task.detail, "exit_status"),
                continue_on_result: task_detail_bool(&task.detail, "continue_on_result"),
                promoted_from_exec_command: task_detail_bool(
                    &task.detail,
                    "promoted_from_exec_command",
                ),
                accepts_input: task_detail_bool(&task.detail, "accepts_input"),
                input_target: task_detail_string(&task.detail, "input_target"),
            })
        } else {
            None
        };
        let child_agent_id = task_detail_string(&task.detail, "child_agent_id");
        let child_observability = task_detail_value(&task.detail, "child_observability")
            .and_then(|value| serde_json::from_value(value.clone()).ok());

        Self {
            task_id: task.id.clone(),
            kind: task.kind.as_str().to_string(),
            status: task.status.clone(),
            summary: task.summary.clone(),
            created_at: task.created_at,
            updated_at: task.updated_at,
            wait_policy: task.wait_policy(),
            parent_message_id: task.parent_message_id.clone(),
            command,
            child_agent_id,
            child_observability,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskOutputRetrievalStatus {
    Success,
    Timeout,
    NotReady,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskOutputSnapshot {
    pub task_id: String,
    pub kind: String,
    pub status: TaskStatus,
    pub summary: Option<String>,
    pub output_preview: String,
    pub output_truncated: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ToolArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_artifact: Option<usize>,
    pub result_summary: Option<String>,
    pub exit_status: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_artifact: Option<FailureArtifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskOutputResult {
    pub retrieval_status: TaskOutputRetrievalStatus,
    pub task: TaskOutputSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "disposition", rename_all = "snake_case")]
pub enum ExecCommandOutcome {
    Completed {
        exit_status: Option<i32>,
        stdout_preview: Option<String>,
        stderr_preview: Option<String>,
        truncated: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        artifacts: Vec<ToolArtifactRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stdout_artifact: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stderr_artifact: Option<usize>,
    },
    PromotedToTask {
        task_handle: TaskHandle,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        initial_output_preview: Option<String>,
        initial_output_truncated: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExecCommandResult {
    #[serde(flatten)]
    pub outcome: ExecCommandOutcome,
    pub summary_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecCommandBatchItemStatus {
    Completed,
    Failed,
    Rejected,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExecCommandBatchItemResult {
    pub index: usize,
    pub cmd: String,
    pub status: ExecCommandBatchItemStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ExecCommandResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExecCommandBatchResult {
    pub item_count: usize,
    pub completed_count: usize,
    pub failed_count: usize,
    pub rejected_count: usize,
    pub skipped_count: usize,
    pub stop_on_error: bool,
    pub items: Vec<ExecCommandBatchItemResult>,
    pub summary_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolArtifactRef {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskStatusResult {
    pub task: TaskStatusSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskInputResult {
    pub task: TaskStatusSnapshot,
    pub accepted_input: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_written: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentGetResult {
    pub agent: AgentSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpawnAgentResult {
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_handle: Option<TaskHandle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_work_item_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpawnAgentWorkItemRequest {
    pub parent_work_item_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_delivery_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_plan: Option<Vec<WorkPlanItem>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApplyPatchAction {
    Add,
    Modify,
    Delete,
    Move,
}

impl ApplyPatchAction {
    pub fn marker(self) -> &'static str {
        match self {
            Self::Add => "A",
            Self::Modify => "M",
            Self::Delete => "D",
            Self::Move => "R",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplyPatchChangedFile {
    pub action: ApplyPatchAction,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplyPatchIgnoredMetadata {
    pub path: String,
    pub kind: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplyPatchDiagnostic {
    pub path: String,
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplyPatchResult {
    #[serde(default)]
    pub changed_files: Vec<ApplyPatchChangedFile>,
    pub changed_paths: Vec<String>,
    pub changed_file_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignored_metadata: Vec<ApplyPatchIgnoredMetadata>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<ApplyPatchDiagnostic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UseWorkspaceResult {
    pub workspace_id: String,
    pub workspace_anchor: PathBuf,
    pub execution_root: PathBuf,
    pub cwd: PathBuf,
    pub mode: String,
    pub projection_kind: String,
    pub access_mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnterWorkspaceResult {
    pub workspace_id: String,
    pub projection_kind: String,
    pub access_mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExitWorkspaceResult {
    pub exited: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskStopResult {
    pub task: TaskRecord,
    pub stop_requested: bool,
    pub force_stop_requested: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskRecoverySpec {
    ChildAgentTask {
        summary: String,
        prompt: String,
        trust: TrustLevel,
        workspace_mode: ChildAgentWorkspaceMode,
    },
    SubagentTask {
        summary: String,
        prompt: String,
        trust: TrustLevel,
    },
    WorktreeSubagentTask {
        summary: String,
        prompt: String,
        trust: TrustLevel,
    },
    CommandTask {
        summary: String,
        spec: CommandTaskSpec,
        trust: TrustLevel,
        promoted_from_exec_command: bool,
    },
}

fn task_detail_string(detail: &Option<Value>, key: &str) -> Option<String> {
    detail
        .as_ref()
        .and_then(|detail| detail.get(key))
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn task_detail_bool(detail: &Option<Value>, key: &str) -> Option<bool> {
    detail
        .as_ref()
        .and_then(|detail| detail.get(key))
        .and_then(Value::as_bool)
}

fn task_detail_i32(detail: &Option<Value>, key: &str) -> Option<i32> {
    detail
        .as_ref()
        .and_then(|detail| detail.get(key))
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
}

fn task_detail_value<'a>(detail: &'a Option<Value>, key: &str) -> Option<&'a Value> {
    detail.as_ref().and_then(|detail| detail.get(key))
}

impl TaskRecoverySpec {
    pub fn child_agent_workspace_mode(&self) -> Option<ChildAgentWorkspaceMode> {
        match self {
            TaskRecoverySpec::ChildAgentTask { workspace_mode, .. } => Some(*workspace_mode),
            TaskRecoverySpec::SubagentTask { .. } => Some(ChildAgentWorkspaceMode::Inherit),
            TaskRecoverySpec::WorktreeSubagentTask { .. } => {
                Some(ChildAgentWorkspaceMode::Worktree)
            }
            TaskRecoverySpec::CommandTask { .. } => None,
        }
    }

    pub fn wait_policy(&self) -> TaskWaitPolicy {
        match self {
            TaskRecoverySpec::ChildAgentTask { .. } => TaskWaitPolicy::Blocking,
            TaskRecoverySpec::SubagentTask { .. } => TaskWaitPolicy::Blocking,
            TaskRecoverySpec::WorktreeSubagentTask { .. } => TaskWaitPolicy::Blocking,
            TaskRecoverySpec::CommandTask { spec, .. } => {
                if spec.continue_on_result {
                    TaskWaitPolicy::Blocking
                } else {
                    TaskWaitPolicy::Background
                }
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CommandTaskSpec {
    pub cmd: String,
    #[serde(default)]
    pub workdir: Option<String>,
    #[serde(default)]
    pub shell: Option<String>,
    pub login: bool,
    #[serde(default)]
    pub tty: bool,
    pub yield_time_ms: u64,
    #[serde(default)]
    pub max_output_tokens: Option<u64>,
    #[serde(default)]
    pub accepts_input: bool,
    #[serde(default)]
    pub continue_on_result: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemState {
    Open,
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkItemRecord {
    pub id: String,
    #[serde(alias = "session_id")]
    pub agent_id: String,
    #[serde(default = "default_agent_home_workspace_id")]
    pub workspace_id: String,
    pub delivery_target: String,
    pub state: WorkItemState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl WorkItemRecord {
    pub fn new(
        agent_id: impl Into<String>,
        delivery_target: impl Into<String>,
        state: WorkItemState,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: format!("work_{}", Uuid::new_v4().simple()),
            agent_id: agent_id.into(),
            workspace_id: AGENT_HOME_WORKSPACE_ID.to_string(),
            delivery_target: delivery_target.into(),
            state,
            blocked_by: None,
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemDelegationState {
    Open,
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkItemDelegationRecord {
    pub delegation_id: String,
    pub parent_agent_id: String,
    pub parent_work_item_id: String,
    pub child_agent_id: String,
    pub child_work_item_id: String,
    pub state: WorkItemDelegationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_summary: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl WorkItemDelegationRecord {
    pub fn new(
        parent_agent_id: impl Into<String>,
        parent_work_item_id: impl Into<String>,
        child_agent_id: impl Into<String>,
        child_work_item_id: impl Into<String>,
    ) -> Self {
        let now = Utc::now();
        Self {
            delegation_id: format!("delegation_{}", Uuid::new_v4().simple()),
            parent_agent_id: parent_agent_id.into(),
            parent_work_item_id: parent_work_item_id.into(),
            child_agent_id: child_agent_id.into(),
            child_work_item_id: child_work_item_id.into(),
            state: WorkItemDelegationState::Open,
            result_summary: None,
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeliverySummaryRecord {
    pub id: String,
    #[serde(alias = "session_id")]
    pub agent_id: String,
    pub work_item_id: String,
    pub created_at: DateTime<Utc>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_turn_index: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Value>,
}

impl DeliverySummaryRecord {
    pub fn new(
        agent_id: impl Into<String>,
        work_item_id: impl Into<String>,
        text: impl Into<String>,
        source_turn_index: Option<u64>,
        evidence: Option<Value>,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            agent_id: agent_id.into(),
            work_item_id: work_item_id.into(),
            created_at: Utc::now(),
            text: text.into(),
            source_turn_index,
            evidence,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkPlanStepStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkPlanItem {
    pub step: String,
    pub status: WorkPlanStepStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkPlanSnapshot {
    pub work_item_id: String,
    #[serde(alias = "session_id")]
    pub agent_id: String,
    pub created_at: DateTime<Utc>,
    pub items: Vec<WorkPlanItem>,
}

impl WorkPlanSnapshot {
    pub fn new(
        agent_id: impl Into<String>,
        work_item_id: impl Into<String>,
        items: Vec<WorkPlanItem>,
    ) -> Self {
        Self {
            work_item_id: work_item_id.into(),
            agent_id: agent_id.into(),
            created_at: Utc::now(),
            items,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TimerRecord {
    pub id: String,
    #[serde(alias = "session_id")]
    pub agent_id: String,
    pub created_at: DateTime<Utc>,
    pub duration_ms: u64,
    pub interval_ms: Option<u64>,
    pub repeat: bool,
    #[serde(default)]
    pub status: TimerStatus,
    pub summary: Option<String>,
    #[serde(default)]
    pub next_fire_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_fired_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub fire_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TimerStatus {
    #[serde(alias = "scheduled")]
    Active,
    Completed,
    Cancelled,
}

impl Default for TimerStatus {
    fn default() -> Self {
        Self::Active
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QueueEntryStatus {
    Queued,
    Dequeued,
    Processed,
    Dropped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QueueEntryRecord {
    pub message_id: String,
    #[serde(alias = "session_id")]
    pub agent_id: String,
    pub priority: Priority,
    pub status: QueueEntryStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolExecutionStatus {
    Success,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolExecutionRecord {
    pub id: String,
    #[serde(alias = "session_id")]
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    #[serde(default)]
    pub turn_index: u64,
    pub tool_name: String,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub duration_ms: u64,
    pub trust: TrustLevel,
    pub status: ToolExecutionStatus,
    pub input: Value,
    pub output: Value,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_surface: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolLatencyMetrics {
    pub tool_name: String,
    pub total_calls: u64,
    pub total_duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorktreeSession {
    pub original_cwd: PathBuf,
    pub original_branch: String,
    pub worktree_path: PathBuf,
    pub worktree_branch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptEntryKind {
    IncomingMessage,
    AssistantRound,
    ToolResults,
    RuntimeFailure,
    ContinuationPrompt,
    SubagentPrompt,
    SubagentAssistantRound,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TranscriptEntry {
    pub id: String,
    #[serde(alias = "session_id")]
    pub agent_id: String,
    pub created_at: DateTime<Utc>,
    pub kind: TranscriptEntryKind,
    pub round: Option<usize>,
    pub related_message_id: Option<String>,
    pub stop_reason: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub data: Value,
}

impl TranscriptEntry {
    pub fn new(
        agent_id: impl Into<String>,
        kind: TranscriptEntryKind,
        round: Option<usize>,
        related_message_id: Option<String>,
        data: Value,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            agent_id: agent_id.into(),
            created_at: Utc::now(),
            kind,
            round,
            related_message_id,
            stop_reason: None,
            input_tokens: None,
            output_tokens: None,
            data,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BriefKind {
    Ack,
    Result,
    Failure,
}

impl BriefKind {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Result | Self::Failure)
    }

    pub fn is_success(self) -> bool {
        matches!(self, Self::Result)
    }

    pub fn is_failure(self) -> bool {
        matches!(self, Self::Failure)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BriefRecord {
    pub id: String,
    #[serde(alias = "session_id")]
    pub agent_id: String,
    #[serde(default = "default_agent_home_workspace_id")]
    pub workspace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    pub kind: BriefKind,
    pub created_at: DateTime<Utc>,
    pub text: String,
    pub attachments: Option<Vec<BriefAttachment>>,
    pub related_message_id: Option<String>,
    pub related_task_id: Option<String>,
}

impl BriefRecord {
    pub fn new(
        agent_id: impl Into<String>,
        kind: BriefKind,
        text: impl Into<String>,
        related_message_id: Option<String>,
        related_task_id: Option<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            agent_id: agent_id.into(),
            workspace_id: AGENT_HOME_WORKSPACE_ID.to_string(),
            work_item_id: None,
            kind,
            created_at: Utc::now(),
            text: text.into(),
            attachments: None,
            related_message_id,
            related_task_id,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub kind: String,
    pub data: Value,
}

impl AuditEvent {
    pub fn new(kind: impl Into<String>, data: Value) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            created_at: Utc::now(),
            kind: kind.into(),
            data,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum ControlAction {
    Pause,
    Resume,
    Stop,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentModelSource {
    RuntimeDefault,
    AgentOverride,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentModelState {
    pub source: AgentModelSource,
    pub runtime_default_model: ModelRef,
    pub effective_model: ModelRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_model: Option<ModelRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_model: Option<ModelRef>,
    #[serde(default)]
    pub fallback_active: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effective_fallback_models: Vec<ModelRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub override_model: Option<ModelRef>,
    #[serde(default)]
    pub resolved_policy: ResolvedRuntimeModelPolicy,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub available_models: Vec<BuiltInModelMetadata>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_availability: Vec<ResolvedModelAvailability>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedModelAvailability {
    pub model: String,
    pub provider: String,
    pub display_name: String,
    pub metadata_source: String,
    pub provider_configured: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_kind: Option<String>,
    pub credential_configured: bool,
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
    pub policy: ResolvedRuntimeModelPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentLifecycleHint {
    pub resume_required: bool,
    pub accepts_external_messages: bool,
    pub wake_requires_resume: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_cli_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_control_path: Option<String>,
}

impl Default for AgentLifecycleHint {
    fn default() -> Self {
        Self {
            resume_required: false,
            accepts_external_messages: true,
            wake_requires_resume: false,
            operator_hint: None,
            resume_cli_hint: None,
            resume_control_path: None,
        }
    }
}

impl AgentLifecycleHint {
    pub fn from_status(agent_id: &str, status: AgentStatus) -> Self {
        if status == AgentStatus::Stopped {
            return Self {
                resume_required: true,
                accepts_external_messages: false,
                wake_requires_resume: true,
                operator_hint: Some(
                    "agent is administratively stopped; resume before new prompts or wakes".into(),
                ),
                resume_cli_hint: Some(format!("holon control resume --agent {agent_id}")),
                resume_control_path: Some(format!("/control/agents/{agent_id}/control")),
            };
        }
        Self::default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentSummary {
    pub identity: AgentIdentityView,
    pub agent: AgentState,
    #[serde(default)]
    pub lifecycle: AgentLifecycleHint,
    pub model: AgentModelState,
    pub token_usage: AgentTokenUsageSummary,
    pub closure: ClosureDecision,
    pub execution: ExecutionSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_workspace_occupancy: Option<WorkspaceOccupancyRecord>,
    pub loaded_agents_md: LoadedAgentsMdView,
    pub skills: SkillsRuntimeView,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_children: Vec<ChildAgentSummary>,
    pub active_waiting_intents: Vec<WaitingIntentSummary>,
    #[serde(default)]
    pub active_external_triggers: Vec<ExternalTriggerSummary>,
    #[serde(default)]
    pub recent_operator_notifications: Vec<OperatorNotificationRecord>,
    pub recent_brief_count: usize,
    pub recent_event_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authority_class_bridges_from_trust_level() {
        assert_eq!(
            AuthorityClass::from_trust(&TrustLevel::TrustedOperator),
            AuthorityClass::OperatorInstruction
        );
        assert_eq!(
            AuthorityClass::from_trust(&TrustLevel::TrustedSystem),
            AuthorityClass::RuntimeInstruction
        );
        assert_eq!(
            AuthorityClass::from_trust(&TrustLevel::TrustedIntegration),
            AuthorityClass::IntegrationSignal
        );
        assert_eq!(
            AuthorityClass::from_trust(&TrustLevel::UntrustedExternal),
            AuthorityClass::ExternalEvidence
        );
    }

    #[test]
    fn new_messages_carry_authority_class() {
        let message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: Some("operator:jolestar".into()),
            },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "ship it".into(),
            },
        );

        assert_eq!(message.authority_class, AuthorityClass::OperatorInstruction);
        let serialized = serde_json::to_value(&message).unwrap();
        assert_eq!(serialized["authority_class"], "operator_instruction");
    }

    #[test]
    fn legacy_messages_without_authority_class_deserialize_from_trust_bridge() {
        let legacy = serde_json::json!({
            "id": "msg-legacy",
            "agent_id": "default",
            "created_at": "2026-04-22T00:00:00Z",
            "kind": "system_tick",
            "origin": {
                "kind": "system",
                "subsystem": "runtime"
            },
            "trust": "trusted_system",
            "priority": "normal",
            "body": {
                "type": "text",
                "text": "resume"
            }
        });

        let message: MessageEnvelope = serde_json::from_value(legacy).unwrap();

        assert_eq!(message.authority_class, AuthorityClass::RuntimeInstruction);
    }

    #[test]
    fn legacy_agent_model_state_deserializes_without_resolved_policy_fields() {
        let legacy = serde_json::json!({
            "source": "runtime_default",
            "runtime_default_model": "anthropic/claude-sonnet-4-6",
            "effective_model": "anthropic/claude-sonnet-4-6",
            "effective_fallback_models": [],
            "override_model": null
        });

        let model: AgentModelState = serde_json::from_value(legacy).unwrap();

        assert_eq!(
            model.effective_model,
            ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap()
        );
        assert_eq!(
            model.resolved_policy.source,
            crate::model_catalog::ModelMetadataSource::UnknownFallback
        );
        assert!(model.available_models.is_empty());
    }

    #[test]
    fn task_kind_serializes_as_stable_snake_case_string() {
        assert_eq!(
            serde_json::to_value(TaskKind::CommandTask).unwrap(),
            serde_json::json!("command_task")
        );
        assert_eq!(
            serde_json::to_value(TaskKind::ChildAgentTask).unwrap(),
            serde_json::json!("child_agent_task")
        );
        assert_eq!(
            serde_json::to_value(TaskKind::SleepJob).unwrap(),
            serde_json::json!("sleep_job")
        );
    }

    #[test]
    fn legacy_task_kind_records_deserialize_without_losing_workspace_mode() {
        let legacy = serde_json::json!({
            "id": "task-legacy",
            "agent_id": "default",
            "kind": "worktree_subagent_task",
            "status": "running",
            "created_at": "2026-04-22T00:00:00Z",
            "updated_at": "2026-04-22T00:00:00Z",
            "parent_message_id": null,
            "summary": "legacy worktree child",
            "detail": null,
            "recovery": null
        });

        let task: TaskRecord = serde_json::from_value(legacy).unwrap();

        assert_eq!(task.kind, TaskKind::WorktreeSubagentTask);
        assert!(task.is_child_agent_task());
        assert_eq!(
            task.child_agent_workspace_mode(),
            Some(ChildAgentWorkspaceMode::Worktree)
        );
    }

    #[test]
    fn runtime_memory_records_default_missing_workspace_id_to_agent_home() {
        let mut brief = serde_json::to_value(BriefRecord::new(
            "default",
            BriefKind::Result,
            "done",
            None,
            None,
        ))
        .unwrap();
        brief.as_object_mut().unwrap().remove("workspace_id");
        let brief: BriefRecord = serde_json::from_value(brief).unwrap();
        assert_eq!(brief.workspace_id, AGENT_HOME_WORKSPACE_ID);

        let mut work_item =
            serde_json::to_value(WorkItemRecord::new("default", "ship", WorkItemState::Open))
                .unwrap();
        work_item.as_object_mut().unwrap().remove("workspace_id");
        let work_item: WorkItemRecord = serde_json::from_value(work_item).unwrap();
        assert_eq!(work_item.workspace_id, AGENT_HOME_WORKSPACE_ID);

        let episode = serde_json::json!({
            "id": "episode-1",
            "agent_id": "default",
            "created_at": "2026-04-20T00:00:00Z",
            "finalized_at": "2026-04-20T00:01:00Z",
            "start_turn_index": 1,
            "end_turn_index": 2,
            "start_message_count": 1,
            "end_message_count": 2,
            "boundary_reason": "hard_turn_cap",
            "work_summary": "summary",
            "scope_hints": [],
            "summary": "episode summary",
            "working_set_files": [],
            "commands": [],
            "verification": [],
            "decisions": [],
            "carry_forward": [],
            "waiting_on": []
        });
        let episode: ContextEpisodeRecord = serde_json::from_value(episode).unwrap();
        assert_eq!(episode.workspace_id, AGENT_HOME_WORKSPACE_ID);
    }

    #[test]
    fn legacy_persistent_identity_maps_to_self_owned_public_named() {
        let legacy = serde_json::json!({
            "agent_id": "default",
            "kind": "default",
            "visibility": "public",
            "durability": "persistent",
            "status": "active",
            "created_at": "2026-04-22T00:00:00Z",
            "updated_at": "2026-04-22T00:00:00Z"
        });
        let record: AgentIdentityRecord = serde_json::from_value(legacy).unwrap();
        let view = AgentIdentityView::from_record(&record, "default");

        assert_eq!(view.ownership, AgentOwnership::SelfOwned);
        assert_eq!(view.profile_preset, AgentProfilePreset::PublicNamed);
        assert!(record.durability.is_some());
    }

    #[test]
    fn legacy_ephemeral_child_identity_maps_to_parent_supervised_private_child() {
        let legacy = serde_json::json!({
            "agent_id": "tmp_child_demo",
            "kind": "child",
            "visibility": "private",
            "durability": "ephemeral",
            "status": "active",
            "parent_agent_id": "default",
            "delegated_from_task_id": "task-1",
            "created_at": "2026-04-22T00:00:00Z",
            "updated_at": "2026-04-22T00:00:00Z"
        });
        let record: AgentIdentityRecord = serde_json::from_value(legacy).unwrap();
        let view = AgentIdentityView::from_record(&record, "default");

        assert_eq!(view.ownership, AgentOwnership::ParentSupervised);
        assert_eq!(view.profile_preset, AgentProfilePreset::PrivateChild);
    }

    #[test]
    fn identity_view_preserves_lineage_separately_from_supervision() {
        let record = AgentIdentityRecord::new(
            "release-bot",
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        )
        .with_lineage_parent_agent_id(Some("default".into()));
        let view = AgentIdentityView::from_record(&record, "default");

        assert_eq!(view.parent_agent_id, None);
        assert_eq!(view.delegated_from_task_id, None);
        assert_eq!(view.lineage_parent_agent_id.as_deref(), Some("default"));
    }

    #[test]
    fn private_child_disables_agent_creation_and_authority_expansion_families() {
        assert!(!AgentProfilePreset::PrivateChild
            .allows_tool_capability_family(ToolCapabilityFamily::AgentCreation));
        assert!(!AgentProfilePreset::PrivateChild
            .allows_tool_capability_family(ToolCapabilityFamily::AuthorityExpanding));
        assert!(AgentProfilePreset::PrivateChild
            .allows_tool_capability_family(ToolCapabilityFamily::LocalEnvironment));
        assert!(AgentProfilePreset::PrivateChild
            .allows_tool_capability_family(ToolCapabilityFamily::ExternalTrigger));
    }

    #[test]
    fn public_named_enables_all_current_tool_capability_families() {
        for family in [
            ToolCapabilityFamily::CoreAgent,
            ToolCapabilityFamily::LocalEnvironment,
            ToolCapabilityFamily::AgentCreation,
            ToolCapabilityFamily::AuthorityExpanding,
            ToolCapabilityFamily::ExternalTrigger,
        ] {
            assert!(AgentProfilePreset::PublicNamed.allows_tool_capability_family(family));
        }
    }
}
