use std::{
    env, fs,
    path::{Path, PathBuf},
};

use crate::types::{
    AgentTemplateCatalogEntry, AgentTemplateDetail, AgentTemplateSkillDependency,
    AgentTemplateSourceKind, SkillInstallKind, SkillInstallMode,
};

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
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
#[serde(deny_unknown_fields)]
struct GitHubContentsFileResponse {
    #[serde(rename = "type")]
    kind: String,
    content: Option<String>,
    encoding: Option<String>,
}

#[derive(Debug, Clone)]
struct ResolvedTemplate {
    provenance: TemplateProvenanceSource,
    agents_md: String,
    skill_refs: Vec<TemplateSkillRef>,
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
        };
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
        };
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
        });
    }
    entries
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
/// Reads AGENTS.md content and skill dependencies for the template,
/// suitable for GUI or daemon API detail responses.
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
            let agents_md = fs::read_to_string(path.join(TEMPLATE_AGENTS_FILENAME)).ok()?;
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
            Err(anyhow!("remote template resolution is not yet implemented"))
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
    })
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
            for stream in listener.incoming().take(2) {
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
}
