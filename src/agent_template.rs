use std::{
    env, fs,
    path::{Path, PathBuf},
};

use crate::{
    config::AgentTemplateRemoteSourceConfigFile,
    runtime_db::RuntimeDb,
    types::{
        AgentTemplateCatalogEntry, AgentTemplateDetail, AgentTemplateSkillDependency,
        AgentTemplateSourceKind, SkillInstallKind, SkillInstallMode,
    },
};

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

const TEMPLATE_AGENTS_FILENAME: &str = "AGENTS.md";
const TEMPLATE_SKILLS_FILENAME: &str = "skills.toml";
const TEMPLATE_MANIFEST_FILENAME: &str = "template.toml";
const TEMPLATE_PROVENANCE_FILENAME: &str = "template-provenance.json";
const BUILTIN_TEMPLATE_STATE_FILENAME: &str = ".holon-builtin-template.json";
pub const DEFAULT_AGENT_TEMPLATE_ID: &str = "holon-default";
const MEMORY_SELF_INITIAL: &str = "# Self Memory\n\n";
const MEMORY_OPERATOR_INITIAL: &str = r#"# Operator Memory

Use this file for stable operator preferences that should influence future turns, such as preferred reply language, communication style, naming conventions, tool defaults, and recurring collaboration expectations.

"#;

pub const REQUIRED_AGENT_HOME_GUIDANCE: &str = r#"## Holon Agent Home

- `agent_home` is this agent's default workspace. Use it for agent-local state, notes, memory, and non-project-local work.
- `AGENTS.md` is automatically loaded as concise agent guidance. Keep durable behavior here, not transient plans or copied project docs.
- `memory/self.md` and `memory/operator.md` are curated agent-scoped Markdown memory. They are searched or retrieved on demand and are not the same as always-loaded guidance.
- Use `memory/operator.md` for stable operator preferences such as preferred reply language, communication style, naming conventions, tool defaults, and recurring collaboration expectations.
- `notes/` is ordinary working notes.
- `work/` is for non-project-local work artifacts. Project-scoped files and memory belong in the active project workspace.
- `skills/` is for agent-local skills.
- `.holon/` is runtime-owned state, ledger, index, and cache storage. Do not edit it as ordinary agent-authored files.
"#;

const BUILTIN_TEMPLATES: &[BuiltinTemplate] = &[
    BuiltinTemplate {
        template_id: "holon-default",
        version: 1,
        agents_md: include_str!("../builtin_templates/holon-default/AGENTS.md"),
        template_toml: include_str!("../builtin_templates/holon-default/template.toml"),
        skill_names: &[],
    },
    BuiltinTemplate {
        template_id: "holon-developer",
        version: 1,
        agents_md: include_str!("../builtin_templates/holon-developer/AGENTS.md"),
        template_toml: include_str!("../builtin_templates/holon-developer/template.toml"),
        skill_names: &[],
    },
    BuiltinTemplate {
        template_id: "holon-reviewer",
        version: 1,
        agents_md: include_str!("../builtin_templates/holon-reviewer/AGENTS.md"),
        template_toml: include_str!("../builtin_templates/holon-reviewer/template.toml"),
        skill_names: &[],
    },
    BuiltinTemplate {
        template_id: "holon-release",
        version: 1,
        agents_md: include_str!("../builtin_templates/holon-release/AGENTS.md"),
        template_toml: include_str!("../builtin_templates/holon-release/template.toml"),
        skill_names: &[],
    },
    BuiltinTemplate {
        template_id: "holon-github-solve",
        version: 1,
        agents_md: include_str!("../builtin_templates/holon-github-solve/AGENTS.md"),
        template_toml: include_str!("../builtin_templates/holon-github-solve/template.toml"),
        skill_names: &[
            "ghx",
            "github-issue-solve",
            "github-pr-fix",
            "github-review",
        ],
    },
];

