use super::templates::AgentTemplateCatalogEntry;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SkillScope {
    #[serde(alias = "user")]
    UserGlobal,
    Agent,
    Workspace,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillCatalogEntry {
    pub skill_id: String,
    pub root_id: String,
    pub skill_dir: String,
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
pub enum SkillLoadReason {
    ReadSkillMd,
    RunSkillScript,
    PromptInjection,
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
    pub agent_templates_catalog: Vec<AgentTemplateCatalogEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discovered_roots: Vec<SkillRootView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discoverable_skills: std::sync::Arc<Vec<SkillCatalogEntry>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attached_skills: std::sync::Arc<Vec<SkillCatalogEntry>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_skills: Vec<ActiveSkillRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SkillInstallMode {
    Linked,
    Copied,
}

impl Default for SkillInstallMode {
    fn default() -> Self {
        Self::Linked
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SkillInstallKind {
    Named {
        name: String,
        #[serde(default)]
        mode: SkillInstallMode,
    },
    Local {
        path: PathBuf,
        #[serde(default)]
        mode: SkillInstallMode,
    },
    Remote {
        package: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        skill: Option<String>,
        #[serde(default)]
        mode: SkillInstallMode,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct InstallSkillRequest {
    pub kind: SkillInstallKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AddSkillRequest {
    pub kind: SkillInstallKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoveSkillRequest {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ReconcileSkillRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RefreshCatalogRequest {}

pub type UpdateSkillRequest = ReconcileSkillRequest;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct CheckSkillRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnableSkillRequest {
    pub name: String,
    #[serde(default)]
    pub mode: SkillInstallMode,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DisableSkillRequest {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UninstallSkillRequest {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillOperationResponse {
    pub ok: bool,
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Source kind of a skill root registration.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SkillRootSourceKind {
    /// Global user library (~/.agents/skills).
    UserGlobal,
    /// Agent-local skills (agent_home/skills).
    AgentHome,
    /// Workspace-local skills (workspace/.agents/skills or workspace/.codex/skills).
    Workspace,
}

/// Last scan status of a registered skill root.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SkillRootScanStatus {
    /// Root has never been scanned.
    NeverScanned,
    /// Last scan succeeded at the given timestamp.
    Scanned { at: i64 },
    /// Last scan failed with an error message.
    Failed { error: String },
}

impl Default for SkillRootScanStatus {
    fn default() -> Self {
        Self::NeverScanned
    }
}

/// Filesystem watcher status for a registered skill root.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SkillRootWatchStatus {
    /// No watcher has been started for this root.
    NotWatched,
    /// A watcher is active for this root.
    Watching,
    /// Watcher setup or delivery failed; manual rescan remains available.
    Failed { error: String },
}

impl Default for SkillRootWatchStatus {
    fn default() -> Self {
        Self::NotWatched
    }
}
/// A registered skill root with metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillRootRegistration {
    pub source_kind: SkillRootSourceKind,
    /// Owner agent id for agent-local roots; None for user-global or workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_agent_id: Option<String>,
    /// Root path on disk.
    pub root_path: PathBuf,
    /// Last scan status.
    #[serde(default)]
    pub scan_status: SkillRootScanStatus,
    /// Filesystem watcher status. Watch failures must not make catalog reads fail.
    #[serde(default)]
    pub watch_status: SkillRootWatchStatus,
}
