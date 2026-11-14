use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentsMdScope {
    #[serde(alias = "global")]
    UserGlobal,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentsMdLoadStatus {
    Loaded,
    DuplicateOfAgentScope,
    NotFound,
    RootUnavailable,
    #[default]
    NotEvaluated,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct LoadedAgentsMd {
    #[serde(
        default,
        alias = "global_source",
        skip_serializing_if = "Option::is_none"
    )]
    pub user_global_source: Option<AgentsMdSource>,
    #[serde(default)]
    pub user_global_status: AgentsMdLoadStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_source: Option<AgentsMdSource>,
    #[serde(default)]
    pub agent_status: AgentsMdLoadStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_source: Option<AgentsMdSource>,
    #[serde(default)]
    pub workspace_status: AgentsMdLoadStatus,
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
    #[serde(
        default,
        alias = "global_source",
        skip_serializing_if = "Option::is_none"
    )]
    pub user_global_source: Option<AgentsMdSourceView>,
    #[serde(default)]
    pub user_global_status: AgentsMdLoadStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_source: Option<AgentsMdSourceView>,
    #[serde(default)]
    pub agent_status: AgentsMdLoadStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_source: Option<AgentsMdSourceView>,
    #[serde(default)]
    pub workspace_status: AgentsMdLoadStatus,
}

impl From<&LoadedAgentsMd> for LoadedAgentsMdView {
    fn from(value: &LoadedAgentsMd) -> Self {
        Self {
            user_global_source: value
                .user_global_source
                .as_ref()
                .map(AgentsMdSourceView::from),
            user_global_status: effective_agents_md_status(
                value.user_global_source.as_ref(),
                value.user_global_status,
            ),
            agent_source: value.agent_source.as_ref().map(AgentsMdSourceView::from),
            agent_status: effective_agents_md_status(
                value.agent_source.as_ref(),
                value.agent_status,
            ),
            workspace_source: value
                .workspace_source
                .as_ref()
                .map(AgentsMdSourceView::from),
            workspace_status: effective_agents_md_status(
                value.workspace_source.as_ref(),
                value.workspace_status,
            ),
        }
    }
}

fn effective_agents_md_status(
    source: Option<&AgentsMdSource>,
    status: AgentsMdLoadStatus,
) -> AgentsMdLoadStatus {
    if source.is_some() {
        AgentsMdLoadStatus::Loaded
    } else {
        status
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentMemorySource {
    pub path: PathBuf,
    #[serde(default, skip_serializing, skip_deserializing)]
    pub content: String,
    pub truncated: bool,
    pub total_chars: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentMemorySourceView {
    pub path: PathBuf,
    pub truncated: bool,
    pub total_chars: usize,
}

impl From<&AgentMemorySource> for AgentMemorySourceView {
    fn from(value: &AgentMemorySource) -> Self {
        Self {
            path: value.path.clone(),
            truncated: value.truncated,
            total_chars: value.total_chars,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct LoadedAgentMemory {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_source: Option<AgentMemorySource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_source: Option<AgentMemorySource>,
    #[serde(default)]
    pub budget_chars: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct LoadedAgentMemoryView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_source: Option<AgentMemorySourceView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_source: Option<AgentMemorySourceView>,
    #[serde(default)]
    pub budget_chars: usize,
}

impl From<&LoadedAgentMemory> for LoadedAgentMemoryView {
    fn from(value: &LoadedAgentMemory) -> Self {
        Self {
            operator_source: value
                .operator_source
                .as_ref()
                .map(AgentMemorySourceView::from),
            self_source: value.self_source.as_ref().map(AgentMemorySourceView::from),
            budget_chars: value.budget_chars,
        }
    }
}