struct BuiltinTemplate {
    template_id: &'static str,
    version: u32,
    agents_md: &'static str,
    template_toml: &'static str,
    skill_names: &'static [&'static str],
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct BuiltinTemplateState {
    template_id: String,
    version: u32,
    content_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TemplateProvenanceSource {
    TemplateId {
        template_id: String,
        path: PathBuf,
    },
    LocalPath {
        path: PathBuf,
    },
    GitHubUrl {
        url: String,
        owner: String,
        repo: String,
        git_ref: String,
        template_path: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TemplateProvenanceRecord {
    pub selector: String,
    pub source: TemplateProvenanceSource,
    pub applied_at: DateTime<Utc>,
    #[serde(default)]
    pub schema_version: Option<String>,
}

/// File-format skill reference as defined in `skills.toml`.
/// Only `local` and `github` are valid in the on-disk format.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum TemplateSkillFileRef {
    Local { path: PathBuf },
    Github { package: String },
}

/// Parsed `skills.toml` manifest.
#[derive(Debug, Deserialize)]
struct TemplateSkillsManifest {
    #[serde(default)]
    skills: Vec<TemplateSkillFileRef>,
}

/// Internal skill reference used throughout template resolution and materialization.
/// The `Builtin` variant exists only for compiled-in skills referenced via
/// `BuiltinTemplate::skill_names`, not in the on-disk `skills.toml` format.
#[derive(Debug, Clone)]
enum TemplateSkillRef {
    Local { path: PathBuf },
    Github { package: String },
    Builtin { name: String },
}

impl From<TemplateSkillFileRef> for TemplateSkillRef {
    fn from(file_ref: TemplateSkillFileRef) -> Self {
        match file_ref {
            TemplateSkillFileRef::Local { path } => TemplateSkillRef::Local { path },
            TemplateSkillFileRef::Github { package } => TemplateSkillRef::Github { package },
        }
    }
}

/// Parsed `template.toml` manifest providing template metadata.
#[derive(Debug, Clone, Deserialize)]
struct TemplateManifest {
    schema: String,
    #[allow(dead_code)]
    id: String,
    name: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    #[allow(dead_code)]
    compatibility: TemplateCompatibility,
}

/// Parsed `[compatibility]` section from `template.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
struct TemplateCompatibility {
    #[serde(default)]
    #[allow(dead_code)]
    holon: Option<String>,
}

struct BuiltinSkill {
    name: &'static str,
    skill_md: &'static str,
    files: &'static [BuiltinSkillFile],
}

struct BuiltinSkillFile {
    path: &'static str,
    content: &'static str,
}

const BUILTIN_SKILLS: &[BuiltinSkill] = &[
    BuiltinSkill {
        name: "ghx",
        skill_md: include_str!("../skills/ghx/SKILL.md"),
        files: &[],
    },
    BuiltinSkill {
        name: "github-issue-solve",
        skill_md: include_str!("../skills/github-issue-solve/SKILL.md"),
        files: &[BuiltinSkillFile {
            path: "references/issue-solve-workflow.md",
            content: include_str!(
                "../skills/github-issue-solve/references/issue-solve-workflow.md"
            ),
        }],
    },
    BuiltinSkill {
        name: "github-pr-fix",
        skill_md: include_str!("../skills/github-pr-fix/SKILL.md"),
        files: &[
            BuiltinSkillFile {
                path: "references/diagnostics.md",
                content: include_str!("../skills/github-pr-fix/references/diagnostics.md"),
            },
            BuiltinSkillFile {
                path: "references/pr-fix-workflow.md",
                content: include_str!("../skills/github-pr-fix/references/pr-fix-workflow.md"),
            },
        ],
    },
    BuiltinSkill {
        name: "github-review",
        skill_md: include_str!("../skills/github-review/SKILL.md"),
        files: &[],
    },
];

#[derive(Debug, Deserialize)]
struct GitHubContentsFileResponse {
    #[serde(rename = "type")]
    kind: String,
    content: Option<String>,
    encoding: Option<String>,
}

/// Entry in a GitHub Contents API directory listing response.
#[derive(Debug, Deserialize)]
struct GitHubContentsDirEntry {
    name: String,
    #[serde(rename = "type")]
    kind: String,
}

/// Parsed `holon-index.toml` repository manifest.
///
/// Present at a remote repository root to declare the collection layout.
/// When absent, discovery falls back to the conventional `agent_templates/` directory.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct HolonIndexManifest {
    #[serde(default)]
    schema: String,
    #[serde(default)]
    collections: HolonIndexCollections,
}

#[allow(dead_code)]
#[derive(Debug, Default, Deserialize)]
struct HolonIndexCollections {
    skills: Option<String>,
    agent_templates: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTemplateRemoteSourceSyncStatus {
    NotSynced,
    Synced,
    Failed,
    Disabled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTemplateRemoteSourceStatus {
    pub source_id: String,
    pub kind: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_ref: Option<String>,
    pub enabled: bool,
    pub status: AgentTemplateRemoteSourceSyncStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_synced_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTemplateCatalogDiagnostic {
    pub source_id: String,
    pub severity: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTemplateRemoteCatalogSnapshot {
    pub catalog: Vec<AgentTemplateCatalogEntry>,
    pub sources: Vec<AgentTemplateRemoteSourceStatus>,
    pub diagnostics: Vec<AgentTemplateCatalogDiagnostic>,
}

#[derive(Debug, Clone)]
struct ResolvedTemplate {
    provenance: TemplateProvenanceSource,
    agents_md: String,
    skill_refs: Vec<TemplateSkillRef>,
    schema_version: Option<String>,
}

pub fn template_provenance_path(agent_home: &Path) -> PathBuf {
    agent_home
        .join(".holon")
        .join("state")
        .join(TEMPLATE_PROVENANCE_FILENAME)
}

pub fn agent_memory_self_path(agent_home: &Path) -> PathBuf {
    agent_home.join("memory").join("self.md")
}

pub fn agent_memory_operator_path(agent_home: &Path) -> PathBuf {
    agent_home.join("memory").join("operator.md")
}

fn builtin_template_state_path(template_dir: &Path) -> PathBuf {
    template_dir.join(BUILTIN_TEMPLATE_STATE_FILENAME)
}

pub fn seed_builtin_templates() -> Result<()> {
    let home_dir = user_home_dir()?;
    seed_builtin_templates_for_home(&home_dir)
}

pub fn seed_builtin_templates_for_home(home_dir: &Path) -> Result<()> {
    let templates_root = templates_root_for_home(home_dir);
    fs::create_dir_all(&templates_root)
        .with_context(|| format!("failed to create {}", templates_root.display()))?;

    for builtin in BUILTIN_TEMPLATES {
        let template_dir = templates_root.join(builtin.template_id);
        let existed_before = template_dir.exists();
        let was_empty_before = if existed_before {
            fs::read_dir(&template_dir)?.next().is_none()
        } else {
            false
        };
        let existing_state = read_builtin_template_state(&template_dir)?;
        let should_write = if !existed_before || was_empty_before {
            true
        } else if let Some(state) = existing_state.as_ref() {
            builtin_template_is_managed(&template_dir, state)?
                && (state.version != builtin.version
                    || state.content_hash != builtin_template_content_hash(builtin))
        } else {
            false
        };
        if should_write {
            write_builtin_template(&template_dir, builtin)?;
        }
    }

    Ok(())
}

pub async fn initialize_agent_home_from_template(
    agent_home: &Path,
    template: &str,
) -> Result<TemplateProvenanceRecord> {
    let home_dir = user_home_dir()?;
    initialize_agent_home_from_template_with_home(agent_home, &home_dir, template).await
}

pub async fn initialize_agent_home_from_template_with_catalog(
    agent_home: &Path,
    home_dir: &Path,
    catalog_agent_home: &Path,
    template: &str,
) -> Result<TemplateProvenanceRecord> {
    initialize_agent_home_from_template_with_home_and_catalog(
        agent_home,
        home_dir,
        Some(catalog_agent_home),
        template,
    )
    .await
}

pub fn initialize_agent_home_without_template(agent_home: &Path) -> Result<()> {
    ensure_agent_home_layout(agent_home)?;
    let agents_md_path = agent_home.join(TEMPLATE_AGENTS_FILENAME);
    create_file_if_missing(
        &agents_md_path,
        render_agent_home_agents_md("", None).as_bytes(),
    )
}

pub async fn initialize_agent_home_from_template_with_home(
    agent_home: &Path,
    home_dir: &Path,
    template: &str,
) -> Result<TemplateProvenanceRecord> {
    initialize_agent_home_from_template_with_home_and_catalog(agent_home, home_dir, None, template)
        .await
}

async fn initialize_agent_home_from_template_with_home_and_catalog(
    agent_home: &Path,
    home_dir: &Path,
    catalog_agent_home: Option<&Path>,
    template: &str,
) -> Result<TemplateProvenanceRecord> {
    let agent_home = agent_home.to_path_buf();
    let existed_before = agent_home.exists();
    let was_empty_before = if existed_before {
        fs::read_dir(&agent_home)?.next().is_none()
    } else {
        false
    };
    if existed_before && !was_empty_before {
        bail!(
            "agent home {} already exists and is not empty; template initialization refuses to overwrite existing agent state",
            agent_home.display()
        );
    }

    if !existed_before {
        fs::create_dir_all(&agent_home)
            .with_context(|| format!("failed to create {}", agent_home.display()))?;
    }

    let result = async {
        ensure_agent_home_layout(&agent_home)?;
        let resolved = resolve_template(
            template,
            home_dir,
            catalog_agent_home.unwrap_or(&agent_home),
        )
        .await?;
        materialize_template(&agent_home, &resolved).await?;
        let record = TemplateProvenanceRecord {
            selector: template.to_string(),
            source: resolved.provenance,
            applied_at: Utc::now(),
            schema_version: resolved.schema_version,
        };
        tracing::info!(
            template = %record.selector,
            schema_version = ?record.schema_version,
            agent_home = %agent_home.display(),
            "template_applied: agent initialized from template"
        );
        let content = serde_json::to_vec_pretty(&record)?;
        fs::write(template_provenance_path(&agent_home), content).with_context(|| {
            format!(
                "failed to write {}",
                template_provenance_path(&agent_home).display()
            )
        })?;
        Ok(record)
    }
    .await;

    if result.is_err() && agent_home.exists() {
        if !existed_before {
            let _ = fs::remove_dir_all(&agent_home);
        } else if was_empty_before {
            let _ = fs::remove_dir_all(&agent_home);
            let _ = fs::create_dir_all(&agent_home);
        }
    }

    result
}

pub async fn ensure_agent_home_agents_md_from_template(
    agent_home: &Path,
    template: &str,
) -> Result<Option<TemplateProvenanceRecord>> {
    let home_dir = user_home_dir()?;
    ensure_agent_home_agents_md_from_template_with_home(agent_home, &home_dir, template).await
}

pub async fn ensure_agent_home_agents_md_from_template_with_catalog(
    agent_home: &Path,
    home_dir: &Path,
    catalog_agent_home: &Path,
    template: &str,
) -> Result<Option<TemplateProvenanceRecord>> {
    ensure_agent_home_agents_md_from_template_with_home_and_catalog(
        agent_home,
        home_dir,
        Some(catalog_agent_home),
        template,
    )
    .await
}

pub async fn ensure_agent_home_agents_md_from_template_with_home(
    agent_home: &Path,
    home_dir: &Path,
    template: &str,
) -> Result<Option<TemplateProvenanceRecord>> {
    ensure_agent_home_agents_md_from_template_with_home_and_catalog(
        agent_home, home_dir, None, template,
    )
    .await
}

async fn ensure_agent_home_agents_md_from_template_with_home_and_catalog(
    agent_home: &Path,
    home_dir: &Path,
    catalog_agent_home: Option<&Path>,
    template: &str,
) -> Result<Option<TemplateProvenanceRecord>> {
    let agent_home = agent_home.to_path_buf();
    fs::create_dir_all(&agent_home)
        .with_context(|| format!("failed to create {}", agent_home.display()))?;
    ensure_agent_home_layout(&agent_home)?;
    let agents_md_path = agent_home.join(TEMPLATE_AGENTS_FILENAME);
    if agents_md_path.exists() {
        return Ok(None);
    }
    let resolved = resolve_template(
        template,
        home_dir,
        catalog_agent_home.unwrap_or(&agent_home),
    )
    .await?;
    let skills_root = agent_home.join("skills");
    let mut created_skill_destinations = Vec::new();

    let result: Result<TemplateProvenanceRecord> = async {
        let agents_md = render_agent_home_agents_md(&resolved.agents_md, None);
        write_file_atomically(&agents_md_path, agents_md.as_bytes())?;
        for skill_ref in &resolved.skill_refs {
            let destination = materialize_skill_ref(&agent_home, &skills_root, skill_ref).await?;
            created_skill_destinations.push(destination);
        }
        let record = TemplateProvenanceRecord {
            selector: template.to_string(),
            source: resolved.provenance,
            applied_at: Utc::now(),
            schema_version: resolved.schema_version,
        };
        tracing::info!(
            template = %record.selector,
            schema_version = ?record.schema_version,
            agent_home = %agent_home.display(),
            "template_applied: agent initialized from template"
        );
        let content = serde_json::to_vec_pretty(&record)?;
        write_file_atomically(&template_provenance_path(&agent_home), &content)?;
        Ok(record)
    }
    .await;

    match result {
        Ok(record) => Ok(Some(record)),
        Err(err) => {
            let _ = fs::remove_file(&agents_md_path);
            let _ = fs::remove_file(template_provenance_path(&agent_home));
            for destination in created_skill_destinations.into_iter().rev() {
                let _ = remove_materialized_skill_destination(&destination);
            }
            Err(err)
        }
    }
}

#[cfg(test)]
fn templates_root() -> Result<PathBuf> {
    Ok(templates_root_for_home(&user_home_dir()?))
}

fn templates_root_for_home(home_dir: &Path) -> PathBuf {
    home_dir.join(".agents").join("agent_templates")
}

pub(crate) fn discover_agent_templates_catalog(
    user_home: Option<&Path>,
    agent_home: &Path,
) -> Vec<AgentTemplateCatalogEntry> {
    let user_templates_root = user_home.map(templates_root_for_home);
    let user_entries = if let Some(root) = user_templates_root.as_deref() {
        discover_local_templates(root, AgentTemplateSourceKind::UserGlobal, false)
    } else {
        Vec::new()
    };
    let agent_home_entries = discover_local_templates(
        &agent_home.join("agent_templates"),
        AgentTemplateSourceKind::AgentHome,
        true,
    );
    let agent_home_template_ids = agent_home_entries
        .iter()
        .map(|entry| entry.template_id.clone())
        .collect::<std::collections::BTreeSet<_>>();

    let mut entries = user_entries
        .into_iter()
        .filter(|entry| !agent_home_template_ids.contains(&entry.template_id))
        .collect::<Vec<_>>();
    let user_template_ids = entries
        .iter()
        .map(|entry| entry.template_id.clone())
        .collect::<std::collections::BTreeSet<_>>();

    for builtin in BUILTIN_TEMPLATES {
        if user_template_ids.contains(builtin.template_id)
            || agent_home_template_ids.contains(builtin.template_id)
        {
            continue;
        }
        entries.push(builtin_template_catalog_entry(builtin));
    }

    entries.extend(agent_home_entries);
    entries.sort_by(|left, right| {
        (
            left.source,
            left.template_id.as_str(),
            left.path.as_ref().map(|path| path.display().to_string()),
        )
            .cmp(&(
                right.source,
                right.template_id.as_str(),
                right.path.as_ref().map(|path| path.display().to_string()),
            ))
    });
    entries
}

fn builtin_template_catalog_entry(builtin: &BuiltinTemplate) -> AgentTemplateCatalogEntry {
    let manifest = parse_template_manifest(builtin.template_toml);
    AgentTemplateCatalogEntry {
        catalog_id: format!("builtin:{}", builtin.template_id),
        template: builtin.template_id.to_string(),
        template_id: builtin.template_id.to_string(),
        source: AgentTemplateSourceKind::Builtin,
        path: None,
        name: manifest
            .as_ref()
            .map(|m| m.name.clone())
            .unwrap_or_else(|| builtin.template_id.to_string()),
        schema_version: manifest.as_ref().map(|m| m.schema.clone()),
        description: manifest
            .as_ref()
            .map(|m| m.summary.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| template_description(builtin.agents_md)),
        included_skills: builtin_template_skills(builtin),
        source_id: None,
        resolved_ref: None,
        resolved_revision: None,
        source_url: None,
    }
}

fn discover_local_templates(
    root: &Path,
    source: AgentTemplateSourceKind,
    use_absolute_selector: bool,
) -> Vec<AgentTemplateCatalogEntry> {
    let Ok(read_dir) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    for entry in read_dir.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(template_id) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if validate_template_id(template_id).is_err() {
            continue;
        }
        if source == AgentTemplateSourceKind::UserGlobal
            && is_managed_seeded_builtin_template(&path, template_id)
        {
            continue;
        }
        let agents_md_path = path.join(TEMPLATE_AGENTS_FILENAME);
        let Ok(agents_md) = fs::read_to_string(&agents_md_path) else {
            continue;
        };
        if agents_md.trim().is_empty() {
            continue;
        }
        let template = if use_absolute_selector {
            path.to_string_lossy().into_owned()
        } else {
            template_id.to_string()
        };
        let manifest = parse_local_template_manifest(&path);
        entries.push(AgentTemplateCatalogEntry {
            catalog_id: format!("{}:{}", source.label(), template_id),
            template,
            template_id: template_id.to_string(),
            source,
            path: Some(path.clone()),
            name: manifest
                .as_ref()
                .map(|m| m.name.clone())
                .unwrap_or_else(|| template_id.to_string()),
            schema_version: manifest.as_ref().map(|m| m.schema.clone()),
            description: manifest
                .as_ref()
                .map(|m| m.summary.clone())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| template_description(&agents_md)),
            included_skills: local_template_skills(&path),
            source_id: None,
            resolved_ref: None,
            resolved_revision: None,
            source_url: None,
        });
    }
    entries
}

pub fn load_remote_template_catalog_snapshot(
    db: &RuntimeDb,
    configured_sources: &std::collections::BTreeMap<String, AgentTemplateRemoteSourceConfigFile>,
) -> Result<AgentTemplateRemoteCatalogSnapshot> {
    let connection = db.connection()?;
    let mut catalog = Vec::new();
    let mut sources = Vec::new();
    let mut diagnostics = Vec::new();

    for (source_id, config) in configured_sources {
        let enabled = config.enabled();
        let Some(row) = load_remote_source_row(&connection, source_id)? else {
            let status = if enabled {
                AgentTemplateRemoteSourceSyncStatus::NotSynced
            } else {
                AgentTemplateRemoteSourceSyncStatus::Disabled
            };
            sources.push(AgentTemplateRemoteSourceStatus {
                source_id: source_id.clone(),
                kind: "github".into(),
                url: config.url.clone(),
                requested_ref: config.git_ref.clone(),
                enabled,
                status,
                last_synced_at: None,
                resolved_ref: None,
                resolved_revision: None,
                error: None,
            });
            continue;
        };
        if enabled {
            catalog.extend(row.catalog.iter().cloned());
        }
        diagnostics.extend(row.diagnostics.iter().cloned());
        let mut status = row.status;
        if !enabled {
            status = AgentTemplateRemoteSourceSyncStatus::Disabled;
        }
        sources.push(AgentTemplateRemoteSourceStatus {
            source_id: source_id.clone(),
            kind: row.kind,
            url: config.url.clone(),
            requested_ref: config.git_ref.clone(),
            enabled,
            status,
            last_synced_at: row.last_synced_at,
            resolved_ref: row.resolved_ref,
            resolved_revision: row.resolved_revision,
            error: row.error,
        });
    }

    Ok(AgentTemplateRemoteCatalogSnapshot {
        catalog,
        sources,
        diagnostics,
    })
}

#[derive(Debug)]
struct RemoteSourceDbRow {
    kind: String,
    status: AgentTemplateRemoteSourceSyncStatus,
    last_synced_at: Option<DateTime<Utc>>,
    resolved_ref: Option<String>,
    resolved_revision: Option<String>,
    catalog: Vec<AgentTemplateCatalogEntry>,
    diagnostics: Vec<AgentTemplateCatalogDiagnostic>,
    error: Option<String>,
}

fn load_remote_source_row(
    connection: &rusqlite::Connection,
    source_id: &str,
) -> Result<Option<RemoteSourceDbRow>> {
    connection
        .query_row(
            "SELECT kind, status, last_synced_at, resolved_ref, resolved_revision, catalog_json, diagnostics_json, error \
             FROM agent_template_remote_source_syncs WHERE source_id = ?1",
            [source_id],
            |row| {
                let status: String = row.get(1)?;
                let last_synced_at: Option<String> = row.get(2)?;
                let catalog_json: String = row.get(5)?;
                let diagnostics_json: String = row.get(6)?;
                Ok((
                    row.get::<_, String>(0)?,
                    status,
                    last_synced_at,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    catalog_json,
                    diagnostics_json,
                    row.get::<_, Option<String>>(7)?,
                ))
            },
        )
        .optional()?
        .map(
            |(
                kind,
                status,
                last_synced_at,
                resolved_ref,
                resolved_revision,
                catalog_json,
                diagnostics_json,
                error,
            )| {
                Ok(RemoteSourceDbRow {
                    kind,
                    status: parse_remote_source_sync_status(&status),
                    last_synced_at: last_synced_at
                        .map(|value| value.parse())
                        .transpose()
                        .context("failed to parse remote source last_synced_at")?,
                    resolved_ref,
                    resolved_revision,
                    catalog: serde_json::from_str(&catalog_json)
                        .context("failed to decode remote template catalog_json")?,
                    diagnostics: serde_json::from_str(&diagnostics_json)
                        .context("failed to decode remote template diagnostics_json")?,
                    error,
                })
            },
        )
        .transpose()
}

fn parse_remote_source_sync_status(value: &str) -> AgentTemplateRemoteSourceSyncStatus {
    match value {
        "synced" => AgentTemplateRemoteSourceSyncStatus::Synced,
        "failed" => AgentTemplateRemoteSourceSyncStatus::Failed,
        "disabled" => AgentTemplateRemoteSourceSyncStatus::Disabled,
        _ => AgentTemplateRemoteSourceSyncStatus::NotSynced,
    }
}

fn remote_source_sync_status_label(status: &AgentTemplateRemoteSourceSyncStatus) -> &'static str {
    match status {
        AgentTemplateRemoteSourceSyncStatus::NotSynced => "not_synced",
        AgentTemplateRemoteSourceSyncStatus::Synced => "synced",
        AgentTemplateRemoteSourceSyncStatus::Failed => "failed",
        AgentTemplateRemoteSourceSyncStatus::Disabled => "disabled",
    }
}

pub async fn sync_agent_template_remote_source(
    db: &RuntimeDb,
    source_id: &str,
    config: &AgentTemplateRemoteSourceConfigFile,
) -> Result<AgentTemplateRemoteSourceStatus> {
    if !config.enabled() {
        let status = AgentTemplateRemoteSourceStatus {
            source_id: source_id.to_string(),
            kind: "github".into(),
            url: config.url.clone(),
            requested_ref: config.git_ref.clone(),
            enabled: false,
            status: AgentTemplateRemoteSourceSyncStatus::Disabled,
            last_synced_at: None,
            resolved_ref: None,
            resolved_revision: None,
            error: None,
        };
        upsert_remote_source_sync(
            db,
            &status,
            &[],
            &[AgentTemplateCatalogDiagnostic {
                source_id: source_id.to_string(),
                severity: "info".into(),
                message: "remote source is disabled".into(),
            }],
        )?;
        return Ok(status);
    }

    let parsed = parse_github_repo_url(&config.url)?;
    let resolved_ref = match config.git_ref.clone().or(parsed.git_ref) {
        Some(value) => value,
        None => fetch_default_branch(&parsed.owner, &parsed.repo).await?,
    };
    let mut catalog =
        discover_github_repo_templates(&parsed.owner, &parsed.repo, &resolved_ref).await?;
    for entry in &mut catalog {
        entry.source_id = Some(source_id.to_string());
        entry.resolved_ref = Some(resolved_ref.clone());
        entry.resolved_revision = None;
        entry.catalog_id = format!("remote:{source_id}:{}", entry.template_id);
        entry.template = entry.catalog_id.clone();
    }
    let now = Utc::now();
    let status = AgentTemplateRemoteSourceStatus {
        source_id: source_id.to_string(),
        kind: "github".into(),
        url: config.url.clone(),
        requested_ref: config.git_ref.clone(),
        enabled: true,
        status: AgentTemplateRemoteSourceSyncStatus::Synced,
        last_synced_at: Some(now),
        resolved_ref: Some(resolved_ref),
        resolved_revision: None,
        error: None,
    };
    upsert_remote_source_sync(db, &status, &catalog, &[])?;
    Ok(status)
}

pub fn record_agent_template_remote_source_sync_failure(
    db: &RuntimeDb,
    source_id: &str,
    config: &AgentTemplateRemoteSourceConfigFile,
    error: &str,
) -> Result<AgentTemplateRemoteSourceStatus> {
    let previous_catalog = db
        .connection()
        .ok()
        .and_then(|connection| {
            load_remote_source_row(&connection, source_id)
                .ok()
                .flatten()
                .map(|row| row.catalog)
        })
        .unwrap_or_default();
    let diagnostic = AgentTemplateCatalogDiagnostic {
        source_id: source_id.to_string(),
        severity: "error".into(),
        message: error.to_string(),
    };
    let status = AgentTemplateRemoteSourceStatus {
        source_id: source_id.to_string(),
        kind: "github".into(),
        url: config.url.clone(),
        requested_ref: config.git_ref.clone(),
        enabled: config.enabled(),
        status: AgentTemplateRemoteSourceSyncStatus::Failed,
        last_synced_at: Some(Utc::now()),
        resolved_ref: None,
        resolved_revision: None,
        error: Some(error.to_string()),
    };
    upsert_remote_source_sync(db, &status, &previous_catalog, &[diagnostic])?;
    Ok(status)
}

fn upsert_remote_source_sync(
    db: &RuntimeDb,
    status: &AgentTemplateRemoteSourceStatus,
    catalog: &[AgentTemplateCatalogEntry],
    diagnostics: &[AgentTemplateCatalogDiagnostic],
) -> Result<()> {
    let status_label = remote_source_sync_status_label(&status.status);
    let last_synced_at = status.last_synced_at.map(|t| t.to_rfc3339());
    let catalog_json = serde_json::to_string(catalog)?;
    let diagnostics_json = serde_json::to_string(diagnostics)?;
    let now = Utc::now().to_rfc3339();
    let created_at = now.clone();
    db.transaction(|tx| {
        tx.execute(
            "INSERT INTO agent_template_remote_source_syncs \
             (source_id, kind, url, requested_ref, enabled, status, last_synced_at, resolved_ref, resolved_revision, catalog_json, diagnostics_json, error, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14) \
             ON CONFLICT(source_id) DO UPDATE SET \
               kind = excluded.kind, url = excluded.url, requested_ref = excluded.requested_ref, \
               enabled = excluded.enabled, status = excluded.status, last_synced_at = excluded.last_synced_at, \
               resolved_ref = excluded.resolved_ref, resolved_revision = excluded.resolved_revision, \
               catalog_json = excluded.catalog_json, diagnostics_json = excluded.diagnostics_json, \
               error = excluded.error, updated_at = excluded.updated_at",
            params![
                status.source_id,
                status.kind,
                status.url,
                status.requested_ref,
                if status.enabled { 1 } else { 0 },
                status_label,
                last_synced_at,
                status.resolved_ref,
                status.resolved_revision,
                catalog_json,
                diagnostics_json,
                status.error,
                created_at,
                now,
            ],
        )?;
        Ok(())
    })
}

fn is_managed_seeded_builtin_template(path: &Path, template_id: &str) -> bool {
    let Some(builtin) = BUILTIN_TEMPLATES
        .iter()
        .find(|builtin| builtin.template_id == template_id)
    else {
        return false;
    };
    let state = match read_builtin_template_state(path) {
        Ok(Some(state)) => state,
        Ok(None) => return false,
        Err(error) => {
            tracing::debug!(
                template_path = %path.display(),
                %error,
                "failed to read seeded builtin template state while building catalog"
            );
            return false;
        }
    };
    if state.template_id != builtin.template_id
        || state.version != builtin.version
        || state.content_hash != builtin_template_content_hash(builtin)
    {
        return false;
    }
    builtin_template_is_managed(path, &state).unwrap_or(false)
}

fn template_description(agents_md: &str) -> String {
    let mut in_html_comment = false;
    for line in agents_md.lines() {
        let trimmed = line.trim();
        let Some(trimmed) = trim_leading_html_comments(trimmed, &mut in_html_comment) else {
            continue;
        };
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        return trimmed.to_string();
    }
    agents_md
        .lines()
        .find_map(|line| {
            let heading = line.trim().trim_start_matches('#').trim();
            (!heading.is_empty()).then(|| heading.to_string())
        })
        .unwrap_or_default()
}

fn trim_leading_html_comments<'a>(mut trimmed: &'a str, in_comment: &mut bool) -> Option<&'a str> {
    loop {
        if *in_comment {
            let end = trimmed.find("-->")?;
            *in_comment = false;
            trimmed = trimmed[end + 3..].trim_start();
            continue;
        }
        let Some(after_start) = trimmed.strip_prefix("<!--") else {
            return Some(trimmed);
        };
        let Some(end) = after_start.find("-->") else {
            *in_comment = true;
            return None;
        };
        trimmed = after_start[end + 3..].trim_start();
    }
}

