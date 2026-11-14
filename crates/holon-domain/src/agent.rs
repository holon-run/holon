use super::instructions::LoadedAgentsMdView;
use super::message::AuthorityClass;
use super::model::{AgentModelResolution, AgentModelSource, AgentModelState};
use super::operator::OperatorNotificationRecord;
use super::skills::SkillsRuntimeView;
use super::state::{
    AgentState, AgentTokenUsageSummary, ClosureDecision, ClosureOutcome, ProviderCooldownWait,
    RuntimePosture, TokenUsage, WaitingReason,
};
use super::task::{ChildAgentWorkspaceMode, TaskHandle};
use super::wake::{ExternalTriggerSummary, WaitConditionSummary};
use super::workspace::{ActiveWorkspaceEntry, WorkspaceOccupancyRecord};
use crate::agent_observation::{AgentLineageChildrenSnapshot, ChildAgentObservabilitySnapshot};
use crate::agent_records::{AgentCanonicalRelationsProjection, AgentCapabilityFamily};
use crate::execution_types::{ExecutionProfile, ExecutionSnapshot};
use crate::model_values::ModelRouteRef;
use crate::model_values::ResolvedRuntimeModelPolicy;
use chrono::{DateTime, Utc};
use clap::ValueEnum;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use unicode_casefold::UnicodeCaseFold;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Default,
    Named,
    Child,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentProfilePreset {
    PrivateChild,
    PublicNamed,
}

pub type ToolCapabilityFamily = AgentCapabilityFamily;

