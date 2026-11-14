use super::serde_helpers::is_false;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AgentTemplateSourceKind {
    Builtin,
    UserGlobal,
    AgentHome,
    Remote,
}

impl AgentTemplateSourceKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::UserGlobal => "user_global",
            Self::AgentHome => "agent_home",
            Self::Remote => "remote",
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        match label {
            "builtin" => Some(Self::Builtin),
            "user_global" | "user" => Some(Self::UserGlobal),
            "agent_home" | "agent" => Some(Self::AgentHome),
            "remote" => Some(Self::Remote),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTemplateCatalogEntry {
    /// Stable source-scoped catalog identifier, such as `user:code-reviewer`.
    pub catalog_id: String,
    /// Preferred selector accepted by agent creation and invocation requests.
    ///
    /// `catalog_id` is also accepted, along with source aliases such as
    /// `user:`, `agent:`, and `remote:`.
    pub template: String,
    /// Human-readable local id after precedence is applied.
    pub template_id: String,
    pub source: AgentTemplateSourceKind,
    /// Filesystem location for local templates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Human-readable name from `template.toml`, fallback to template_id.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Schema identifier from `template.toml` (e.g. `holon.agent_template.v1`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<String>,
    /// Short display summary derived from the template AGENTS.md.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Skill names declared by the template manifest, if any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub included_skills: Vec<String>,
    /// Configured remote source id for remote templates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    /// Branch, tag, or commit used for this catalog entry after source
    /// configuration/default-branch resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_ref: Option<String>,
    /// Immutable provider revision when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_revision: Option<String>,
    /// Full template tree URL for Remote templates (e.g.
    /// `https://github.com/owner/repo/tree/ref/agent_templates/worker`).
    /// `None` for local and builtin sources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
}

/// Detailed template information for a single catalog entry.
///
/// Returned by the template detail resolution path, this model includes
/// the full AGENTS.md content and parsed skill dependencies, suitable for
/// a GUI detail view or daemon API response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTemplateDetail {
    /// Stable source-scoped catalog identifier, such as `user:code-reviewer`.
    pub catalog_id: String,
    /// Preferred selector accepted by agent creation and invocation requests.
    pub template: String,
    /// Human-readable local id after precedence is applied.
    pub template_id: String,
    pub source: AgentTemplateSourceKind,
    /// Filesystem path or repository URL, depending on source kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_location: Option<String>,
    /// Human-readable name from `template.toml`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Summary from `template.toml` or AGENTS.md fallback.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
    /// Schema identifier from `template.toml`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<String>,
    /// Full AGENTS.md content.
    pub agents_md: String,
    /// Skill dependencies declared by the template.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<AgentTemplateSkillDependency>,
}

/// Skill dependency information within a template detail view.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTemplateSkillDependency {
    /// Skill kind: `local`, `github`, or `builtin`.
    pub kind: String,
    /// Skill reference: filesystem path, GitHub package, or builtin name.
    pub reference: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CheckTemplateRequest {
    /// Local filesystem path to a template directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// GitHub tree URL for a template package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstallTemplateRequest {
    /// GitHub tree URL for the template package to install.
    pub github_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoveTemplateRequest {
    /// Template id within the user global library.
    pub template_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
pub struct SyncTemplateRemoteSourcesRequest {
    /// Optional configured source id. When omitted, all enabled remote sources
    /// are synchronized.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    /// Force a refresh even if a later TTL policy would consider the cache
    /// fresh. Currently accepted for forward compatibility.
    #[serde(default, skip_serializing_if = "is_false")]
    pub force: bool,
}