/// Parse a `template.toml` manifest string (e.g., from a builtin template).
fn parse_template_manifest(content: &str) -> Option<TemplateManifest> {
    toml::from_str(content)
        .map_err(|error| {
            tracing::warn!(%error, "failed to parse template.toml manifest");
        })
        .ok()
}

/// Parse `template.toml` from a local template directory.
fn parse_local_template_manifest(template_dir: &Path) -> Option<TemplateManifest> {
    let manifest_path = template_dir.join(TEMPLATE_MANIFEST_FILENAME);
    fs::read_to_string(&manifest_path)
        .ok()
        .and_then(|content| parse_template_manifest(&content))
}

/// Resolve a catalog entry into a detailed template view.
///
/// Reads AGENTS.md content and skill dependencies for the template, suitable
/// for GUI or daemon API detail responses.
///
#[allow(dead_code)]
pub(crate) fn resolve_agent_template_detail(
    entry: &AgentTemplateCatalogEntry,
) -> Option<AgentTemplateDetail> {
    let (agents_md, skills) = match entry.source {
        AgentTemplateSourceKind::Builtin => {
            let builtin = BUILTIN_TEMPLATES
                .iter()
                .find(|t| t.template_id == entry.template_id)?;
            let skills = builtin
                .skill_names
                .iter()
                .map(|&name| AgentTemplateSkillDependency {
                    kind: "builtin".to_string(),
                    reference: name.to_string(),
                })
                .collect::<Vec<_>>();
            (builtin.agents_md.to_string(), skills)
        }
        AgentTemplateSourceKind::UserGlobal | AgentTemplateSourceKind::AgentHome => {
            let path = entry.path.as_ref()?;
            let agents_md_path = path.join(TEMPLATE_AGENTS_FILENAME);
            let agents_md = match fs::read_to_string(&agents_md_path) {
                Ok(content) => content,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
                Err(error) => {
                    tracing::warn!(
                        template_path = %agents_md_path.display(),
                        %error,
                        "failed to read AGENTS.md for template detail"
                    );
                    return None;
                }
            };
            let skills = parse_skill_refs(path.join(TEMPLATE_SKILLS_FILENAME))
                .unwrap_or_default()
                .into_iter()
                .map(|skill_ref| match skill_ref {
                    TemplateSkillRef::Local { path } => AgentTemplateSkillDependency {
                        kind: "local".to_string(),
                        reference: path.display().to_string(),
                    },
                    TemplateSkillRef::Github { package } => AgentTemplateSkillDependency {
                        kind: "github".to_string(),
                        reference: package,
                    },
                    TemplateSkillRef::Builtin { name } => AgentTemplateSkillDependency {
                        kind: "builtin".to_string(),
                        reference: name,
                    },
                })
                .collect::<Vec<_>>();
            (agents_md, skills)
        }
        AgentTemplateSourceKind::Remote => return None,
    };
    Some(AgentTemplateDetail {
        catalog_id: entry.catalog_id.clone(),
        template: entry.template.clone(),
        template_id: entry.template_id.clone(),
        source: entry.source,
        source_location: entry.path.as_ref().map(|p| p.display().to_string()),
        name: entry.name.clone(),
        summary: entry.description.clone(),
        schema_version: entry.schema_version.clone(),
        agents_md,
        skills,
    })
}

pub(crate) async fn resolve_remote_agent_template_detail(
    entry: &AgentTemplateCatalogEntry,
) -> Result<AgentTemplateDetail> {
    let source_url = entry
        .source_url
        .as_deref()
        .ok_or_else(|| anyhow!("remote template {} has no source URL", entry.catalog_id))?;
    let resolved = resolve_github_template(source_url).await?;
    let skills = resolved
        .skill_refs
        .into_iter()
        .map(|skill_ref| match skill_ref {
            TemplateSkillRef::Local { path } => AgentTemplateSkillDependency {
                kind: "local".to_string(),
                reference: path.display().to_string(),
            },
            TemplateSkillRef::Github { package } => AgentTemplateSkillDependency {
                kind: "github".to_string(),
                reference: package,
            },
            TemplateSkillRef::Builtin { name } => AgentTemplateSkillDependency {
                kind: "builtin".to_string(),
                reference: name,
            },
        })
        .collect::<Vec<_>>();
    Ok(AgentTemplateDetail {
        catalog_id: entry.catalog_id.clone(),
        template: entry.template.clone(),
        template_id: entry.template_id.clone(),
        source: entry.source,
        source_location: entry.source_url.clone(),
        name: entry.name.clone(),
        summary: entry.description.clone(),
        schema_version: resolved
            .schema_version
            .or_else(|| entry.schema_version.clone()),
        agents_md: resolved.agents_md,
        skills,
    })
}

fn local_template_skills(path: &Path) -> Vec<String> {
    match parse_skill_refs(path.join(TEMPLATE_SKILLS_FILENAME)) {
        Ok(skill_refs) => skill_ref_names(skill_refs),
        Err(error) => {
            tracing::warn!(
                template_path = %path.display(),
                %error,
                "failed to load local agent template skills"
            );
            Vec::new()
        }
    }
}

fn builtin_template_skills(builtin: &BuiltinTemplate) -> Vec<String> {
    let mut names: Vec<String> = builtin
        .skill_names
        .iter()
        .map(|&name| name.to_string())
        .collect();
    names.sort();
    names.dedup();
    names
}

fn skill_ref_names(skill_refs: Vec<TemplateSkillRef>) -> Vec<String> {
    let mut names = skill_refs
        .into_iter()
        .map(|skill_ref| match skill_ref {
            TemplateSkillRef::Local { path } => path.display().to_string(),
            TemplateSkillRef::Github { package } => package,
            TemplateSkillRef::Builtin { name } => name,
        })
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

pub(crate) fn user_home_dir() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .or_else(|| {
            env::var_os("USERPROFILE")
                .map(PathBuf::from)
                .filter(|path| !path.as_os_str().is_empty())
        })
        .or_else(|| {
            let drive = env::var_os("HOMEDRIVE")?;
            let path = env::var_os("HOMEPATH")?;
            let mut combined = PathBuf::from(drive);
            combined.push(path);
            if combined.as_os_str().is_empty() {
                None
            } else {
                Some(combined)
            }
        })
        .ok_or_else(|| anyhow!("HOME is not set; cannot resolve ~/.agents/agent_templates"))
}

async fn resolve_template(
    template: &str,
    home_dir: &Path,
    catalog_agent_home: &Path,
) -> Result<ResolvedTemplate> {
    let template = template.trim();
    if template.is_empty() {
        bail!("template selector must not be empty");
    }

    if let Ok(path) = resolve_absolute_template_path(template) {
        return resolve_local_template(
            path,
            TemplateProvenanceSource::LocalPath {
                path: PathBuf::from(template),
            },
        );
    }

    if is_github_tree_url(template)? {
        return resolve_github_template(template).await;
    }

    let entry = resolve_template_catalog_entry(template, home_dir, catalog_agent_home)?;
    resolve_catalog_template(entry, home_dir).await
}

fn resolve_absolute_template_path(template: &str) -> Result<PathBuf> {
    let path = PathBuf::from(template);
    if !path.is_absolute() {
        bail!("template selector is not an absolute path");
    }
    Ok(path)
}

fn validate_template_id(template_id: &str) -> Result<()> {
    if template_id.contains('/') || template_id.contains('\\') {
        bail!("template_id must not be path-like");
    }
    if template_id == "." || template_id == ".." || template_id.contains("..") {
        bail!("template_id must be a simple stable name");
    }
    if !template_id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_'))
    {
        bail!("template_id contains unsupported characters");
    }
    Ok(())
}

fn resolve_template_catalog_entry(
    template: &str,
    home_dir: &Path,
    catalog_agent_home: &Path,
) -> Result<AgentTemplateCatalogEntry> {
    if let Some((source_label, template_id)) = template.split_once(':') {
        let Some(source) = AgentTemplateSourceKind::from_label(source_label) else {
            return Err(unknown_template_error(
                template,
                home_dir,
                catalog_agent_home,
            ));
        };
        validate_template_id(template_id)?;
        return resolve_prefixed_template_catalog_entry(
            source,
            template_id,
            home_dir,
            catalog_agent_home,
        )
        .ok_or_else(|| unknown_template_error(template, home_dir, catalog_agent_home));
    }

    validate_template_id(template)?;
    let catalog = discover_agent_templates_catalog(Some(home_dir), catalog_agent_home);
    catalog
        .into_iter()
        .find(|entry| entry.template_id == template)
        .ok_or_else(|| unknown_template_error(template, home_dir, catalog_agent_home))
}

fn resolve_prefixed_template_catalog_entry(
    source: AgentTemplateSourceKind,
    template_id: &str,
    home_dir: &Path,
    catalog_agent_home: &Path,
) -> Option<AgentTemplateCatalogEntry> {
    match source {
        AgentTemplateSourceKind::Builtin => BUILTIN_TEMPLATES
            .iter()
            .find(|template| template.template_id == template_id)
            .map(builtin_template_catalog_entry),
        AgentTemplateSourceKind::Remote => None,
        AgentTemplateSourceKind::UserGlobal => discover_local_templates(
            &templates_root_for_home(home_dir),
            AgentTemplateSourceKind::UserGlobal,
            false,
        )
        .into_iter()
        .find(|entry| entry.template_id == template_id),
        AgentTemplateSourceKind::AgentHome => discover_local_templates(
            &catalog_agent_home.join("agent_templates"),
            AgentTemplateSourceKind::AgentHome,
            true,
        )
        .into_iter()
        .find(|entry| entry.template_id == template_id),
    }
}

async fn resolve_catalog_template(
    entry: AgentTemplateCatalogEntry,
    home_dir: &Path,
) -> Result<ResolvedTemplate> {
    match entry.source {
        AgentTemplateSourceKind::Builtin => resolve_builtin_template(&entry.template_id, home_dir),
        AgentTemplateSourceKind::Remote => {
            let source_url = entry
                .source_url
                .ok_or_else(|| anyhow!("remote template {} has no source URL", entry.catalog_id))?;
            resolve_github_template(&source_url).await
        }
        AgentTemplateSourceKind::UserGlobal | AgentTemplateSourceKind::AgentHome => {
            let path = entry.path.clone().ok_or_else(|| {
                anyhow!(
                    "template catalog entry {} has no local source path",
                    entry.catalog_id
                )
            })?;
            let mut resolved = resolve_local_template(
                path.clone(),
                TemplateProvenanceSource::TemplateId {
                    template_id: entry.template_id.clone(),
                    path,
                },
            )?;
            // Seeded builtin templates carry compiled-in skills that cannot be
            // expressed in skills.toml. Merge them so they are materialized.
            if let Some(builtin) = BUILTIN_TEMPLATES
                .iter()
                .find(|t| t.template_id == entry.template_id)
            {
                for &name in builtin.skill_names {
                    resolved.skill_refs.push(TemplateSkillRef::Builtin {
                        name: name.to_string(),
                    });
                }
            }
            Ok(resolved)
        }
    }
}

fn resolve_builtin_template(template_id: &str, home_dir: &Path) -> Result<ResolvedTemplate> {
    let builtin = BUILTIN_TEMPLATES
        .iter()
        .find(|builtin| builtin.template_id == template_id)
        .ok_or_else(|| anyhow!("unknown builtin template id {template_id}"))?;
    let skill_refs: Vec<TemplateSkillRef> = builtin
        .skill_names
        .iter()
        .map(|&name| TemplateSkillRef::Builtin {
            name: name.to_string(),
        })
        .collect();
    Ok(ResolvedTemplate {
        provenance: TemplateProvenanceSource::TemplateId {
            template_id: template_id.to_string(),
            path: templates_root_for_home(home_dir).join(template_id),
        },
        agents_md: builtin.agents_md.to_string(),
        skill_refs,
        schema_version: parse_template_manifest(builtin.template_toml).map(|m| m.schema),
    })
}

fn unknown_template_error(
    template: &str,
    home_dir: &Path,
    catalog_agent_home: &Path,
) -> anyhow::Error {
    let catalog = discover_agent_templates_catalog(Some(home_dir), catalog_agent_home);
    let known = if catalog.is_empty() {
        "none".to_string()
    } else {
        catalog
            .iter()
            .map(|entry| {
                let source = entry
                    .path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| entry.source.label().to_string());
                format!(
                    "{} (id={}, source={source})",
                    entry.catalog_id, entry.template_id
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    anyhow!("unknown template selector: {template}; known template ids/catalog ids: {known}")
}

fn resolve_local_template(
    path: PathBuf,
    provenance: TemplateProvenanceSource,
) -> Result<ResolvedTemplate> {
    if !path.is_dir() {
        bail!("template directory {} does not exist", path.display());
    }
    let agents_md_path = path.join(TEMPLATE_AGENTS_FILENAME);
    let agents_md = fs::read_to_string(&agents_md_path)
        .with_context(|| format!("failed to read {}", agents_md_path.display()))?;
    if agents_md.trim().is_empty() {
        bail!("template {} has an empty AGENTS.md", path.display());
    }
    let skill_refs = parse_skill_refs(path.join(TEMPLATE_SKILLS_FILENAME))?;

    Ok(ResolvedTemplate {
        provenance,
        agents_md,
        skill_refs,
        schema_version: read_template_schema_version(&path),
    })
}

fn read_template_schema_version(template_dir: &Path) -> Option<String> {
    let manifest_path = template_dir.join(TEMPLATE_MANIFEST_FILENAME);
    let content = fs::read_to_string(&manifest_path).ok()?;
    parse_template_manifest(&content).map(|m| m.schema)
}

fn parse_skill_refs(path: PathBuf) -> Result<Vec<TemplateSkillRef>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let manifest: TemplateSkillsManifest =
        toml::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))?;
    let skill_refs: Vec<TemplateSkillRef> = manifest.skills.into_iter().map(Into::into).collect();
    for skill_ref in &skill_refs {
        match skill_ref {
            TemplateSkillRef::Github { package } => {
                template_github_skill_install_kind(package)?;
            }
            TemplateSkillRef::Builtin { name } => {
                if builtin_skill(name).is_none() {
                    bail!("unknown builtin skill ref: {name}");
                }
            }
            TemplateSkillRef::Local { .. } => {}
        }
    }
    Ok(skill_refs)
}

fn template_github_skill_install_kind(package: &str) -> Result<SkillInstallKind> {
    validate_template_github_skill_package(package)?;
    let (remote_package, skill) = split_template_github_skill_package(package)?;
    Ok(SkillInstallKind::Remote {
        package: remote_package.to_string(),
        skill: skill.map(str::to_string),
        mode: SkillInstallMode::Linked,
    })
}

fn split_template_github_skill_package(package: &str) -> Result<(&str, Option<&str>)> {
    let Some(at_index) = package.rfind('@') else {
        return Ok((package, None));
    };
    let remote_package = &package[..at_index];
    if at_index == 0 || !remote_package.contains('/') {
        return Ok((package, None));
    }

    let skill = &package[at_index + 1..];
    validate_template_github_skill_package(remote_package)?;
    validate_template_github_skill_name(skill)?;
    Ok((remote_package, Some(skill)))
}

fn validate_template_github_skill_package(package: &str) -> Result<()> {
    if package.trim().is_empty() {
        bail!("github skill ref package must not be empty");
    }
    if package.trim() != package {
        bail!("github skill ref package must not contain leading or trailing whitespace");
    }
    if package.starts_with('-') {
        bail!("github skill ref package must not start with '-'");
    }
    if package
        .chars()
        .any(|ch| ch.is_control() || ch.is_ascii_whitespace())
    {
        bail!("github skill ref package must not contain whitespace or control characters");
    }
    Ok(())
}

fn validate_template_github_skill_name(skill: &str) -> Result<()> {
    if skill.is_empty()
        || skill == "."
        || skill == ".."
        || skill.contains('/')
        || skill.contains('\\')
    {
        bail!("github skill ref skill name must be a plain skill directory name");
    }
    validate_template_github_skill_package(skill)?;
    Ok(())
}

fn read_builtin_template_state(template_dir: &Path) -> Result<Option<BuiltinTemplateState>> {
    let path = builtin_template_state_path(template_dir);
    if !path.is_file() {
        return Ok(None);
    }
    let content =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    if content.trim().is_empty() {
        return Ok(None);
    }
    let state = serde_json::from_str(&content).ok();
    Ok(state)
}