impl AgentProfilePreset {
    pub fn label(self) -> &'static str {
        match self {
            Self::PrivateChild => "private_child",
            Self::PublicNamed => "public_named",
        }
    }

    pub fn agent_tool_surface_summary(self) -> &'static str {
        match self {
            Self::PrivateChild => {
                "this profile can message authorized agents but cannot create or invoke them"
            }
            Self::PublicNamed => {
                "CreateAgent creates independent identities; SendAgentMessage sends asynchronously; InvokeAgent returns message-wait or child-result task handles"
            }
        }
    }

    pub fn allows_tool_capability_family(self, family: ToolCapabilityFamily) -> bool {
        match self {
            Self::PrivateChild => matches!(
                family,
                ToolCapabilityFamily::CoreAgent
                    | ToolCapabilityFamily::LocalEnvironment
                    | ToolCapabilityFamily::Web
                    | ToolCapabilityFamily::ExternalTrigger
            ),
            Self::PublicNamed => matches!(
                family,
                ToolCapabilityFamily::CoreAgent
                    | ToolCapabilityFamily::LocalEnvironment
                    | ToolCapabilityFamily::Web
                    | ToolCapabilityFamily::AgentCreation
                    | ToolCapabilityFamily::AuthorityExpanding
                    | ToolCapabilityFamily::ExternalTrigger
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentDurability {
    Persistent,
    Ephemeral,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentLifecycleDisposition {
    #[default]
    Retain,
    DeleteOnTerminal,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentRegistryStatus {
    Active,
    Deleting,
    #[serde(alias = "archived")]
    Deleted,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AgentIdentityRecord {
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub kind: AgentKind,
    pub visibility: AgentVisibility,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ownership: Option<AgentOwnership>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_preset: Option<AgentProfilePreset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub durability: Option<AgentDurability>,
    pub status: AgentRegistryStatus,
    #[serde(default)]
    pub revision: u64,
    /// Monotonic incarnation counter for this agent id. Deletion does not
    /// reset it: recreating a fully deleted id produces incarnation + 1 so
    /// historical evidence stays distinguishable from the new incarnation.
    #[serde(default = "default_agent_incarnation")]
    pub incarnation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineage_parent_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_from_task_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(
        default,
        alias = "archived_at",
        skip_serializing_if = "Option::is_none"
    )]
    pub deleted_at: Option<DateTime<Utc>>,
}

fn default_agent_incarnation() -> u64 {
    1
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentCreateStage {
    Reserved,
    Profiled,
    Resolved,
    Degraded,
    Bootstrapped,
    /// Bootstrap was submitted and runs in the background; poll the agent
    /// detail or the create receipt job for the terminal state.
    Provisioning,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentBootstrapStatus {
    Ready,
    Degraded,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentBootstrapStepStatus {
    Pending,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AgentBootstrapStep {
    pub status: AgentBootstrapStepStatus,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub updated_at: DateTime<Utc>,
}

impl AgentBootstrapStep {
    pub fn pending(now: DateTime<Utc>) -> Self {
        Self {
            status: AgentBootstrapStepStatus::Pending,
            attempts: 0,
            last_error: None,
            updated_at: now,
        }
    }

    pub fn succeeded(now: DateTime<Utc>) -> Self {
        Self {
            status: AgentBootstrapStepStatus::Succeeded,
            attempts: 0,
            last_error: None,
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentBootstrapInitialMessage {
    pub message_id: String,
    pub text: String,
    pub authority_class: AuthorityClass,
    pub creator_agent_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentBootstrapWorkspaceState {
    pub attached_workspaces: Vec<String>,
    pub execution_profile: ExecutionProfile,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inherited_model_override: Option<ModelRouteRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inherited_model_override_reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inherited_model_override_service_tier: Option<crate::model_values::ServiceTier>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentBootstrapDesiredState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_agent_home: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<AgentBootstrapWorkspaceState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_resolution: Option<AgentModelResolution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_message: Option<AgentBootstrapInitialMessage>,
    /// User-initiated creation requested the default bootstrap
    /// initialization message because no explicit `initial_message` was
    /// provided. The bootstrap executor synthesizes it; legacy and repair
    /// records never set it, so reconciles of pre-existing agents cannot
    /// invent initialization turns.
    #[serde(default, skip_serializing_if = "is_default_initial_message_unset")]
    pub default_initial_message: bool,
}

fn is_default_initial_message_unset(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentBootstrapRecord {
    pub agent_id: String,
    /// Absent only on legacy bootstrap records; never infer their generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incarnation: Option<u64>,
    pub desired: AgentBootstrapDesiredState,
    pub template: AgentBootstrapStep,
    pub runtime: AgentBootstrapStep,
    pub workspace: AgentBootstrapStep,
    pub model: AgentBootstrapStep,
    pub initial_message: AgentBootstrapStep,
    #[serde(default)]
    pub revision: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AgentBootstrapSummary {
    pub status: AgentBootstrapStatus,
    pub template: AgentBootstrapStep,
    pub runtime: AgentBootstrapStep,
    pub workspace: AgentBootstrapStep,
    pub model: AgentBootstrapStep,
    pub initial_message: AgentBootstrapStep,
}

impl AgentBootstrapRecord {
    pub fn new(agent_id: impl Into<String>, desired: AgentBootstrapDesiredState) -> Self {
        let now = Utc::now();
        Self {
            agent_id: agent_id.into(),
            incarnation: None,
            template: AgentBootstrapStep::pending(now),
            runtime: AgentBootstrapStep::pending(now),
            workspace: if desired.workspace.is_some() {
                AgentBootstrapStep::pending(now)
            } else {
                AgentBootstrapStep::succeeded(now)
            },
            model: if desired.model_resolution.is_some() {
                AgentBootstrapStep::pending(now)
            } else {
                AgentBootstrapStep::succeeded(now)
            },
            initial_message: if desired.initial_message.is_some() {
                AgentBootstrapStep::pending(now)
            } else if desired.default_initial_message {
                AgentBootstrapStep::pending(now)
            } else {
                AgentBootstrapStep::succeeded(now)
            },
            desired,
            revision: 0,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn for_incarnation(mut self, incarnation: u64) -> Self {
        self.incarnation = Some(incarnation);
        let message_id = self.initial_message_id();
        if let Some(initial) = &mut self.desired.initial_message {
            initial.message_id = message_id;
        }
        self
    }

    pub fn initial_message_id(&self) -> String {
        match self.incarnation {
            Some(incarnation) => {
                format!("agent_bootstrap_message:{}:{incarnation}", self.agent_id)
            }
            None => format!("agent_bootstrap_message:{}", self.agent_id),
        }
    }

    pub fn summary(&self) -> AgentBootstrapSummary {
        AgentBootstrapSummary {
            status: self.summary_status(),
            template: self.template.clone(),
            runtime: self.runtime.clone(),
            workspace: self.workspace.clone(),
            model: self.model.clone(),
            initial_message: self.initial_message.clone(),
        }
    }

    pub fn into_summary(self) -> AgentBootstrapSummary {
        AgentBootstrapSummary {
            status: self.summary_status(),
            template: self.template,
            runtime: self.runtime,
            workspace: self.workspace,
            model: self.model,
            initial_message: self.initial_message,
        }
    }

    fn summary_status(&self) -> AgentBootstrapStatus {
        let steps = [
            &self.template,
            &self.runtime,
            &self.workspace,
            &self.model,
            &self.initial_message,
        ];
        if steps
            .iter()
            .all(|step| step.status == AgentBootstrapStepStatus::Succeeded)
        {
            AgentBootstrapStatus::Ready
        } else {
            AgentBootstrapStatus::Degraded
        }
    }

    /// True when the frontier (first not-yet-succeeded) step never recorded
    /// a result. This identifies bootstraps interrupted before the current
    /// step was attempted; steps that already recorded a failure are left
    /// for an explicit repair instead of a startup sweep.
    pub fn frontier_step_pending(&self) -> bool {
        [
            &self.template,
            &self.runtime,
            &self.workspace,
            &self.model,
            &self.initial_message,
        ]
        .into_iter()
        .find(|step| step.status != AgentBootstrapStepStatus::Succeeded)
        .is_some_and(|step| step.status == AgentBootstrapStepStatus::Pending)
    }

    pub fn legacy_ready(agent_id: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            agent_id: agent_id.into(),
            incarnation: None,
            desired: AgentBootstrapDesiredState {
                template: None,
                catalog_agent_home: None,
                workspace: None,
                model_resolution: None,
                initial_message: None,
                default_initial_message: false,
            },
            template: AgentBootstrapStep::succeeded(now),
            runtime: AgentBootstrapStep::succeeded(now),
            workspace: AgentBootstrapStep::succeeded(now),
            model: AgentBootstrapStep::succeeded(now),
            initial_message: AgentBootstrapStep::succeeded(now),
            revision: 0,
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AgentCreateReceipt {
    pub receipt_id: String,
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub display_name: String,
    pub stage: AgentCreateStage,
    pub lifecycle: AgentRegistryStatus,
    pub created: bool,
    /// Background job observing the bootstrap when the receipt is returned
    /// in the `Provisioning` stage. Absent for terminal stages and for
    /// callers that bootstrap inline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    pub bootstrap: AgentBootstrapSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AgentCreateResult {
    pub identity: AgentIdentityView,
    pub receipt: AgentCreateReceipt,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct CreateAgentRequest {
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_message: Option<String>,
    pub authority_class: AuthorityClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_resolution: Option<AgentModelResolution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineage_parent_agent_id: Option<String>,
    #[serde(default)]
    pub inherit_parent_runtime: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InvokeAgentTarget {
    ExistingAgent {
        agent_id: String,
        /// Zero explicitly disables the deadline, which is fixed at task creation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_timeout_ms: Option<u64>,
    },
    NewSubagent {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        template: Option<String>,
        #[serde(default)]
        workspace_mode: ChildAgentWorkspaceMode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_resolution: Option<AgentModelResolution>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct InvokeAgentRequest {
    pub target: InvokeAgentTarget,
    pub message: String,
    pub authority_class: AuthorityClass,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AgentInvocationReceipt {
    pub agent_id: String,
    pub created: bool,
    pub task_handle: TaskHandle,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AgentDetail {
    pub identity: AgentIdentityView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub display_name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub canonical_relations: AgentCanonicalRelationsProjection,
    #[serde(
        default,
        skip_serializing_if = "AgentLineageChildrenSnapshot::is_empty"
    )]
    pub lineage_children: AgentLineageChildrenSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootstrap: Option<AgentBootstrapSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deletion: Option<AgentDeletionJob>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct AgentTreeNode {
    pub agent: AgentListEntry,
    pub canonical_relations: AgentCanonicalRelationsProjection,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<AgentTreeNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct AgentTreeProjection {
    #[serde(default)]
    pub roots: Vec<AgentTreeNode>,
}

impl AgentTreeProjection {
    pub fn into_agent_entries(self) -> Vec<AgentListEntry> {
        fn append(node: AgentTreeNode, entries: &mut Vec<AgentListEntry>) {
            entries.push(node.agent);
            for child in node.children {
                append(child, entries);
            }
        }

        let mut entries = Vec::new();
        for root in self.roots {
            append(root, &mut entries);
        }
        entries
    }
}

pub fn normalize_agent_name(value: &str) -> anyhow::Result<String> {
    let name = value.trim();
    anyhow::ensure!(!name.is_empty(), "agent name must not be empty");
    anyhow::ensure!(
        name.chars().count() <= 64,
        "agent name must be at most 64 characters"
    );
    anyhow::ensure!(
        !name
            .chars()
            .any(|character| { character.is_control() || matches!(character, '/' | '\\' | ':') }),
        "agent name must not contain control characters or path separators"
    );
    Ok(name.to_string())
}

pub fn agent_name_key(value: &str) -> String {
    value.case_fold().collect()
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
            name: None,
            kind,
            visibility,
            ownership: Some(ownership),
            profile_preset: Some(profile_preset),
            durability: None,
            status: AgentRegistryStatus::Active,
            revision: 0,
            incarnation: 1,
            parent_agent_id,
            lineage_parent_agent_id: None,
            delegated_from_task_id,
            created_at: now,
            updated_at: now,
            deleted_at: None,
        }
    }

    pub fn display_name(&self) -> String {
        self.name.clone().unwrap_or_else(|| self.agent_id.clone())
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentDeletionStatus {
    Pending,
    Running,
    RetryableFailed,
    Completed,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentDeletionMode {
    #[default]
    Delete,
    CleanupRepair,
    /// Supervision-authorized deletion that must preserve accepted work.
    ParentCleanup,
}

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum AgentDeletionPhase {
    Fence,
    Quiesce,
    Ingress,
    Scheduler,
    Workspace,
    Archive,
    Index,
    Home,
    Finalize,
}

impl AgentDeletionPhase {
    /// All phases in execution order.
    pub const ALL: [Self; 9] = [
        Self::Fence,
        Self::Quiesce,
        Self::Ingress,
        Self::Scheduler,
        Self::Workspace,
        Self::Archive,
        Self::Index,
        Self::Home,
        Self::Finalize,
    ];

    /// The next phase in execution order, or `None` after `Finalize`.
    pub fn next(self) -> Option<Self> {
        let idx = Self::ALL.iter().position(|p| *p == self)?;
        Self::ALL.get(idx + 1).copied()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AgentDeletionJob {
    pub deletion_id: String,
    pub agent_id: String,
    #[serde(default)]
    pub mode: AgentDeletionMode,
    pub status: AgentDeletionStatus,
    pub phase: AgentDeletionPhase,
    pub requested_by: String,
    pub expected_identity_revision: u64,
    pub cascade_private_children: bool,
    /// Explicit user-retained export, outside the deleted AgentHome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_path: Option<PathBuf>,
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AgentIdentityView {
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip, default = "default_agent_kind")]
    #[schemars(skip)]
    pub kind: AgentKind,
    #[serde(skip, default = "default_agent_visibility")]
    #[schemars(skip)]
    pub visibility: AgentVisibility,
    #[serde(skip, default = "default_agent_ownership")]
    #[schemars(skip)]
    pub ownership: AgentOwnership,
    #[serde(skip, default = "default_agent_profile_preset")]
    #[schemars(skip)]
    pub profile_preset: AgentProfilePreset,
    /// True when this identity satisfies the backend rename guard (public
    /// and self-owned). UI surfaces use this to
    /// show the rename entry point; the rename endpoint stays authoritative.
    #[serde(default)]
    pub can_rename: bool,
    pub status: AgentRegistryStatus,
    pub is_default_agent: bool,
    /// Monotonic incarnation counter; 1 for the original identity and +1
    /// for every explicit recreation of the same agent id after a fully
    /// completed deletion.
    #[serde(default = "default_agent_incarnation")]
    pub incarnation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineage_parent_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_from_task_id: Option<String>,
}

fn default_agent_kind() -> AgentKind {
    AgentKind::Named
}

fn default_agent_visibility() -> AgentVisibility {
    AgentVisibility::Public
}

fn default_agent_ownership() -> AgentOwnership {
    AgentOwnership::SelfOwned
}

fn default_agent_profile_preset() -> AgentProfilePreset {
    AgentProfilePreset::PublicNamed
}

impl AgentIdentityView {
    /// Consume an already-owned identity without duplicating its text fields.
    pub fn from_owned_record(record: AgentIdentityRecord, default_agent_id: &str) -> Self {
        let ownership = record.ownership();
        let profile_preset = record.profile_preset();
        let is_default_agent = record.agent_id == default_agent_id;
        Self {
            agent_id: record.agent_id,
            name: record.name,
            kind: record.kind,
            visibility: record.visibility,
            ownership,
            profile_preset,
            can_rename: record.visibility == AgentVisibility::Public
                && ownership == AgentOwnership::SelfOwned,
            status: record.status,
            is_default_agent,
            incarnation: record.incarnation,
            parent_agent_id: record.parent_agent_id,
            lineage_parent_agent_id: record.lineage_parent_agent_id,
            delegated_from_task_id: record.delegated_from_task_id,
        }
    }

    pub fn from_record(record: &AgentIdentityRecord, default_agent_id: &str) -> Self {
        Self {
            agent_id: record.agent_id.clone(),
            name: record.name.clone(),
            kind: record.kind,
            visibility: record.visibility,
            ownership: record.ownership(),
            profile_preset: record.profile_preset(),
            can_rename: record.visibility == AgentVisibility::Public
                && record.ownership() == AgentOwnership::SelfOwned,
            status: record.status,
            is_default_agent: record.agent_id == default_agent_id,
            incarnation: record.incarnation,
            parent_agent_id: record.parent_agent_id.clone(),
            lineage_parent_agent_id: record.lineage_parent_agent_id.clone(),
            delegated_from_task_id: record.delegated_from_task_id.clone(),
        }
    }

    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.agent_id)
    }

    pub fn relation_summary(&self) -> String {
        if let Some(parent_agent_id) = self.lineage_parent_agent_id.as_deref() {
            format!("agent with lineage parent `{parent_agent_id}`")
        } else {
            "independently addressable agent identity".into()
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Booting,
    AwakeIdle,
    AwakeRunning,
    AwaitingTask,
    Asleep,
    #[serde(alias = "paused")]
    Stopped,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentSchedulingPosture {
    Unknown,
    #[serde(alias = "archived")]
    Stopped,
    ActiveTurn,
    HasQueuedInput,
    HasRunnableWork,
    WaitingForTask,
    WaitingForExternal,
    WaitingForOperator,
    Blocked,
    Idle,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AgentPostureProjection {
    pub posture: AgentSchedulingPosture,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

impl Default for AgentPostureProjection {
    fn default() -> Self {
        Self {
            posture: AgentSchedulingPosture::Unknown,
            reason: "posture projection unavailable".into(),
            work_item_id: None,
            task_id: None,
            run_id: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum ControlAction {
    Start,
    Stop,
}

impl ControlAction {
    pub fn canonical(&self) -> Self {
        self.clone()
    }

    pub fn is_start(&self) -> bool {
        matches!(self, Self::Start)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AgentLifecycleHint {
    pub accepts_external_messages: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_hint: Option<String>,
}

impl Default for AgentLifecycleHint {
    fn default() -> Self {
        Self {
            accepts_external_messages: true,
            operator_hint: None,
        }
    }
}

impl AgentLifecycleHint {
    pub const STOPPED_OPERATOR_HINT: &str =
        "agent is administratively stopped; start before new prompts or wakes";

    #[allow(unused_variables)]
    pub fn from_status(agent_id: &str, status: AgentStatus) -> Self {
        if status == AgentStatus::Stopped {
            return Self {
                accepts_external_messages: false,
                operator_hint: Some(Self::STOPPED_OPERATOR_HINT.into()),
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
    pub scheduling_posture: AgentPostureProjection,
    #[serde(default)]
    pub active_task_count: usize,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_wait_conditions: Vec<WaitConditionSummary>,
    #[serde(default)]
    pub active_external_triggers: Vec<ExternalTriggerSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_operator_notifications: Vec<OperatorNotificationRecord>,
    pub recent_brief_count: usize,
    pub recent_event_count: usize,
    /// Active turn-local provider cooldown wait sub-state, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_cooldown_wait: Option<ProviderCooldownWait>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct AgentListModelSummary {
    pub source: AgentModelSource,
    #[schemars(with = "String")]
    pub runtime_default_model: ModelRouteRef,
    #[schemars(with = "String")]
    pub effective_model: ModelRouteRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub requested_model: Option<ModelRouteRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub active_model: Option<ModelRouteRef>,
    #[serde(default)]
    pub fallback_active: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(with = "Vec<String>")]
    pub effective_fallback_models: Vec<ModelRouteRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub override_model: Option<ModelRouteRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub override_reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub override_service_tier: Option<crate::model_values::ServiceTier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_service_tier: Option<crate::model_values::ServiceTier>,
}

impl From<&AgentModelState> for AgentListModelSummary {
    fn from(value: &AgentModelState) -> Self {
        Self {
            source: value.source.clone(),
            runtime_default_model: value.runtime_default_model.clone(),
            effective_model: value.effective_model.clone(),
            requested_model: value.requested_model.clone(),
            active_model: value.active_model.clone(),
            fallback_active: value.fallback_active,
            effective_fallback_models: value.effective_fallback_models.clone(),
            override_model: value.override_model.clone(),
            override_reasoning_effort: value.override_reasoning_effort.clone(),
            override_service_tier: value.override_service_tier,
            effective_service_tier: value.effective_service_tier,
        }
    }
}

impl From<AgentModelState> for AgentListModelSummary {
    fn from(value: AgentModelState) -> Self {
        Self {
            source: value.source,
            runtime_default_model: value.runtime_default_model,
            effective_model: value.effective_model,
            requested_model: value.requested_model,
            active_model: value.active_model,
            fallback_active: value.fallback_active,
            effective_fallback_models: value.effective_fallback_models,
            override_model: value.override_model,
            override_reasoning_effort: value.override_reasoning_effort,
            override_service_tier: value.override_service_tier,
            effective_service_tier: value.effective_service_tier,
        }
    }
}

impl AgentListModelSummary {
    pub fn into_model_state(self) -> AgentModelState {
        AgentModelState {
            source: self.source,
            runtime_default_model: self.runtime_default_model,
            effective_model: self.effective_model,
            requested_model: self.requested_model,
            active_model: self.active_model,
            fallback_active: self.fallback_active,
            effective_fallback_models: self.effective_fallback_models,
            override_model: self.override_model,
            override_reasoning_effort: self.override_reasoning_effort,
            override_service_tier: self.override_service_tier,
            effective_service_tier: self.effective_service_tier,
            resolved_policy: ResolvedRuntimeModelPolicy::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct AgentListEntry {
    pub identity: AgentIdentityView,
    pub status: AgentStatus,
    #[serde(default)]
    pub scheduling_posture: AgentPostureProjection,
    #[serde(default)]
    pub lifecycle: AgentLifecycleHint,
    #[serde(default)]
    pub pending: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_reason: Option<WaitingReason>,
    pub model: AgentListModelSummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_workspace_entry: Option<ActiveWorkspaceEntry>,
}

impl AgentListEntry {
    pub fn from_summary(summary: &AgentSummary) -> Self {
        Self {
            identity: summary.identity.clone(),
            status: summary.agent.status.clone(),
            scheduling_posture: summary.scheduling_posture.clone(),
            lifecycle: summary.lifecycle.clone(),
            pending: summary.agent.pending,
            current_run_id: summary.agent.current_run_id.clone(),
            waiting_reason: summary.closure.waiting_reason,
            model: (&summary.model).into(),
            active_workspace_entry: summary
                .agent
                .active_workspace_entry
                .clone()
                .map(ActiveWorkspaceEntry::without_projection_metadata),
        }
    }

    pub fn into_agent_summary_placeholder(self) -> AgentSummary {
        let mut agent = AgentState::new(self.identity.agent_id.clone());
        agent.status = self.status;
        agent.pending = self.pending;
        agent.current_run_id = self.current_run_id;
        agent.active_workspace_entry = self.active_workspace_entry.clone();
        agent.model_override = self.model.override_model.clone();
        agent.model_override_reasoning_effort = self.model.override_reasoning_effort.clone();

        let runtime_posture = if agent.status == AgentStatus::Asleep {
            RuntimePosture::Sleeping
        } else {
            RuntimePosture::Awake
        };
        let closure = ClosureDecision {
            outcome: if self.waiting_reason.is_some() {
                ClosureOutcome::Waiting
            } else {
                ClosureOutcome::Completed
            },
            waiting_reason: self.waiting_reason,
            work_signal: None,
            runtime_posture,
            evidence: Vec::new(),
        };
        let execution = if let Some(entry) = agent.active_workspace_entry.as_ref() {
            ExecutionSnapshot {
                profile: ExecutionProfile::default(),
                policy: ExecutionProfile::default().policy_snapshot(),
                attached_workspaces: Vec::new(),
                workspace_id: Some(entry.workspace_id.clone()),
                workspace_anchor: entry.workspace_anchor.clone(),
                execution_root: entry.execution_root.clone(),
                cwd: entry.cwd.clone(),
                execution_root_id: Some(entry.execution_root_id.clone()),
                projection_kind: Some(entry.projection_kind),
                access_mode: Some(entry.access_mode),
                worktree_root: None,
                execution_roots: Vec::new(),
            }
        } else {
            ExecutionSnapshot {
                profile: ExecutionProfile::default(),
                policy: ExecutionProfile::default().policy_snapshot(),
                attached_workspaces: Vec::new(),
                workspace_id: None,
                workspace_anchor: PathBuf::new(),
                execution_root: PathBuf::new(),
                cwd: PathBuf::new(),
                execution_root_id: None,
                projection_kind: None,
                access_mode: None,
                worktree_root: None,
                execution_roots: Vec::new(),
            }
        };

        AgentSummary {
            identity: self.identity,
            lifecycle: self.lifecycle,
            scheduling_posture: self.scheduling_posture,
            model: self.model.into_model_state(),
            token_usage: AgentTokenUsageSummary {
                total: TokenUsage::new(0, 0),
                total_model_rounds: 0,
                last_turn: None,
            },
            closure,
            execution,
            agent,
            active_task_count: 0,
            active_workspace_occupancy: None,
            loaded_agents_md: LoadedAgentsMdView::default(),
            skills: SkillsRuntimeView::default(),
            active_children: Vec::new(),
            active_wait_conditions: Vec::new(),
            active_external_triggers: Vec::new(),
            recent_operator_notifications: Vec::new(),
            recent_brief_count: 0,
            recent_event_count: 0,
            provider_cooldown_wait: None,
        }
    }
}