fn builtin_template_content_hash(template: &BuiltinTemplate) -> String {
    use sha2::{Digest as _, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(TEMPLATE_AGENTS_FILENAME.as_bytes());
    hasher.update(b"\n");
    hasher.update(template.agents_md.as_bytes());
    hasher.update(b"\n");
    hasher.update(TEMPLATE_MANIFEST_FILENAME.as_bytes());
    hasher.update(b"\n");
    hasher.update(template.template_toml.as_bytes());
    if !template.skill_names.is_empty() {
        hasher.update(b"\n");
        hasher.update(b"skill_names\n");
        for &name in template.skill_names {
            hasher.update(name.as_bytes());
            hasher.update(b"\n");
        }
    }
    format!("{:x}", hasher.finalize())
}

fn current_builtin_template_content_hash(template_dir: &Path) -> Result<String> {
    use sha2::{Digest as _, Sha256};

    let agents_md_path = template_dir.join(TEMPLATE_AGENTS_FILENAME);
    let agents_md = fs::read_to_string(&agents_md_path)
        .with_context(|| format!("failed to read {}", agents_md_path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(TEMPLATE_AGENTS_FILENAME.as_bytes());
    hasher.update(b"\n");
    hasher.update(agents_md.as_bytes());
    let manifest_path = template_dir.join(TEMPLATE_MANIFEST_FILENAME);
    if manifest_path.exists() {
        let manifest = fs::read_to_string(&manifest_path)
            .with_context(|| format!("failed to read {}", manifest_path.display()))?;
        hasher.update(b"\n");
        hasher.update(TEMPLATE_MANIFEST_FILENAME.as_bytes());
        hasher.update(b"\n");
        hasher.update(manifest.as_bytes());
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn builtin_template_is_managed(template_dir: &Path, state: &BuiltinTemplateState) -> Result<bool> {
    let current_hash = current_builtin_template_content_hash(template_dir)?;
    Ok(current_hash == state.content_hash)
}

fn write_builtin_template(template_dir: &Path, builtin: &BuiltinTemplate) -> Result<()> {
    fs::create_dir_all(template_dir)
        .with_context(|| format!("failed to create {}", template_dir.display()))?;
    let agents_md_path = template_dir.join(TEMPLATE_AGENTS_FILENAME);
    write_file_atomically(&agents_md_path, builtin.agents_md.as_bytes())?;
    let manifest_path = template_dir.join(TEMPLATE_MANIFEST_FILENAME);
    write_file_atomically(&manifest_path, builtin.template_toml.as_bytes())?;
    // Remove any leftover skills.json from the old format.
    let old_skills_path = template_dir.join("skills.json");
    if old_skills_path.exists() {
        let _ = fs::remove_file(&old_skills_path);
    }
    let state = BuiltinTemplateState {
        template_id: builtin.template_id.to_string(),
        version: builtin.version,
        content_hash: builtin_template_content_hash(builtin),
    };
    let content = serde_json::to_vec_pretty(&state)?;
    write_file_atomically(&builtin_template_state_path(template_dir), &content)?;
    Ok(())
}

fn write_file_atomically(path: &Path, content: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("path {} has no parent directory", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let temp_path = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("write"),
        uuid::Uuid::new_v4().simple()
    ));
    fs::write(&temp_path, content)
        .with_context(|| format!("failed to write {}", temp_path.display()))?;
    if path.exists() {
        fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    fs::rename(&temp_path, path)
        .with_context(|| format!("failed to replace {}", path.display()))?;
    Ok(())
}

fn is_github_tree_url(template: &str) -> Result<bool> {
    let Ok(url) = reqwest::Url::parse(template) else {
        return Ok(false);
    };
    Ok(url.scheme() == "https"
        && url.host_str() == Some("github.com")
        && url
            .path_segments()
            .map(|segments| segments.collect::<Vec<_>>())
            .is_some_and(|segments| segments.len() >= 5 && segments[2] == "tree"))
}

async fn resolve_github_template(template: &str) -> Result<ResolvedTemplate> {
    let url = reqwest::Url::parse(template)?;
    if url.query().is_some() || url.fragment().is_some() {
        bail!("GitHub template URL must not include a query string or fragment");
    }

    let segments = url
        .path_segments()
        .ok_or_else(|| anyhow!("GitHub template URL is missing path segments"))?
        .collect::<Vec<_>>();
    if segments.len() < 5 || segments[2] != "tree" {
        bail!("GitHub template URL must have the form https://github.com/<owner>/<repo>/tree/<ref>/<path-to-template-dir>");
    }

    let owner = segments[0].to_string();
    let repo = segments[1].to_string();
    let ref_and_path = &segments[3..];

    for split in 1..ref_and_path.len() {
        let git_ref = ref_and_path[..split].join("/");
        let template_path = ref_and_path[split..].join("/");
        let agents_md_path = format!("{template_path}/{TEMPLATE_AGENTS_FILENAME}");
        let maybe_agents_md = fetch_github_file(&owner, &repo, &git_ref, &agents_md_path).await?;
        let Some(agents_md) = maybe_agents_md else {
            continue;
        };
        if agents_md.trim().is_empty() {
            bail!("GitHub template {template} resolved to an empty AGENTS.md");
        }
        let skills_path = format!("{template_path}/{TEMPLATE_SKILLS_FILENAME}");
        let skills_toml = fetch_github_file(&owner, &repo, &git_ref, &skills_path).await?;
        let skill_refs = match skills_toml {
            Some(content) => {
                let manifest: TemplateSkillsManifest =
                    toml::from_str(&content).with_context(|| {
                        format!("failed to parse {template}::{TEMPLATE_SKILLS_FILENAME}")
                    })?;
                manifest.skills.into_iter().map(Into::into).collect()
            }
            None => Vec::new(),
        };
        let manifest_path = format!("{template_path}/{TEMPLATE_MANIFEST_FILENAME}");
        let manifest_content = fetch_github_file(&owner, &repo, &git_ref, &manifest_path).await?;
        let schema_version = manifest_content
            .as_deref()
            .and_then(parse_template_manifest)
            .map(|m| m.schema);
        return Ok(ResolvedTemplate {
            provenance: TemplateProvenanceSource::GitHubUrl {
                url: template.to_string(),
                owner,
                repo,
                git_ref,
                template_path,
            },
            agents_md,
            skill_refs,
            schema_version,
        });
    }

    bail!("GitHub template URL did not resolve to a readable template directory: {template}")
}

async fn fetch_github_file(
    owner: &str,
    repo: &str,
    git_ref: &str,
    path: &str,
) -> Result<Option<String>> {
    let base = env::var("HOLON_TEMPLATE_GITHUB_API_BASE")
        .unwrap_or_else(|_| "https://api.github.com".to_string());
    let mut url = reqwest::Url::parse(&format!("{base}/repos/{owner}/{repo}/contents/{path}"))
        .with_context(|| {
            format!("failed to build GitHub contents URL for {owner}/{repo}:{path}")
        })?;
    url.query_pairs_mut().append_pair("ref", git_ref);

    let client = reqwest::Client::builder()
        .build()
        .context("failed to build GitHub template client")?;
    let response = client
        .get(url)
        .header(reqwest::header::USER_AGENT, "holon-template-resolver")
        .send()
        .await
        .with_context(|| {
            format!("failed to fetch GitHub template file {owner}/{repo}:{path}@{git_ref}")
        })?;

    if response.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!(
            "GitHub template fetch failed for {owner}/{repo}:{path}@{git_ref} with status {status}: {body}"
        );
    }

    let payload: GitHubContentsFileResponse = response.json().await.with_context(|| {
        format!("failed to decode GitHub contents response for {owner}/{repo}:{path}@{git_ref}")
    })?;
    if payload.kind != "file" {
        bail!("GitHub template target {owner}/{repo}:{path}@{git_ref} is not a file");
    }
    let content = payload
        .content
        .ok_or_else(|| anyhow!("GitHub contents response omitted file content"))?;
    let encoding = payload.encoding.unwrap_or_default();
    if encoding != "base64" {
        bail!("unsupported GitHub contents encoding: {encoding}");
    }
    let decoded = BASE64_STANDARD
        .decode(content.replace('\n', ""))
        .context("failed to decode GitHub contents payload")?;
    String::from_utf8(decoded)
        .context("GitHub template file is not valid UTF-8")
        .map(Some)
}

/// Parsed components from a GitHub repository URL.
#[derive(Debug)]
struct ParsedGithubRepoUrl {
    owner: String,
    repo: String,
    git_ref: Option<String>,
}

/// Parse a GitHub repository URL into owner, repo, and ref.
///
/// Accepts:
/// - `https://github.com/<owner>/<repo>` (ref left as `None` — resolved later)
/// - `https://github.com/<owner>/<repo>/tree/<ref>` (explicit ref)
///
/// Rejects URLs with additional path segments beyond the ref (those are
/// individual template tree URLs handled by `is_github_tree_url`).
fn parse_github_repo_url(url: &str) -> Result<ParsedGithubRepoUrl> {
    let parsed = reqwest::Url::parse(url).with_context(|| format!("invalid GitHub URL: {url}"))?;
    if parsed.scheme() != "https" || parsed.host_str() != Some("github.com") {
        bail!("not a github.com HTTPS URL: {url}");
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        bail!("GitHub repo URL must not include a query string or fragment");
    }
    let segments = parsed
        .path_segments()
        .ok_or_else(|| anyhow!("GitHub repo URL is missing path segments: {url}"))?
        .collect::<Vec<_>>();
    match segments.as_slice() {
        [owner, repo] if !owner.is_empty() && !repo.is_empty() => Ok(ParsedGithubRepoUrl {
            owner: owner.to_string(),
            repo: repo.to_string(),
            git_ref: None,
        }),
        [owner, repo, "tree", git_ref] if !git_ref.is_empty() => Ok(ParsedGithubRepoUrl {
            owner: owner.to_string(),
            repo: repo.to_string(),
            git_ref: Some(git_ref.to_string()),
        }),
        _ => bail!(
            "GitHub repo URL must be https://github.com/<owner>/<repo> or https://github.com/<owner>/<repo>/tree/<ref>: {url}"
        ),
    }
}

/// List directory entries at a GitHub path via the Contents API.
///
/// Returns `Ok(None)` for a 404 (directory not found).
async fn fetch_github_dir(
    owner: &str,
    repo: &str,
    git_ref: &str,
    path: &str,
) -> Result<Option<Vec<GitHubContentsDirEntry>>> {
    let base = env::var("HOLON_TEMPLATE_GITHUB_API_BASE")
        .unwrap_or_else(|_| "https://api.github.com".to_string());
    let mut url = reqwest::Url::parse(&format!("{base}/repos/{owner}/{repo}/contents/{path}"))
        .with_context(|| format!("failed to build GitHub dir URL for {owner}/{repo}:{path}"))?;
    url.query_pairs_mut().append_pair("ref", git_ref);

    let client = reqwest::Client::builder()
        .build()
        .context("failed to build GitHub template client")?;
    let response = client
        .get(url)
        .header(reqwest::header::USER_AGENT, "holon-template-resolver")
        .send()
        .await
        .with_context(|| {
            format!("failed to list GitHub directory {owner}/{repo}:{path}@{git_ref}")
        })?;

    if response.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!(
            "GitHub directory listing failed for {owner}/{repo}:{path}@{git_ref} with status {status}: {body}"
        );
    }
    let entries = response
        .json::<Vec<GitHubContentsDirEntry>>()
        .await
        .with_context(|| {
            format!("failed to decode GitHub directory listing for {owner}/{repo}:{path}@{git_ref}")
        })?;
    Ok(Some(entries))
}

const DEFAULT_REMOTE_TEMPLATE_DIR: &str = "agent_templates";

/// Subset of the GitHub `GET /repos/{owner}/{repo}` response we care about.
#[derive(Debug, Deserialize)]
struct GitHubRepoInfo {
    default_branch: String,
}

/// Resolve the default branch of a GitHub repository via the REST API.
///
/// Used when a repo URL omits `/tree/<ref>` — avoids hardcoding `main` so
/// repositories on `master`, `develop`, etc. are handled correctly.
async fn fetch_default_branch(owner: &str, repo: &str) -> Result<String> {
    let base = env::var("HOLON_TEMPLATE_GITHUB_API_BASE")
        .unwrap_or_else(|_| "https://api.github.com".to_string());
    let url = reqwest::Url::parse(&format!("{base}/repos/{owner}/{repo}"))
        .with_context(|| format!("failed to build GitHub repo URL for {owner}/{repo}"))?;
    let client = reqwest::Client::builder()
        .build()
        .context("failed to build GitHub template client")?;
    let response = client
        .get(url)
        .header(reqwest::header::USER_AGENT, "holon-template-resolver")
        .send()
        .await
        .with_context(|| format!("failed to fetch repo info for {owner}/{repo}"))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!("GitHub repo info fetch failed for {owner}/{repo} with status {status}: {body}");
    }
    response
        .json::<GitHubRepoInfo>()
        .await
        .map(|info| info.default_branch)
        .with_context(|| format!("failed to decode repo info for {owner}/{repo}"))
}

/// Discover all AgentTemplates under a GitHub repository's conventional
/// `agent_templates/` collection.
pub(crate) async fn discover_github_repo_templates(
    owner: &str,
    repo: &str,
    git_ref: &str,
) -> Result<Vec<AgentTemplateCatalogEntry>> {
    let template_dir = resolve_remote_template_collection_dir(owner, repo, git_ref).await?;

    // List subdirectories under the template collection.
    let entries = match fetch_github_dir(owner, repo, git_ref, &template_dir).await? {
        Some(entries) => entries,
        None => return Ok(Vec::new()),
    };

    let mut catalog_entries = Vec::new();
    for entry in entries {
        if entry.kind != "dir" {
            continue;
        }
        let template_id = entry.name;
        let manifest_path = format!("{template_dir}/{template_id}/{TEMPLATE_MANIFEST_FILENAME}");
        let manifest = match fetch_github_file(owner, repo, git_ref, &manifest_path).await {
            Ok(Some(content)) => parse_template_manifest(&content),
            Ok(None) => None,
            Err(error) => {
                tracing::warn!(
                    template = %template_id,
                    %error,
                    "skipping remote template: failed to fetch template.toml"
                );
                continue;
            }
        };
        let manifest = match manifest {
            Some(m) => m,
            None => {
                tracing::warn!(template = %template_id, "skipping remote template: invalid or missing template.toml");
                continue;
            }
        };
        catalog_entries.push(AgentTemplateCatalogEntry {
            catalog_id: format!("remote:{owner}/{repo}/{template_id}"),
            template: format!("remote:{owner}/{repo}#{template_id}"),
            template_id: template_id.clone(),
            source: AgentTemplateSourceKind::Remote,
            path: None,
            name: manifest.name,
            schema_version: Some(manifest.schema),
            description: manifest.summary,
            included_skills: Vec::new(),
            source_id: None,
            resolved_ref: Some(git_ref.to_string()),
            resolved_revision: None,
            source_url: Some(format!(
                "https://github.com/{owner}/{repo}/tree/{git_ref}/{template_dir}/{template_id}"
            )),
        });
    }
    Ok(catalog_entries)
}

/// Resolve the template collection directory for a remote GitHub repository.
///
/// Tries `holon-index.toml` at the repo root first; when present and parseable,
/// uses its `collections.agent_templates` value. Falls back to
/// [`DEFAULT_REMOTE_TEMPLATE_DIR`] when the index is absent, unreadable, or
/// does not declare a custom collection path.
async fn resolve_remote_template_collection_dir(
    owner: &str,
    repo: &str,
    git_ref: &str,
) -> Result<String> {
    match fetch_github_file(owner, repo, git_ref, "holon-index.toml").await? {
        Some(content) => match toml::from_str::<HolonIndexManifest>(&content) {
            Ok(manifest) if manifest.collections.agent_templates.as_deref().is_some() => {
                Ok(manifest.collections.agent_templates.unwrap())
            }
            _ => Ok(DEFAULT_REMOTE_TEMPLATE_DIR.to_string()),
        },
        None => Ok(DEFAULT_REMOTE_TEMPLATE_DIR.to_string()),
    }
}

/// Discover AgentTemplates from a GitHub repository URL.
///
/// Parses the URL with [`parse_github_repo_url`], resolves the default branch
/// via the GitHub API when no `/tree/<ref>` is specified, and delegates to
/// [`discover_github_repo_templates`]. This is the primary entry point for
/// browsing a remote template library from a repo URL.
pub async fn discover_remote_templates(url: &str) -> Result<Vec<AgentTemplateCatalogEntry>> {
    let parsed = parse_github_repo_url(url)?;
    let git_ref = match parsed.git_ref {
        Some(r) => r,
        None => fetch_default_branch(&parsed.owner, &parsed.repo).await?,
    };
    discover_github_repo_templates(&parsed.owner, &parsed.repo, &git_ref).await
}

async fn materialize_template(agent_home: &Path, template: &ResolvedTemplate) -> Result<()> {
    ensure_agent_home_layout(agent_home)?;
    let agents_md_path = agent_home.join(TEMPLATE_AGENTS_FILENAME);
    if agents_md_path.exists() {
        bail!(
            "{} already exists; template initialization refuses to overwrite live AGENTS.md",
            agents_md_path.display()
        );
    }
    let agents_md = render_agent_home_agents_md(&template.agents_md, None);
    fs::write(&agents_md_path, &agents_md)
        .with_context(|| format!("failed to write {}", agents_md_path.display()))?;

    let skills_root = agent_home.join("skills");
    for skill_ref in &template.skill_refs {
        materialize_skill_ref(agent_home, &skills_root, skill_ref).await?;
    }
    Ok(())
}

pub fn ensure_agent_home_layout(agent_home: &Path) -> Result<()> {
    fs::create_dir_all(agent_home)
        .with_context(|| format!("failed to create {}", agent_home.display()))?;
    for dir in [
        agent_home.join("memory"),
        agent_home.join("notes"),
        agent_home.join("work"),
        agent_home.join("work-items"),
        agent_home.join("skills"),
        agent_home.join(".holon/state"),
        agent_home.join(".holon/ledger"),
        agent_home.join(".holon/indexes"),
        agent_home.join(".holon/cache"),
    ] {
        fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    }
    create_file_if_missing(
        &agent_memory_self_path(agent_home),
        MEMORY_SELF_INITIAL.as_bytes(),
    )?;
    create_file_if_missing(
        &agent_memory_operator_path(agent_home),
        MEMORY_OPERATOR_INITIAL.as_bytes(),
    )?;
    Ok(())
}

fn create_file_if_missing(path: &Path, content: &[u8]) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    write_file_atomically(path, content)
}

fn render_agent_home_agents_md(template_guidance: &str, profile_seed: Option<&str>) -> String {
    let mut sections = Vec::new();
    let template_guidance = template_guidance.trim();
    if !template_guidance.is_empty() {
        sections.push(template_guidance.to_string());
    }
    sections.push(REQUIRED_AGENT_HOME_GUIDANCE.trim().to_string());
    if let Some(profile_seed) = profile_seed.map(str::trim).filter(|seed| !seed.is_empty()) {
        sections.push(profile_seed.to_string());
    }
    format!("{}\n", sections.join("\n\n"))
}

async fn materialize_skill_ref(
    agent_home: &Path,
    skills_root: &Path,
    skill_ref: &TemplateSkillRef,
) -> Result<PathBuf> {
    match skill_ref {
        TemplateSkillRef::Local { path } => materialize_local_skill_ref(skills_root, path),
        TemplateSkillRef::Builtin { name } => materialize_builtin_skill_ref(skills_root, name),
        TemplateSkillRef::Github { package } => materialize_github_skill_ref(agent_home, package),
    }
}

fn materialize_github_skill_ref(agent_home: &Path, package: &str) -> Result<PathBuf> {
    let install_kind = template_github_skill_install_kind(package)?;
    let user_home = user_home_dir()?;
    let skill_name = crate::skills::install_skill_with_user_home(
        agent_home,
        Some(user_home.as_path()),
        &install_kind,
    )?;
    Ok(agent_home.join("skills").join(skill_name))
}

fn builtin_skill(name: &str) -> Option<&'static BuiltinSkill> {
    BUILTIN_SKILLS.iter().find(|skill| skill.name == name)
}

pub fn builtin_skill_names() -> Vec<&'static str> {
    BUILTIN_SKILLS.iter().map(|skill| skill.name).collect()
}

pub fn materialize_builtin_skill_ref(skills_root: &Path, name: &str) -> Result<PathBuf> {
    let skill = builtin_skill(name).ok_or_else(|| anyhow!("unknown builtin skill ref: {name}"))?;
    fs::create_dir_all(skills_root)
        .with_context(|| format!("failed to create {}", skills_root.display()))?;
    let destination = skills_root.join(skill.name);
    if destination.exists() {
        bail!(
            "template skill destination {} already exists",
            destination.display()
        );
    }
    fs::create_dir_all(&destination)
        .with_context(|| format!("failed to create {}", destination.display()))?;
    write_file_atomically(&destination.join("SKILL.md"), skill.skill_md.as_bytes())?;
    for file in skill.files {
        write_file_atomically(&destination.join(file.path), file.content.as_bytes())?;
    }
    Ok(destination)
}

pub fn materialize_local_skill_ref(skills_root: &Path, path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        bail!("local skill ref path must be absolute: {}", path.display());
    }
    if !path.is_dir() {
        bail!(
            "local skill ref directory does not exist: {}",
            path.display()
        );
    }
    let skill_entrypoint = path.join("SKILL.md");
    if !skill_entrypoint.is_file() {
        bail!(
            "local skill ref {} does not contain SKILL.md",
            path.display()
        );
    }
    let skill_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "local skill ref {} has no usable directory name",
                path.display()
            )
        })?;
    fs::create_dir_all(skills_root)
        .with_context(|| format!("failed to create {}", skills_root.display()))?;
    let destination = skills_root.join(skill_name);
    if destination.exists() {
        bail!(
            "template skill destination {} already exists",
            destination.display()
        );
    }
    create_directory_symlink(path, &destination).with_context(|| {
        format!(
            "failed to materialize local skill ref {} -> {}",
            path.display(),
            destination.display()
        )
    })?;
    Ok(destination)
}

pub fn remove_materialized_skill_destination(destination: &Path) -> std::io::Result<()> {
    let metadata = fs::symlink_metadata(destination)?;
    if metadata.file_type().is_symlink() {
        fs::remove_file(destination)
    } else {
        fs::remove_dir_all(destination)
    }
}

#[cfg(unix)]
fn create_directory_symlink(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

#[cfg(windows)]
fn create_directory_symlink(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(src, dst)
}

// ---------------------------------------------------------------------------
// Public API helpers for HTTP handlers
// ---------------------------------------------------------------------------

/// Parse `template.toml` from a local template directory and return it as a
/// JSON value for API responses.
pub fn parse_template_manifest_for_api(template_dir: &Path) -> Option<serde_json::Value> {
    parse_local_template_manifest(template_dir).map(|manifest| {
        serde_json::json!({
            "schema": manifest.schema,
            "id": manifest.id,
            "name": manifest.name,
            "summary": manifest.summary,
        })
    })
}

/// Remove a template from the user global library.
pub fn remove_user_template(user_home: &Path, template_id: &str) -> Result<()> {
    validate_template_id(template_id)?;
    let templates_root = templates_root_for_home(user_home);
    let target = templates_root.join(template_id);
    if !target.is_dir() {
        bail!(
            "template '{}' not found in user global library at {}",
            template_id,
            templates_root.display()
        );
    }
    fs::remove_dir_all(&target)
        .with_context(|| format!("failed to remove {}", target.display()))?;
    tracing::info!(template_id, "removed user global template");
    Ok(())
}

/// Install a template package from a GitHub tree URL into the user global
/// library.
///
/// Expected URL: `https://github.com/<owner>/<repo>/tree/<ref>[/<path>]`
pub fn install_template_from_github(user_home: &Path, github_url: &str) -> Result<String> {
    let source = ParsedGithubTemplateUrl::parse(github_url)?;
    let templates_root = templates_root_for_home(user_home);
    fs::create_dir_all(&templates_root)
        .with_context(|| format!("failed to create {}", templates_root.display()))?;

    let tmp = templates_root.join(format!(
        ".tmp-holon-template-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _guard = TmpDirGuard(&tmp);

    fs::create_dir_all(&tmp).with_context(|| format!("failed to create {}", tmp.display()))?;

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .user_agent(format!("holon/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .context("failed to create HTTP client for template install")?;

    // GitHub refs (branch/tag names) can contain slashes, making the
    // ref/path boundary in a tree URL ambiguous.  Try candidate splits
    // from longest ref to shortest; the first tarball download that
    // succeeds identifies the correct ref.
    let segments: Vec<&str> = source.all_after_tree.split('/').collect();
    let mut bytes = None;
    let mut resolved_path = String::new();

    for i in (1..=segments.len()).rev() {
        let candidate_ref = segments[..i].join("/");
        let candidate_path = if i < segments.len() {
            segments[i..].join("/")
        } else {
            String::new()
        };
        let tarball_url = format!(
            "https://api.github.com/repos/{}/{}/tarball/{}",
            source.owner, source.repo, candidate_ref
        );
        // Forward GITHUB_TOKEN / GH_TOKEN when available so private repos work and
        // the 60 req/hr unauthenticated rate limit is avoided.
        let mut request = client
            .get(&tarball_url)
            .header("Accept", "application/vnd.github+json");
        if let Ok(token) = env::var("GITHUB_TOKEN").or_else(|_| env::var("GH_TOKEN")) {
            request = request.bearer_auth(&token);
        }
        let response = request.send().context("failed to request GitHub tarball")?;
        if response.status().is_success() {
            resolved_path = candidate_path;
            bytes = Some(response.bytes().context("failed to read GitHub tarball body")?);
            break;
        }
    }
    let bytes = bytes.ok_or_else(|| {
        anyhow!(
            "GitHub tarball download failed: no valid ref found for '{}'",
            source.all_after_tree
        )
    })?;
    let tarball_path = tmp.join("archive.tar.gz");
    fs::write(&tarball_path, &bytes)
        .with_context(|| format!("failed to write tarball to {}", tarball_path.display()))?;

    let extract_dir = tmp.join("extracted");
    fs::create_dir_all(&extract_dir)?;
    extract_tarball(&tarball_path, &extract_dir)?;

    // GitHub tarballs contain a single top-level directory.
    let mut search_dir = extract_dir.clone();
    let entries: Vec<_> = fs::read_dir(&search_dir)?.collect::<Result<_, _>>()?;
    if entries.len() == 1 && entries[0].path().is_dir() {
        search_dir = entries[0].path();
    }
    if !resolved_path.is_empty() {
        search_dir = search_dir.join(&resolved_path);
    }
    if !search_dir.is_dir() {
        bail!(
            "template directory not found at '{}' in the downloaded archive",
            resolved_path
        );
    }

    if !search_dir.join(TEMPLATE_MANIFEST_FILENAME).exists()
        && !search_dir.join("AGENTS.md").exists()
    {
        bail!(
            "the specified path does not contain template.toml or AGENTS.md; \
             not a valid template directory"
        );
    }

    let template_id = parse_local_template_manifest(&search_dir)
        .map(|m| m.id)
        .unwrap_or_else(|| {
            search_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("imported")
                .to_string()
        });
    validate_template_id(&template_id)?;

    let destination = templates_root.join(&template_id);
    if destination.exists() {
        fs::remove_dir_all(&destination)
            .with_context(|| format!("failed to clear existing {}", destination.display()))?;
    }
    copy_template_dir(&search_dir, &destination)?;

    tracing::info!(template_id = %template_id, "installed user global template from GitHub");
    Ok(template_id)
}

struct TmpDirGuard<'a>(&'a Path);
impl Drop for TmpDirGuard<'_> {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.0);
    }
}

struct ParsedGithubTemplateUrl {
    owner: String,
    repo: String,
    /// All segments after `tree/`, preserved verbatim so the caller can
    /// resolve refs that contain slashes (e.g. `feat/branch-name`).
    all_after_tree: String,
}

impl ParsedGithubTemplateUrl {
    fn parse(url: &str) -> Result<Self> {
        let url = url.trim();
        let rest = url
            .strip_prefix("https://github.com/")
            .or_else(|| url.strip_prefix("http://github.com/"))
            .ok_or_else(|| anyhow!("github_url must be a https://github.com/ URL"))?;
        let parts: Vec<&str> = rest.split('/').collect();
        if parts.len() < 2 {
            bail!("invalid GitHub URL: expected owner/repo/tree/ref[/path]");
        }
        let owner = parts[0].to_string();
        let repo = parts[1].trim_end_matches(".git").to_string();
        if parts.len() < 4 || parts[2] != "tree" {
            bail!("invalid GitHub URL: expected .../tree/<ref>[/<path>]");
        }
        let all_after_tree = parts[3..].join("/");
        Ok(Self {
            owner,
            repo,
            all_after_tree,
        })
    }
}

fn extract_tarball(tarball_path: &Path, dest: &Path) -> Result<()> {
    let output = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(tarball_path)
        .arg("-C")
        .arg(dest)
        .output()
        .context("failed to run tar")?;
    if !output.status.success() {
        bail!(
            "tar extraction failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

/// Validate a GitHub template URL format without downloading anything.
/// Returns a canonical path string (owner/repo/tree/ref[/path]).
pub fn validate_github_template_url(url: &str) -> Result<String> {
    let parsed = ParsedGithubTemplateUrl::parse(url)?;
    Ok(format!(
        "{}/{}/tree/{}",
        parsed.owner,
        parsed.repo,
        parsed.all_after_tree,
    ))
}

fn copy_template_dir(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if file_type.is_dir() {
            if from.file_name().is_some_and(|n| n == ".git") {
                continue;
            }
            copy_template_dir(&from, &to)?;
        } else if file_type.is_file() {
            fs::copy(&from, &to)?;
        } else {
            bail!(
                "template contains unsupported file type (e.g. symlink): {}",
                from.display()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::{Arc, Mutex},
        thread,
    };

    use tempfile::tempdir;

    use super::*;

    struct EnvGuard {
        key: &'static str,
        old: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: String) -> Self {
            let old = env::var(key).ok();
            env::set_var(key, value);
            Self { key, old }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.old {
                Some(value) => env::set_var(self.key, value),
                None => env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn seed_builtin_templates_is_idempotent() {
        let home = tempdir().unwrap();
        let templates = templates_root_for_home(home.path());

        seed_builtin_templates_for_home(home.path()).unwrap();
        let developer_agents = templates.join("holon-developer/AGENTS.md");
        assert!(developer_agents.is_file());

        fs::write(&developer_agents, "custom").unwrap();
        seed_builtin_templates_for_home(home.path()).unwrap();
        assert_eq!(fs::read_to_string(&developer_agents).unwrap(), "custom");
    }

    #[test]
    fn seed_builtin_templates_writes_prefixed_default_template() {
        let home = tempdir().unwrap();
        let templates = templates_root_for_home(home.path());

        seed_builtin_templates_for_home(home.path()).unwrap();
        let agents_md = templates.join("holon-default/AGENTS.md");
        assert!(agents_md.is_file());
        assert!(fs::read_to_string(agents_md)
            .unwrap()
            .contains("Holon Default Agent"));
    }

    #[test]
    fn seed_builtin_templates_upgrades_managed_builtin_content() {
        let home = tempdir().unwrap();
        let templates = templates_root_for_home(home.path());

        seed_builtin_templates_for_home(home.path()).unwrap();
        let template_dir = templates.join("holon-reviewer");
        let agents_md = template_dir.join("AGENTS.md");
        let state_path = builtin_template_state_path(&template_dir);
        let original = fs::read_to_string(&agents_md).unwrap();
        let mut state: BuiltinTemplateState =
            serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
        state.version = 0;
        fs::write(&state_path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();

        seed_builtin_templates_for_home(home.path()).unwrap();

        assert_eq!(fs::read_to_string(&agents_md).unwrap(), original);
        let upgraded: BuiltinTemplateState =
            serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
        assert_eq!(upgraded.version, 1);
    }

    #[test]
    fn seed_builtin_templates_tolerates_empty_builtin_state_file() {
        let home = tempdir().unwrap();
        let templates = templates_root_for_home(home.path());

        seed_builtin_templates_for_home(home.path()).unwrap();
        let template_dir = templates.join("holon-default");
        fs::write(builtin_template_state_path(&template_dir), "").unwrap();

        seed_builtin_templates_for_home(home.path()).unwrap();

        assert!(template_dir.join("AGENTS.md").is_file());
    }

    #[test]
    fn discover_agent_templates_catalog_lists_stable_selectors_and_shadowing() {
        let user_home = tempdir().unwrap();
        let agent_home = tempdir().unwrap();
        let user_templates = templates_root_for_home(user_home.path());
        seed_builtin_templates_for_home(user_home.path()).unwrap();
        let worker = user_templates.join("worker");
        fs::create_dir_all(&worker).unwrap();
        fs::write(
            worker.join(TEMPLATE_AGENTS_FILENAME),
            "# Worker\n\nDoes worker things\n",
        )
        .unwrap();
        let source_skill = user_home.path().join("source-skill");
        fs::create_dir_all(&source_skill).unwrap();
        fs::write(
            source_skill.join("SKILL.md"),
            "# Source
",
        )
        .unwrap();
        fs::write(
            worker.join(TEMPLATE_SKILLS_FILENAME),
            format!(
                "[[skills]]\nkind = \"local\"\npath = \"{}\"\n",
                source_skill.display()
            ),
        )
        .unwrap();

        let shadowed_default = user_templates.join("holon-default");
        fs::create_dir_all(&shadowed_default).unwrap();
        fs::write(
            shadowed_default.join(TEMPLATE_AGENTS_FILENAME),
            "# Custom default\n",
        )
        .unwrap();

        let local = agent_home
            .path()
            .join("agent_templates")
            .join("local-agent");
        fs::create_dir_all(&local).unwrap();
        fs::write(
            local.join(TEMPLATE_AGENTS_FILENAME),
            "# Local agent\n\nLocal template\n",
        )
        .unwrap();

        let catalog = discover_agent_templates_catalog(Some(user_home.path()), agent_home.path());

        assert!(catalog
            .iter()
            .any(|entry| entry.catalog_id == "user_global:holon-default"));
        assert!(!catalog
            .iter()
            .any(|entry| entry.catalog_id == "builtin:holon-default"));
        assert!(catalog
            .iter()
            .any(|entry| entry.catalog_id == "builtin:holon-developer"));
        assert!(!catalog
            .iter()
            .any(|entry| entry.catalog_id == "user_global:holon-developer"));

        let worker_entry = catalog
            .iter()
            .find(|entry| entry.catalog_id == "user_global:worker")
            .unwrap();
        assert_eq!(worker_entry.template, "worker");
        assert_eq!(worker_entry.source, AgentTemplateSourceKind::UserGlobal);
        assert_eq!(worker_entry.path.as_deref(), Some(worker.as_path()));
        assert_eq!(worker_entry.description, "Does worker things");
        assert_eq!(
            worker_entry.included_skills,
            vec![source_skill.display().to_string()]
        );

        let local_entry = catalog
            .iter()
            .find(|entry| entry.catalog_id == "agent_home:local-agent")
            .unwrap();
        assert_eq!(local_entry.template, local.display().to_string());
        assert_eq!(local_entry.path.as_deref(), Some(local.as_path()));
    }

    #[test]
    fn discover_agent_templates_catalog_prefers_agent_home_over_other_sources() {
        let user_home = tempdir().unwrap();
        let agent_home = tempdir().unwrap();
        let user_templates = templates_root_for_home(user_home.path());
        seed_builtin_templates_for_home(user_home.path()).unwrap();

        let user_worker = user_templates.join("worker");
        fs::create_dir_all(&user_worker).unwrap();
        fs::write(
            user_worker.join(TEMPLATE_AGENTS_FILENAME),
            "# User worker\n\nUser-global worker\n",
        )
        .unwrap();

        let agent_templates = agent_home.path().join("agent_templates");
        let agent_worker = agent_templates.join("worker");
        fs::create_dir_all(&agent_worker).unwrap();
        fs::write(
            agent_worker.join(TEMPLATE_AGENTS_FILENAME),
            "# Agent worker\n\nAgent-home worker\n",
        )
        .unwrap();
        let agent_default = agent_templates.join("holon-default");
        fs::create_dir_all(&agent_default).unwrap();
        fs::write(
            agent_default.join(TEMPLATE_AGENTS_FILENAME),
            "# Agent default\n\nAgent-home default\n",
        )
        .unwrap();

        let catalog = discover_agent_templates_catalog(Some(user_home.path()), agent_home.path());

        assert!(catalog
            .iter()
            .any(|entry| entry.catalog_id == "agent_home:worker"));
        assert!(!catalog
            .iter()
            .any(|entry| entry.catalog_id == "user_global:worker"));
        assert!(catalog
            .iter()
            .any(|entry| entry.catalog_id == "agent_home:holon-default"));
        assert!(!catalog
            .iter()
            .any(|entry| entry.catalog_id == "builtin:holon-default"));
        assert!(!catalog
            .iter()
            .any(|entry| entry.catalog_id == "user_global:holon-default"));
    }

    #[tokio::test]
    async fn prefixed_template_selector_resolves_requested_source_when_shadowed() {
        let user_home = tempdir().unwrap();
        let agent_home = tempdir().unwrap();
        let user_templates = templates_root_for_home(user_home.path());
        seed_builtin_templates_for_home(user_home.path()).unwrap();

        let agent_default = agent_home
            .path()
            .join("agent_templates")
            .join("holon-default");
        fs::create_dir_all(&agent_default).unwrap();
        fs::write(
            agent_default.join(TEMPLATE_AGENTS_FILENAME),
            "# Agent default\n\nAgent-home default\n",
        )
        .unwrap();

        let user_worker = user_templates.join("worker");
        fs::create_dir_all(&user_worker).unwrap();
        fs::write(
            user_worker.join(TEMPLATE_AGENTS_FILENAME),
            "# User worker\n\nUser-global worker\n",
        )
        .unwrap();

        let agent_worker = agent_home.path().join("agent_templates").join("worker");
        fs::create_dir_all(&agent_worker).unwrap();
        fs::write(
            agent_worker.join(TEMPLATE_AGENTS_FILENAME),
            "# Agent worker\n\nAgent-home worker\n",
        )
        .unwrap();

        let builtin =
            resolve_template("builtin:holon-default", user_home.path(), agent_home.path())
                .await
                .unwrap();
        assert!(builtin.agents_md.contains("## Role"));

        let user = resolve_template("user_global:worker", user_home.path(), agent_home.path())
            .await
            .unwrap();
        assert_eq!(user.agents_md, "# User worker\n\nUser-global worker\n");

        let agent = resolve_template("agent_home:worker", user_home.path(), agent_home.path())
            .await
            .unwrap();
        assert_eq!(agent.agents_md, "# Agent worker\n\nAgent-home worker\n");
    }

    #[test]
    fn template_description_skips_multiline_html_comments() {
        assert_eq!(
            template_description("<!--\nmetadata\n-->\n# Heading\n\nVisible description\n"),
            "Visible description"
        );
        assert_eq!(template_description("<!-- hidden --> Visible\n"), "Visible");
    }

    #[test]
    fn builtin_template_is_managed_accounts_for_template_manifest() {
        let home = tempdir().unwrap();
        let template_dir = home.path().join("template");
        fs::create_dir_all(&template_dir).unwrap();
        fs::write(template_dir.join(TEMPLATE_AGENTS_FILENAME), "agents").unwrap();
        fs::write(
            template_dir.join(TEMPLATE_MANIFEST_FILENAME),
            "schema = \"holon.agent_template.v1\"\nid = \"test\"\nname = \"Test\"\nsummary = \"test\"\n",
        )
        .unwrap();

        let builtin = BuiltinTemplate {
            template_id: "holon-test",
            version: 1,
            agents_md: "agents",
            template_toml: "schema = \"holon.agent_template.v1\"\nid = \"test\"\nname = \"Test\"\nsummary = \"test\"\n",
            skill_names: &[],
        };
        let state = BuiltinTemplateState {
            template_id: builtin.template_id.to_string(),
            version: builtin.version,
            content_hash: builtin_template_content_hash(&builtin),
        };

        assert!(builtin_template_is_managed(&template_dir, &state).unwrap());
    }

    #[tokio::test]
    async fn github_solve_template_materializes_builtin_skills() {
        let _lock = crate::test_env::lock_env();
        let home = tempdir().unwrap();
        let _guard = EnvGuard::set("HOME", home.path().display().to_string());
        seed_builtin_templates().unwrap();

        let agent_home = home.path().join("agent");
        initialize_agent_home_from_template(&agent_home, "holon-github-solve")
            .await
            .unwrap();

        let agents_md = fs::read_to_string(agent_home.join("AGENTS.md")).unwrap();
        assert!(agents_md.contains("Holon GitHub Solve Agent"));
        assert!(agent_home
            .join("skills/github-issue-solve/SKILL.md")
            .is_file());
        assert!(agent_home
            .join("skills/github-issue-solve/references/issue-solve-workflow.md")
            .is_file());
        assert!(agent_home.join("skills/github-pr-fix/SKILL.md").is_file());
        assert!(agent_home
            .join("skills/github-pr-fix/references/diagnostics.md")
            .is_file());
        assert!(agent_home
            .join("skills/github-pr-fix/references/pr-fix-workflow.md")
            .is_file());
        assert!(agent_home.join("skills/github-review/SKILL.md").is_file());
        assert!(agent_home.join("skills/ghx/SKILL.md").is_file());
    }

    #[tokio::test]
    async fn initialize_agent_home_from_template_id_materializes_agents_md_and_local_skill() {
        let _lock = crate::test_env::lock_env();
        let home = tempdir().unwrap();
        let _guard = EnvGuard::set("HOME", home.path().display().to_string());
        let templates = templates_root().unwrap();
        fs::create_dir_all(&templates).unwrap();

        let source_skill = home.path().join("source-skill");
        fs::create_dir_all(&source_skill).unwrap();
        fs::write(
            source_skill.join("SKILL.md"),
            "---\nname: source\n---\nbody",
        )
        .unwrap();

        let template_dir = templates.join("worker");
        fs::create_dir_all(&template_dir).unwrap();
        fs::write(template_dir.join("AGENTS.md"), "worker instructions").unwrap();
        fs::write(
            template_dir.join(TEMPLATE_SKILLS_FILENAME),
            format!(
                "[[skills]]\nkind = \"local\"\npath = \"{}\"\n",
                source_skill.display()
            ),
        )
        .unwrap();

        let agent_home = home.path().join("agent");
        let provenance = initialize_agent_home_from_template(&agent_home, "worker")
            .await
            .unwrap();

        assert!(matches!(
            provenance.source,
            TemplateProvenanceSource::TemplateId { .. }
        ));
        let agents_md = fs::read_to_string(agent_home.join("AGENTS.md")).unwrap();
        assert!(agents_md.contains("worker instructions"));
        assert!(agents_md.contains("## Holon Agent Home"));
        assert!(agents_md.contains("memory/self.md"));
        assert!(agent_home.join("skills/source-skill/SKILL.md").exists());
        assert!(agent_memory_self_path(&agent_home).is_file());
        assert!(agent_memory_operator_path(&agent_home).is_file());
        assert!(agent_home.join("notes").is_dir());
        assert!(agent_home.join("work").is_dir());
        assert!(agent_home.join(".holon/state").is_dir());
        assert!(agent_home.join(".holon/ledger").is_dir());
        assert!(agent_home.join(".holon/indexes").is_dir());
        assert!(agent_home.join(".holon/cache").is_dir());
        assert!(template_provenance_path(&agent_home).is_file());
    }

    #[tokio::test]
    async fn catalog_id_initializes_from_agent_home_catalog() {
        let home = tempdir().unwrap();
        let catalog_agent_home = tempdir().unwrap();
        let template_dir = catalog_agent_home
            .path()
            .join("agent_templates")
            .join("worker");
        fs::create_dir_all(&template_dir).unwrap();
        fs::write(
            template_dir.join("AGENTS.md"),
            "catalog worker instructions",
        )
        .unwrap();

        let agent_home = home.path().join("agent-catalog-id");
        let provenance = initialize_agent_home_from_template_with_catalog(
            &agent_home,
            home.path(),
            catalog_agent_home.path(),
            "agent_home:worker",
        )
        .await
        .unwrap();

        assert!(fs::read_to_string(agent_home.join("AGENTS.md"))
            .unwrap()
            .contains("catalog worker instructions"));
        assert!(matches!(
            provenance.source,
            TemplateProvenanceSource::TemplateId {
                ref template_id,
                ref path
            } if template_id == "worker" && path == &template_dir
        ));

        let bare_agent_home = home.path().join("agent-bare-id");
        initialize_agent_home_from_template_with_catalog(
            &bare_agent_home,
            home.path(),
            catalog_agent_home.path(),
            "worker",
        )
        .await
        .unwrap();

        assert!(fs::read_to_string(bare_agent_home.join("AGENTS.md"))
            .unwrap()
            .contains("catalog worker instructions"));
    }

    #[tokio::test]
    async fn template_catalog_selector_error_lists_known_ids() {
        let home = tempdir().unwrap();
        let catalog_agent_home = tempdir().unwrap();
        let template_dir = catalog_agent_home
            .path()
            .join("agent_templates")
            .join("worker");
        fs::create_dir_all(&template_dir).unwrap();
        fs::write(
            template_dir.join("AGENTS.md"),
            "catalog worker instructions",
        )
        .unwrap();

        let agent_home = home.path().join("agent");
        let err = initialize_agent_home_from_template_with_catalog(
            &agent_home,
            home.path(),
            catalog_agent_home.path(),
            "missing",
        )
        .await
        .unwrap_err();
        let message = err.to_string();

        assert!(message.contains("unknown template selector: missing"));
        assert!(message.contains("known template ids/catalog ids"));
        assert!(message.contains("agent_home:worker"));
        assert!(message.contains("worker"));
        assert!(message.contains(&template_dir.display().to_string()));
        assert!(message.contains("agent_home"));
    }

    #[tokio::test]
    async fn template_without_directory_guidance_still_gets_required_agent_home_guidance() {
        let home = tempdir().unwrap();
        let template_dir = home.path().join("template");
        fs::create_dir_all(&template_dir).unwrap();
        fs::write(template_dir.join("AGENTS.md"), "role only").unwrap();

        let agent_home = home.path().join("agent");
        initialize_agent_home_from_template_with_home(
            &agent_home,
            home.path(),
            template_dir.to_str().unwrap(),
        )
        .await
        .unwrap();

        let agents_md = fs::read_to_string(agent_home.join("AGENTS.md")).unwrap();
        assert!(agents_md.starts_with("role only"));
        assert!(agents_md.contains("## Holon Agent Home"));
        assert!(agents_md.contains("`.holon/` is runtime-owned"));
        assert!(agents_md.contains("active project workspace"));
    }

    #[tokio::test]
    async fn initialize_agent_home_from_absolute_template_path_works() {
        let home = tempdir().unwrap();
        let template_dir = home.path().join("template");
        fs::create_dir_all(&template_dir).unwrap();
        fs::write(template_dir.join("AGENTS.md"), "absolute template").unwrap();

        let agent_home = home.path().join("agent");
        let provenance =
            initialize_agent_home_from_template(&agent_home, template_dir.to_str().unwrap())
                .await
                .unwrap();

        assert!(matches!(
            provenance.source,
            TemplateProvenanceSource::LocalPath { .. }
        ));
        let agents_md = fs::read_to_string(agent_home.join("AGENTS.md")).unwrap();
        assert!(agents_md.contains("absolute template"));
        assert!(agents_md.contains("## Holon Agent Home"));
    }

    #[tokio::test]
    async fn ensure_agent_home_agents_md_from_template_fills_missing_agents_md() {
        let _lock = crate::test_env::lock_env();
        let home = tempdir().unwrap();
        let _guard = EnvGuard::set("HOME", home.path().display().to_string());
        seed_builtin_templates().unwrap();

        let agent_home = home.path().join("agent");
        fs::create_dir_all(&agent_home).unwrap();
        fs::create_dir_all(agent_home.join(".holon/state")).unwrap();
        fs::write(agent_home.join(".holon/state/agent.json"), "{}").unwrap();

        let record =
            ensure_agent_home_agents_md_from_template(&agent_home, DEFAULT_AGENT_TEMPLATE_ID)
                .await
                .unwrap()
                .expect("missing AGENTS.md should be materialized");
        assert!(matches!(
            record.source,
            TemplateProvenanceSource::TemplateId { .. }
        ));
        assert!(fs::read_to_string(agent_home.join("AGENTS.md"))
            .unwrap()
            .contains("Holon Default Agent"));
        assert!(fs::read_to_string(agent_home.join("AGENTS.md"))
            .unwrap()
            .contains("## Holon Agent Home"));
    }

    #[tokio::test]
    async fn ensure_agent_home_agents_md_rolls_back_on_skill_failure() {
        let _lock = crate::test_env::lock_env();
        let home = tempdir().unwrap();
        let _guard = EnvGuard::set("HOME", home.path().display().to_string());
        let templates = templates_root().unwrap();
        fs::create_dir_all(&templates).unwrap();

        let valid_skill = home.path().join("valid-skill");
        fs::create_dir_all(&valid_skill).unwrap();
        fs::write(valid_skill.join("SKILL.md"), "# Valid Skill\n").unwrap();

        let template_dir = templates.join("broken");
        fs::create_dir_all(&template_dir).unwrap();
        fs::write(template_dir.join("AGENTS.md"), "broken").unwrap();
        fs::write(
            template_dir.join(TEMPLATE_SKILLS_FILENAME),
            format!(
                "[[skills]]\nkind = \"local\"\npath = \"{}\"\n[[skills]]\nkind = \"local\"\npath = \"relative/path\"\n",
                valid_skill.display()
            ),
        )
        .unwrap();

        let agent_home = home.path().join("agent");
        fs::create_dir_all(&agent_home).unwrap();
        fs::create_dir_all(agent_home.join(".holon/state")).unwrap();
        fs::write(agent_home.join(".holon/state/agent.json"), "{}").unwrap();

        let err = ensure_agent_home_agents_md_from_template(&agent_home, "broken")
            .await
            .unwrap_err();

        assert!(err.to_string().contains("absolute"));
        assert!(!agent_home.join("AGENTS.md").exists());
        assert!(!template_provenance_path(&agent_home).exists());
        assert!(!agent_home.join("skills/valid-skill").exists());
        assert!(agent_home.join("skills").is_dir());
    }

    #[tokio::test]
    async fn initialize_agent_home_fails_closed_on_invalid_skill_ref() {
        let _lock = crate::test_env::lock_env();
        let home = tempdir().unwrap();
        let _guard = EnvGuard::set("HOME", home.path().display().to_string());
        let templates = templates_root().unwrap();
        fs::create_dir_all(&templates).unwrap();
        let template_dir = templates.join("broken");
        fs::create_dir_all(&template_dir).unwrap();
        fs::write(template_dir.join("AGENTS.md"), "broken").unwrap();
        fs::write(
            template_dir.join(TEMPLATE_SKILLS_FILENAME),
            r#"[[skills]]
kind = "local"
path = "relative/path"
"#,
        )
        .unwrap();

        let agent_home = home.path().join("agent");
        let err = initialize_agent_home_from_template(&agent_home, "broken")
            .await
            .unwrap_err();

        assert!(err.to_string().contains("absolute"));
        assert!(!agent_home.exists());
    }

    #[tokio::test]
    async fn initialize_agent_home_restores_preexisting_empty_agent_home_on_failure() {
        let _lock = crate::test_env::lock_env();
        let home = tempdir().unwrap();
        let _guard = EnvGuard::set("HOME", home.path().display().to_string());
        let templates = templates_root().unwrap();
        fs::create_dir_all(&templates).unwrap();
        let template_dir = templates.join("broken");
        fs::create_dir_all(&template_dir).unwrap();
        fs::write(template_dir.join("AGENTS.md"), "broken").unwrap();
        fs::write(
            template_dir.join(TEMPLATE_SKILLS_FILENAME),
            r#"[[skills]]
kind = "local"
path = "relative/path"
"#,
        )
        .unwrap();

        let agent_home = home.path().join("agent");
        fs::create_dir_all(&agent_home).unwrap();

        let err = initialize_agent_home_from_template(&agent_home, "broken")
            .await
            .unwrap_err();

        assert!(err.to_string().contains("absolute"));
        assert!(agent_home.exists());
        assert!(fs::read_dir(&agent_home).unwrap().next().is_none());
    }

    #[test]
    fn parse_skill_refs_accepts_github_skill_refs() {
        let home = tempdir().unwrap();
        let manifest_path = home.path().join(TEMPLATE_SKILLS_FILENAME);
        fs::write(
            &manifest_path,
            r#"[[skills]]
kind = "github"
package = "owner/repo@skill"

[[skills]]
kind = "github"
package = "@scope/package"
"#,
        )
        .unwrap();

        let refs = parse_skill_refs(manifest_path).unwrap();
        assert_eq!(refs.len(), 2);
        assert!(
            matches!(&refs[0], TemplateSkillRef::Github { package } if package == "owner/repo@skill")
        );
        assert!(
            matches!(&refs[1], TemplateSkillRef::Github { package } if package == "@scope/package")
        );
    }

    #[test]
    fn template_github_skill_install_kind_splits_skill_selector() {
        let kind = template_github_skill_install_kind("owner/repo@skill").unwrap();
        assert!(matches!(
            kind,
            SkillInstallKind::Remote {
                package,
                skill: Some(skill),
                mode: SkillInstallMode::Linked,
            } if package == "owner/repo" && skill == "skill"
        ));

        let scoped = template_github_skill_install_kind("@scope/package").unwrap();
        assert!(matches!(
            scoped,
            SkillInstallKind::Remote {
                package,
                skill: None,
                mode: SkillInstallMode::Linked,
            } if package == "@scope/package"
        ));
    }

    #[test]
    fn parse_skill_refs_rejects_invalid_github_skill_refs() {
        let home = tempdir().unwrap();
        let manifest_path = home.path().join(TEMPLATE_SKILLS_FILENAME);
        fs::write(
            &manifest_path,
            r#"[[skills]]
kind = "github"
package = "owner/repo@../bad"
"#,
        )
        .unwrap();
        let err = parse_skill_refs(manifest_path).unwrap_err();
        assert!(err.to_string().contains("plain skill directory name"));
    }

    #[test]
    fn user_home_dir_falls_back_to_userprofile() {
        let _lock = crate::test_env::lock_env();
        let profile = tempdir().unwrap();
        let _home = EnvGuard::set("HOME", String::new());
        let _userprofile = EnvGuard::set("USERPROFILE", profile.path().display().to_string());

        assert_eq!(user_home_dir().unwrap(), profile.path());
    }

    #[tokio::test]
    async fn initialize_agent_home_from_github_url_works() {
        let _lock = crate::test_env::lock_env();
        let home = tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let seen_paths = Arc::new(Mutex::new(Vec::new()));
        let seen_paths_clone = seen_paths.clone();

        thread::spawn(move || {
            for stream in listener.incoming().take(3) {
                let mut stream = stream.unwrap();
                let mut buffer = [0_u8; 2048];
                let count = stream.read(&mut buffer).unwrap();
                let request = String::from_utf8_lossy(&buffer[..count]);
                let request_line = request.lines().next().unwrap().to_string();
                let path = request_line.split_whitespace().nth(1).unwrap().to_string();
                seen_paths_clone.lock().unwrap().push(path.clone());

                let body =
                    if path == "/repos/owner/repo/contents/templates/reviewer/AGENTS.md?ref=main" {
                        serde_json::json!({
                            "type": "file",
                            "encoding": "base64",
                            "content": BASE64_STANDARD.encode("reviewer rules")
                        })
                        .to_string()
                    } else {
                        "{\"message\":\"not found\"}".to_string()
                    };
                let status = if body.contains("not found") {
                    "404 Not Found"
                } else {
                    "200 OK"
                };
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                )
                .unwrap();
            }
        });

        let _api_guard =
            EnvGuard::set("HOLON_TEMPLATE_GITHUB_API_BASE", format!("http://{}", addr));
        let agent_home = home.path().join("agent");
        let url = "https://github.com/owner/repo/tree/main/templates/reviewer";

        let provenance = initialize_agent_home_from_template(&agent_home, url)
            .await
            .unwrap();

        assert!(matches!(
            provenance.source,
            TemplateProvenanceSource::GitHubUrl { .. }
        ));
        let agents_md = fs::read_to_string(agent_home.join("AGENTS.md")).unwrap();
        assert!(agents_md.contains("reviewer rules"));
        assert!(agents_md.contains("## Holon Agent Home"));
        assert_eq!(
            seen_paths.lock().unwrap().as_slice(),
            &[
                "/repos/owner/repo/contents/templates/reviewer/AGENTS.md?ref=main",
                "/repos/owner/repo/contents/templates/reviewer/skills.toml?ref=main",
                "/repos/owner/repo/contents/templates/reviewer/template.toml?ref=main",
            ]
        );
    }

    #[test]
    fn agent_template_source_kind_remote_label_roundtrip() {
        assert_eq!(AgentTemplateSourceKind::Remote.label(), "remote");
        assert_eq!(
            AgentTemplateSourceKind::from_label("remote"),
            Some(AgentTemplateSourceKind::Remote)
        );
    }

    #[test]
    fn catalog_entry_has_name_and_schema_from_template_toml() {
        let entry = builtin_template_catalog_entry(
            BUILTIN_TEMPLATES
                .iter()
                .find(|t| t.template_id == "holon-default")
                .unwrap(),
        );
        assert_eq!(entry.name, "Holon Default Agent");
        assert_eq!(
            entry.schema_version.as_deref(),
            Some("holon.agent_template.v1")
        );
        assert!(!entry.description.is_empty());
    }

    #[test]
    fn local_catalog_entry_has_name_and_schema_from_template_toml() {
        let home = tempdir().unwrap();
        let templates = templates_root_for_home(home.path());
        let worker = templates.join("worker");
        fs::create_dir_all(&worker).unwrap();
        fs::write(
            worker.join(TEMPLATE_AGENTS_FILENAME),
            "# Worker\n\nDoes worker things\n",
        )
        .unwrap();
        fs::write(
            worker.join(TEMPLATE_MANIFEST_FILENAME),
            "schema = \"holon.agent_template.v1\"\nid = \"worker\"\nname = \"Worker Agent\"\nsummary = \"A worker agent\"\n",
        )
        .unwrap();

        let entries =
            discover_local_templates(&templates, AgentTemplateSourceKind::UserGlobal, false);
        let entry = entries.iter().find(|e| e.template_id == "worker").unwrap();
        assert_eq!(entry.name, "Worker Agent");
        assert_eq!(
            entry.schema_version.as_deref(),
            Some("holon.agent_template.v1")
        );
        assert_eq!(entry.description, "A worker agent");
    }

    #[test]
    fn resolve_agent_template_detail_builtin() {
        let catalog = discover_agent_templates_catalog(None, std::path::Path::new("/nonexistent"));
        let entry = catalog
            .iter()
            .find(|e| e.catalog_id == "builtin:holon-default")
            .unwrap();
        let detail = resolve_agent_template_detail(entry).unwrap();
        assert_eq!(detail.template_id, "holon-default");
        assert_eq!(detail.source, AgentTemplateSourceKind::Builtin);
        assert!(!detail.agents_md.is_empty());
        assert!(detail.skills.is_empty());
        assert_eq!(detail.name, "Holon Default Agent");
        assert_eq!(
            detail.schema_version.as_deref(),
            Some("holon.agent_template.v1")
        );
    }

    #[test]
    fn resolve_agent_template_detail_local_with_skills() {
        let home = tempdir().unwrap();
        let templates = templates_root_for_home(home.path());
        let worker = templates.join("worker");
        fs::create_dir_all(&worker).unwrap();
        fs::write(
            worker.join(TEMPLATE_AGENTS_FILENAME),
            "# Worker\n\nDoes worker things\n",
        )
        .unwrap();
        let skill_dir = home.path().join("my-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), "# My Skill\n").unwrap();
        fs::write(
            worker.join(TEMPLATE_SKILLS_FILENAME),
            format!(
                "[[skills]]\nkind = \"local\"\npath = \"{}\"\n",
                skill_dir.display()
            ),
        )
        .unwrap();

        let entries =
            discover_local_templates(&templates, AgentTemplateSourceKind::UserGlobal, false);
        let entry = entries.iter().find(|e| e.template_id == "worker").unwrap();
        let detail = resolve_agent_template_detail(entry).unwrap();
        assert!(!detail.agents_md.is_empty());
        assert_eq!(detail.skills.len(), 1);
        assert_eq!(detail.skills[0].kind, "local");
        assert_eq!(detail.skills[0].reference, skill_dir.display().to_string());
    }

    // ---- #1984: GitHub repo discovery tests ----

    #[test]
    fn parse_github_repo_url_basic() {
        let parsed = parse_github_repo_url("https://github.com/owner/repo").unwrap();
        assert_eq!(parsed.owner, "owner");
        assert_eq!(parsed.repo, "repo");
        assert_eq!(parsed.git_ref, None);
    }

    #[test]
    fn parse_github_repo_url_with_ref() {
        let parsed = parse_github_repo_url("https://github.com/owner/repo/tree/dev").unwrap();
        assert_eq!(parsed.owner, "owner");
        assert_eq!(parsed.repo, "repo");
        assert_eq!(parsed.git_ref.as_deref(), Some("dev"));
    }

    #[test]
    fn parse_github_repo_url_rejects_template_tree_url() {
        let err =
            parse_github_repo_url("https://github.com/owner/repo/tree/main/templates/reviewer")
                .unwrap_err();
        assert!(err.to_string().contains("must be"));
    }

    #[test]
    fn parse_github_repo_url_rejects_non_github() {
        assert!(parse_github_repo_url("https://gitlab.com/owner/repo").is_err());
        assert!(parse_github_repo_url("https://example.com/owner/repo").is_err());
    }

    #[test]
    fn parse_github_repo_url_rejects_query_and_fragment() {
        assert!(parse_github_repo_url("https://github.com/owner/repo?foo=bar").is_err());
        assert!(parse_github_repo_url("https://github.com/owner/repo#section").is_err());
    }

    #[test]
    fn holon_index_manifest_parses_collections() {
        let toml_str = r#"schema = "holon.repository.v1"
[collections]
skills = "my-skills"
agent_templates = "my-templates"
"#;
        let index: HolonIndexManifest = toml::from_str(toml_str).unwrap();
        assert_eq!(
            index.collections.agent_templates.as_deref(),
            Some("my-templates")
        );
    }

    #[test]
    fn holon_index_manifest_missing_collections_defaults() {
        let toml_str = r#"schema = "holon.repository.v1""#;
        let index: HolonIndexManifest = toml::from_str(toml_str).unwrap();
        assert!(index.collections.agent_templates.is_none());
    }

    /// Build a mock GitHub API server that serves configured responses.
    struct MockGithubServer {
        addr: std::net::SocketAddr,
        _handle: thread::JoinHandle<()>,
    }

    impl MockGithubServer {
        /// Start a server that responds to paths matching the given closures.
        /// Each request consumes one entry; extra requests block.
        fn start(responses: Vec<(&'static str, u16, String)>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let handle = thread::spawn(move || {
                for stream in listener.incoming().take(responses.len()) {
                    let mut stream = stream.unwrap();
                    let mut buffer = [0_u8; 4096];
                    let _ = stream.read(&mut buffer);
                    let request = String::from_utf8_lossy(&buffer);
                    let request_line = request.lines().next().unwrap_or("");
                    let req_path = request_line.split_whitespace().nth(1).unwrap_or("");

                    // Strip query for matching.
                    let match_path = req_path.split('?').next().unwrap_or("");
                    let matched = responses.iter().find(|(p, _, _)| match_path == *p);
                    let (status_code, body) = match matched {
                        Some((_, code, body)) => (*code, body.clone()),
                        None => (404, "{\"message\":\"not found\"}".to_string()),
                    };
                    let status_text = if status_code == 200 {
                        "200 OK"
                    } else {
                        "404 Not Found"
                    };
                    let _ = write!(
                        stream,
                        "HTTP/1.1 {status_text}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                        body.len(),
                        body
                    );
                }
            });
            Self {
                addr,
                _handle: handle,
            }
        }
    }

    fn github_file_response(content: &str) -> String {
        serde_json::json!({
            "type": "file",
            "encoding": "base64",
            "content": BASE64_STANDARD.encode(content)
        })
        .to_string()
    }

    fn github_dir_response(entries: &[(&str, &str)]) -> String {
        let arr: Vec<_> = entries
            .iter()
            .map(|(name, kind)| serde_json::json!({ "name": name, "type": kind }))
            .collect();
        serde_json::Value::Array(arr).to_string()
    }

    #[tokio::test]
    async fn discover_github_repo_templates_with_index() {
        let _lock = crate::test_env::lock_env();
        let server = MockGithubServer::start(vec![
            // holon-index.toml at repo root
            (
                "/repos/owner/repo/contents/holon-index.toml",
                200,
                github_file_response(
                    r#"schema = "holon.repository.v1"
[collections]
agent_templates = "custom-templates"
"#,
                ),
            ),
            // Directory listing of custom-templates/
            (
                "/repos/owner/repo/contents/custom-templates",
                200,
                github_dir_response(&[
                    ("worker", "dir"),
                    ("reviewer", "dir"),
                    ("README.md", "file"),
                ]),
            ),
            // worker/template.toml
            (
                "/repos/owner/repo/contents/custom-templates/worker/template.toml",
                200,
                github_file_response(
                    r#"schema = "holon.agent_template.v1"
id = "worker"
name = "Worker"
summary = "Implementation agent"
"#,
                ),
            ),
            // reviewer/template.toml
            (
                "/repos/owner/repo/contents/custom-templates/reviewer/template.toml",
                200,
                github_file_response(
                    r#"schema = "holon.agent_template.v1"
id = "reviewer"
name = "Reviewer"
"#,
                ),
            ),
        ]);
        let _api_guard = EnvGuard::set(
            "HOLON_TEMPLATE_GITHUB_API_BASE",
            format!("http://{}", server.addr),
        );

        let entries = discover_github_repo_templates("owner", "repo", "main")
            .await
            .unwrap();
        assert_eq!(entries.len(), 2);
        let worker = entries.iter().find(|e| e.template_id == "worker").unwrap();
        assert_eq!(worker.name, "Worker");
        assert_eq!(worker.source, AgentTemplateSourceKind::Remote);
        assert_eq!(worker.catalog_id, "remote:owner/repo/worker");
        assert_eq!(worker.template, "remote:owner/repo#worker");
        let reviewer = entries
            .iter()
            .find(|e| e.template_id == "reviewer")
            .unwrap();
        assert_eq!(reviewer.name, "Reviewer");
    }

    #[tokio::test]
    async fn discover_github_repo_templates_fallback_default_dir() {
        let _lock = crate::test_env::lock_env();
        let server = MockGithubServer::start(vec![
            // No holon-index.toml → 404
            (
                "/repos/owner/repo/contents/holon-index.toml",
                404,
                "{\"message\":\"not found\"}".to_string(),
            ),
            // Default agent_templates/ directory listing
            (
                "/repos/owner/repo/contents/agent_templates",
                200,
                github_dir_response(&[("simple", "dir")]),
            ),
            // simple/template.toml
            (
                "/repos/owner/repo/contents/agent_templates/simple/template.toml",
                200,
                github_file_response(
                    r#"schema = "holon.agent_template.v1"
id = "simple"
name = "Simple"
"#,
                ),
            ),
        ]);
        let _api_guard = EnvGuard::set(
            "HOLON_TEMPLATE_GITHUB_API_BASE",
            format!("http://{}", server.addr),
        );

        let entries = discover_github_repo_templates("owner", "repo", "main")
            .await
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].template_id, "simple");
    }

    #[tokio::test]
    async fn discover_github_repo_templates_skips_invalid_template() {
        let _lock = crate::test_env::lock_env();
        let server = MockGithubServer::start(vec![
            // No holon-index.toml
            (
                "/repos/owner/repo/contents/holon-index.toml",
                404,
                "{\"message\":\"not found\"}".to_string(),
            ),
            // Directory with two templates
            (
                "/repos/owner/repo/contents/agent_templates",
                200,
                github_dir_response(&[("good", "dir"), ("bad", "dir")]),
            ),
            // good/template.toml - valid
            (
                "/repos/owner/repo/contents/agent_templates/good/template.toml",
                200,
                github_file_response(
                    r#"schema = "holon.agent_template.v1"
id = "good"
name = "Good"
"#,
                ),
            ),
            // bad/template.toml - missing (404)
            (
                "/repos/owner/repo/contents/agent_templates/bad/template.toml",
                404,
                "{\"message\":\"not found\"}".to_string(),
            ),
        ]);
        let _api_guard = EnvGuard::set(
            "HOLON_TEMPLATE_GITHUB_API_BASE",
            format!("http://{}", server.addr),
        );

        let entries = discover_github_repo_templates("owner", "repo", "main")
            .await
            .unwrap();
        // Only "good" should be discovered; "bad" is skipped silently.
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].template_id, "good");
    }

    #[tokio::test]
    async fn discover_github_repo_templates_empty_when_no_dir() {
        let _lock = crate::test_env::lock_env();
        let server = MockGithubServer::start(vec![
            // No holon-index.toml
            (
                "/repos/owner/repo/contents/holon-index.toml",
                404,
                "{\"message\":\"not found\"}".to_string(),
            ),
            // No agent_templates/ directory either
            (
                "/repos/owner/repo/contents/agent_templates",
                404,
                "{\"message\":\"not found\"}".to_string(),
            ),
        ]);
        let _api_guard = EnvGuard::set(
            "HOLON_TEMPLATE_GITHUB_API_BASE",
            format!("http://{}", server.addr),
        );

        let entries = discover_github_repo_templates("owner", "repo", "main")
            .await
            .unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn discover_remote_templates_stores_source_url() {
        let _lock = crate::test_env::lock_env();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let server = MockGithubServer::start(vec![
            (
                "/repos/owner/repo/contents/holon-index.toml",
                404,
                "{\"message\":\"not found\"}".to_string(),
            ),
            (
                "/repos/owner/repo/contents/agent_templates",
                200,
                github_dir_response(&[("worker", "dir")]),
            ),
            (
                "/repos/owner/repo/contents/agent_templates/worker/template.toml",
                200,
                github_file_response(
                    r#"schema = "holon.agent_template.v1"
id = "worker"
name = "Worker"
"#,
                ),
            ),
        ]);
        let _api_guard = EnvGuard::set(
            "HOLON_TEMPLATE_GITHUB_API_BASE",
            format!("http://{}", server.addr),
        );

        let entries = rt
            .block_on(discover_github_repo_templates("owner", "repo", "dev"))
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].source_url.as_deref(),
            Some("https://github.com/owner/repo/tree/dev/agent_templates/worker")
        );
    }

    #[tokio::test]
    async fn fetch_default_branch_resolves_repo_info() {
        let _lock = crate::test_env::lock_env();
        let server = MockGithubServer::start(vec![(
            "/repos/owner/repo",
            200,
            serde_json::json!({"default_branch": "develop"}).to_string(),
        )]);
        let _api_guard = EnvGuard::set(
            "HOLON_TEMPLATE_GITHUB_API_BASE",
            format!("http://{}", server.addr),
        );

        let branch = fetch_default_branch("owner", "repo").await.unwrap();
        assert_eq!(branch, "develop");
    }

    #[tokio::test]
    async fn discover_remote_templates_chains_url_to_discovery() {
        let _lock = crate::test_env::lock_env();
        let server = MockGithubServer::start(vec![
            // Default branch lookup
            (
                "/repos/owner/repo",
                200,
                serde_json::json!({"default_branch": "main"}).to_string(),
            ),
            // No holon-index.toml
            (
                "/repos/owner/repo/contents/holon-index.toml",
                404,
                "{\"message\":\"not found\"}".to_string(),
            ),
            // Directory listing
            (
                "/repos/owner/repo/contents/agent_templates",
                200,
                github_dir_response(&[("worker", "dir")]),
            ),
            // worker/template.toml
            (
                "/repos/owner/repo/contents/agent_templates/worker/template.toml",
                200,
                github_file_response(
                    r#"schema = "holon.agent_template.v1"
id = "worker"
name = "Worker"
"#,
                ),
            ),
        ]);
        let _api_guard = EnvGuard::set(
            "HOLON_TEMPLATE_GITHUB_API_BASE",
            format!("http://{}", server.addr),
        );

        let entries = discover_remote_templates("https://github.com/owner/repo")
            .await
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].template_id, "worker");
        assert_eq!(entries[0].source, AgentTemplateSourceKind::Remote);
        assert_eq!(
            entries[0].source_url.as_deref(),
            Some("https://github.com/owner/repo/tree/main/agent_templates/worker")
        );
    }

    #[tokio::test]
    async fn resolve_remote_agent_template_detail_fetches_template_tree() {
        let _lock = crate::test_env::lock_env();
        let server = MockGithubServer::start(vec![
            (
                "/repos/owner/repo/contents/agent_templates/worker/AGENTS.md",
                200,
                github_file_response("# Worker\n\nDoes worker things\n"),
            ),
            (
                "/repos/owner/repo/contents/agent_templates/worker/skills.toml",
                200,
                github_file_response(
                    "[[skills]]\nkind = \"github\"\npackage = \"owner/skills/ghx\"\n",
                ),
            ),
            (
                "/repos/owner/repo/contents/agent_templates/worker/template.toml",
                200,
                github_file_response(
                    r#"schema = "holon.agent_template.v1"
id = "worker"
name = "Worker"
"#,
                ),
            ),
        ]);
        let _api_guard = EnvGuard::set(
            "HOLON_TEMPLATE_GITHUB_API_BASE",
            format!("http://{}", server.addr),
        );
        let entry = AgentTemplateCatalogEntry {
            catalog_id: "remote:community:worker".into(),
            template: "remote:community:worker".into(),
            template_id: "worker".into(),
            source: AgentTemplateSourceKind::Remote,
            path: None,
            name: "Worker".into(),
            schema_version: None,
            description: "Does worker things".into(),
            included_skills: vec!["owner/skills/ghx".into()],
            source_id: Some("community".into()),
            resolved_ref: Some("main".into()),
            resolved_revision: None,
            source_url: Some(
                "https://github.com/owner/repo/tree/main/agent_templates/worker".into(),
            ),
        };

        let detail = resolve_remote_agent_template_detail(&entry).await.unwrap();
        assert_eq!(detail.catalog_id, "remote:community:worker");
        assert_eq!(detail.source, AgentTemplateSourceKind::Remote);
        assert_eq!(
            detail.schema_version.as_deref(),
            Some("holon.agent_template.v1")
        );
        assert!(detail.agents_md.contains("Does worker things"));
        assert_eq!(detail.skills.len(), 1);
        assert_eq!(detail.skills[0].kind, "github");
        assert_eq!(detail.skills[0].reference, "owner/skills/ghx");
    }
    #[tokio::test]
    async fn provenance_records_schema_version_from_template_toml() {
        let _lock = crate::test_env::lock_env();
        let home = tempdir().unwrap();
        let _guard = EnvGuard::set("HOME", home.path().display().to_string());
        let templates = templates_root().unwrap();
        let template_dir = templates.join("versioned");
        fs::create_dir_all(&template_dir).unwrap();
        fs::write(template_dir.join("AGENTS.md"), "versioned template").unwrap();
        fs::write(
            template_dir.join(TEMPLATE_MANIFEST_FILENAME),
            r#"schema = "holon.agent_template.v1"
id = "versioned"
name = "Versioned"
"#,
        )
        .unwrap();

        let agent_home = home.path().join("agent-versioned");
        let provenance = initialize_agent_home_from_template(&agent_home, "versioned")
            .await
            .unwrap();

        assert_eq!(
            provenance.schema_version.as_deref(),
            Some("holon.agent_template.v1")
        );
    }

    #[tokio::test]
    async fn provenance_schema_version_none_when_no_template_toml() {
        let _lock = crate::test_env::lock_env();
        let home = tempdir().unwrap();
        let _guard = EnvGuard::set("HOME", home.path().display().to_string());
        let templates = templates_root().unwrap();
        let template_dir = templates.join("bare");
        fs::create_dir_all(&template_dir).unwrap();
        fs::write(template_dir.join("AGENTS.md"), "bare template").unwrap();

        let agent_home = home.path().join("agent-bare");
        let provenance = initialize_agent_home_from_template(&agent_home, "bare")
            .await
            .unwrap();

        assert!(provenance.schema_version.is_none());
    }

    #[tokio::test]
    async fn provenance_builtin_template_has_schema_version() {
        let _lock = crate::test_env::lock_env();
        let home = tempdir().unwrap();
        let _guard = EnvGuard::set("HOME", home.path().display().to_string());
        seed_builtin_templates().unwrap();

        let agent_home = home.path().join("agent-builtin");
        let provenance = initialize_agent_home_from_template(&agent_home, "holon-default")
            .await
            .unwrap();

        assert_eq!(
            provenance.schema_version.as_deref(),
            Some("holon.agent_template.v1")
        );
    }

    #[tokio::test]
    async fn template_modification_after_init_does_not_change_agent() {
        let _lock = crate::test_env::lock_env();
        let home = tempdir().unwrap();
        let _guard = EnvGuard::set("HOME", home.path().display().to_string());
        let templates = templates_root().unwrap();
        let template_dir = templates.join("mutable");
        fs::create_dir_all(&template_dir).unwrap();
        fs::write(template_dir.join("AGENTS.md"), "original instructions").unwrap();

        let agent_home = home.path().join("agent-mutable");
        initialize_agent_home_from_template(&agent_home, "mutable")
            .await
            .unwrap();

        // Modify the template source after agent initialization.
        fs::write(
            template_dir.join("AGENTS.md"),
            "modified instructions that should not propagate",
        )
        .unwrap();

        let agents_md = fs::read_to_string(agent_home.join("AGENTS.md")).unwrap();
        assert!(agents_md.contains("original instructions"));
        assert!(!agents_md.contains("modified instructions"));
    }

    #[tokio::test]
    async fn re_applying_template_to_existing_agent_fails() {
        let _lock = crate::test_env::lock_env();
        let home = tempdir().unwrap();
        let _guard = EnvGuard::set("HOME", home.path().display().to_string());
        let templates = templates_root().unwrap();
        let template_dir = templates.join("reapply");
        fs::create_dir_all(&template_dir).unwrap();
        fs::write(template_dir.join("AGENTS.md"), "reapply template").unwrap();

        let agent_home = home.path().join("agent-reapply");
        initialize_agent_home_from_template(&agent_home, "reapply")
            .await
            .unwrap();

        // Second initialization on the now-populated agent home must fail.
        let err = initialize_agent_home_from_template(&agent_home, "reapply")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn parsed_github_template_url_parses_tree_url() {
        let url = ParsedGithubTemplateUrl::parse(
            "https://github.com/holon-run/templates/tree/main/my-template",
        )
        .unwrap();
        assert_eq!(url.owner, "holon-run");
        assert_eq!(url.repo, "templates");
        assert_eq!(url.all_after_tree, "main/my-template");
    }

    #[test]
    fn parsed_github_template_url_parses_nested_path() {
        let url = ParsedGithubTemplateUrl::parse(
            "https://github.com/owner/repo/tree/v1.0.0/nested/path/to/template",
        )
        .unwrap();
        assert_eq!(url.owner, "owner");
        assert_eq!(url.repo, "repo");
        assert_eq!(url.all_after_tree, "v1.0.0/nested/path/to/template");
    }

    #[test]
    fn parsed_github_template_url_parses_without_subpath() {
        let url =
            ParsedGithubTemplateUrl::parse("https://github.com/owner/repo/tree/main").unwrap();
        assert_eq!(url.owner, "owner");
        assert_eq!(url.repo, "repo");
        assert_eq!(url.all_after_tree, "main");
    }

    #[test]
    fn parsed_github_template_url_rejects_non_github_url() {
        assert!(ParsedGithubTemplateUrl::parse("https://gitlab.com/owner/repo").is_err());
        assert!(ParsedGithubTemplateUrl::parse("not-a-url").is_err());
    }

    #[test]
    fn parsed_github_template_url_rejects_missing_tree_segment() {
        assert!(ParsedGithubTemplateUrl::parse("https://github.com/owner/repo").is_err());
    }

    #[test]
    fn remove_user_template_removes_existing_and_reports_missing() {
        let tmp = tempdir().unwrap();
        let templates_root = templates_root_for_home(tmp.path());
        let template_dir = templates_root.join("my-test-template");
        fs::create_dir_all(&template_dir).unwrap();
        fs::write(template_dir.join("AGENTS.md"), "# test").unwrap();

        remove_user_template(tmp.path(), "my-test-template").unwrap();
        let err = remove_user_template(tmp.path(), "my-test-template").unwrap_err();
        assert!(!template_dir.exists());

        assert!(err.to_string().contains("not found"));
    }

    #[cfg(unix)]
    #[test]
    fn copy_template_dir_rejects_symlinks() {
        use std::os::unix::fs::symlink;
        let tmp = tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("AGENTS.md"), "# test").unwrap();
        // Create a symlink pointing outside the template directory.
        symlink("/etc/hostname", src.join("evil")).unwrap();

        let err = copy_template_dir(&src, &dst).unwrap_err();
        assert!(
            err.to_string().contains("symlink"),
            "expected symlink rejection, got: {err}"
        );
    }

    #[test]
    fn validate_github_template_url_parses_and_reports() {
        let result =
            validate_github_template_url("https://github.com/owner/repo/tree/main/templates/dev")
                .unwrap();
        assert_eq!(result, "owner/repo/tree/main/templates/dev");

        assert!(validate_github_template_url("https://gitlab.com/owner/repo").is_err());
    }

    #[test]
    fn parsed_github_template_url_preserves_slash_in_ref() {
        let url = ParsedGithubTemplateUrl::parse(
            "https://github.com/holon-run/holon-test/tree/feat/seed-test-templates/agent_templates/test-developer",
        )
        .unwrap();
        assert_eq!(url.owner, "holon-run");
        assert_eq!(url.repo, "holon-test");
        // all_after_tree preserves the full ref+path for downstream resolution
        assert_eq!(
            url.all_after_tree,
            "feat/seed-test-templates/agent_templates/test-developer"
        );
    }

    #[test]
    fn validate_github_template_url_preserves_slash_in_ref() {
        let result = validate_github_template_url(
            "https://github.com/owner/repo/tree/feat/branch-name/templates/dev",
        )
        .unwrap();
        // Canonical form preserves the full ref+path since we can't resolve
        // the ref/path boundary without an API call.
        assert_eq!(result, "owner/repo/tree/feat/branch-name/templates/dev");
    }
}
