use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
};

use crate::PackageIo;
use holon_domain::{
    AgentTemplateCatalogEntry, AgentTemplateDetail, AgentTemplateSkillDependency,
    AgentTemplateSourceKind, SkillInstallKind, SkillInstallMode,
};
use holon_github::{
    archive::GitHubArchive,
    client::{ApiFailure, AsyncClient, BlockingClient, GitHubAuth},
};

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use reqwest::header::USER_AGENT;
use serde::{Deserialize, Serialize};

/// Runtime filesystem policy and built-in content for template operations.
#[derive(Clone, Copy)]
pub struct TemplateEnvironment {
    io: &'static dyn PackageIo,
    builtin_templates: &'static [BuiltinTemplate],
    builtin_skills: &'static [BuiltinSkill],
    archive_base: fn() -> String,
}

impl TemplateEnvironment {
    pub const fn new(
        io: &'static dyn PackageIo,
        builtin_templates: &'static [BuiltinTemplate],
        builtin_skills: &'static [BuiltinSkill],
        archive_base: fn() -> String,
    ) -> Self {
        Self {
            io,
            builtin_templates,
            builtin_skills,
            archive_base,
        }
    }
}

const TEMPLATE_AGENTS_FILENAME: &str = "AGENTS.md";
const TEMPLATE_SKILLS_FILENAME: &str = "skills.toml";
const TEMPLATE_MANIFEST_FILENAME: &str = "template.toml";
const TEMPLATE_PROVENANCE_FILENAME: &str = "template-provenance.json";
const TEMPLATE_STAGING_FILENAME: &str = "template-staging.json";
const MANAGED_TEMPLATE_STATE_FILENAME: &str = ".holon-template.json";
const TEMPLATE_REGISTRY_FILENAME: &str = ".registry.json";
pub const DEFAULT_AGENT_TEMPLATE_ID: &str = "holon-default";
pub const GITHUB_SOLVE_AGENT_TEMPLATE_ID: &str = "github-solver";
pub const OFFICIAL_AGENT_TEMPLATE_REMOTE_SOURCE_ID: &str = "official";
pub const OFFICIAL_AGENT_TEMPLATE_REMOTE_SOURCE_URL: &str = "https://github.com/holon-run/holon";
const GITHUB_TEMPLATE_API_BASE_ENV: &str = "HOLON_TEMPLATE_GITHUB_API_BASE";
const GITHUB_TEMPLATE_TOKEN_ENV_VARS: &[&str] = &["HOLON_GITHUB_TOKEN", "GITHUB_TOKEN", "GH_TOKEN"];
const GITHUB_TEMPLATE_USER_AGENT: &str = "holon-template-resolver";
const MEMORY_SELF_INITIAL: &str = "# Self Memory\n\n";
const MEMORY_OPERATOR_INITIAL: &str = r#"# Operator Memory

Use this file for stable operator preferences that should influence future turns, such as preferred reply language, communication style, naming conventions, tool defaults, and recurring collaboration expectations.

"#;

/// Minimal durable-role note used for blank (no-template) agent homes. It is
/// intentionally role-neutral so the agent can be shaped by its first
/// assignment rather than inheriting a preset identity.
const BLANK_AGENT_HOME_GUIDANCE: &str = "# Agent\n\nThis agent was created without a role template. Record durable, agent-specific responsibilities and coordination rules here as they are established.";

pub const REQUIRED_AGENT_HOME_GUIDANCE: &str = r#"## Holon Agent Home

- `agent_home` is this agent's default workspace. Use it for agent-local state, notes, memory, and non-project-local work.
- `AGENTS.md` is automatically loaded as concise agent guidance. Keep durable behavior here, not transient plans or copied project docs.
- `memory/self.md` and `memory/operator.md` are curated agent-scoped Markdown memory. They are searched or retrieved on demand and are not the same as always-loaded guidance.
- Use `memory/operator.md` for stable operator preferences such as preferred reply language, communication style, naming conventions, tool defaults, and recurring collaboration expectations.
- `notes/` is ordinary working notes.
- `work/` is for non-project-local work artifacts. Project-scoped files and memory belong in the active project workspace.
- `skills/` is for agent-local skills.
- `tmp/` is for short-lived working files. Prefer it over the system temp directory for intermediate artifacts that may be referenced from tool results or history (for example images inspected with ViewImage), because system temp files can be cleaned up independently and break those references. Do not keep anything there that must survive; contents may be cleaned up at any time.
- `.holon/` is runtime-owned state, ledger, index, and cache storage. Do not edit it as ordinary agent-authored files.
"#;

pub struct BuiltinTemplate {
    pub template_id: &'static str,
    pub agents_md: &'static str,
    pub template_toml: &'static str,
    pub skill_names: &'static [&'static str],
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ManagedTemplateState {
    template_id: String,
    content_hash: String,
    owner: ManagedTemplateOwner,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ManagedTemplateOwner {
    RemoteSource {
        source_id: String,
        url: String,
        resolved_ref: String,
        source_url: String,
        synced_at: DateTime<Utc>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct AgentTemplateRegistry {
    version: u32,
    #[serde(default)]
    sources: BTreeMap<String, AgentTemplateRegistrySource>,
    #[serde(default)]
    installed: BTreeMap<String, AgentTemplateRegistryInstalled>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct AgentTemplateRegistrySource {
    kind: String,
    url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    requested_ref: Option<String>,
    resolved_ref: String,
    catalog_path: String,
    catalog_hash: String,
    synced_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct AgentTemplateRegistryInstalled {
    template_id: String,
    source_id: String,
    source_url: String,
    source_resolved_ref: String,
    template_path: String,
    local_path: String,
    content_hash: String,
    synced_at: DateTime<Utc>,
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
    /// No role template was applied. The agent home was initialized with only
    /// the required agent-home guidance, which is the blank canvas used when a
    /// caller omits `template`.
    Blank,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TemplateProvenanceRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    pub source: TemplateProvenanceSource,
    pub applied_at: DateTime<Utc>,
    #[serde(default)]
    pub schema_version: Option<String>,
}

/// File-format skill reference as defined in `skills.toml`.
/// Template-authored manifests expose only `local` and `github` skill
/// references. The legacy `package` field is accepted for old manifests, but
/// new GitHub references should use structured `repo` + `path` fields. The
/// `uses` field is accepted as a GitHub Actions-style shorthand and is
/// normalized into the structured form.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum TemplateSkillFileRef {
    Builtin {
        name: String,
    },
    Local {
        path: PathBuf,
    },
    Github {
        #[serde(default)]
        repo: Option<String>,
        #[serde(default)]
        path: Option<String>,
        #[serde(default, rename = "ref")]
        git_ref: Option<String>,
        #[serde(default)]
        uses: Option<String>,
        #[serde(default)]
        package: Option<String>,
    },
}

/// Parsed `skills.toml` manifest.
#[derive(Debug, Deserialize)]
struct TemplateSkillsManifest {
    #[serde(default)]
    skills: Vec<TemplateSkillFileRef>,
}

/// Internal skill reference used throughout template resolution and materialization.
#[derive(Debug, Clone)]
enum TemplateSkillRef {
    Local { path: PathBuf },
    Github(TemplateGithubSkillRef),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TemplateGithubSkillRef {
    Structured {
        repo: String,
        path: String,
        git_ref: Option<String>,
    },
    LegacyPackage {
        package: String,
    },
}

impl TemplateGithubSkillRef {
    fn display_reference(&self) -> String {
        match self {
            TemplateGithubSkillRef::Structured {
                repo,
                path,
                git_ref,
            } => match git_ref {
                Some(git_ref) => format!("{repo}/{path}@{git_ref}"),
                None => format!("{repo}/{path}"),
            },
            TemplateGithubSkillRef::LegacyPackage { package } => package.clone(),
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
    #[serde(default)]
    execution: Option<Box<TemplateExecutionBinding>>,
}

/// Optional execution-driver declaration from a template manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct TemplateExecutionBinding {
    #[serde(default)]
    pub driver: Option<String>,
    #[serde(default)]
    pub harness_profile: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
}

/// Parsed `[compatibility]` section from `template.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
struct TemplateCompatibility {
    #[serde(default)]
    #[allow(dead_code)]
    holon: Option<String>,
}

pub struct BuiltinSkill {
    pub name: &'static str,
    pub skill_md: &'static str,
    pub files: &'static [BuiltinSkillFile],
}

pub struct BuiltinSkillFile {
    pub path: &'static str,
    pub content: &'static str,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTemplateRemoteSourceSyncStatus {
    NotSynced,
    Synced,
    PartiallyFailed,
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

#[derive(Debug, Clone)]
struct ResolvedTemplate {
    provenance: TemplateProvenanceSource,
    agents_md: String,
    template_toml: Option<String>,
    skill_refs: Vec<TemplateSkillRef>,
    schema_version: Option<String>,
}

pub fn template_provenance_path(agent_home: &Path) -> PathBuf {
    agent_home
        .join(".holon")
        .join("state")
        .join(TEMPLATE_PROVENANCE_FILENAME)
}

pub fn template_staging_path(agent_home: &Path) -> PathBuf {
    agent_home
        .join(".holon")
        .join("state")
        .join(TEMPLATE_STAGING_FILENAME)
}

/// Runtime-owned record of an in-flight template materialization. It lets a
/// later repair distinguish "the runtime wrote AGENTS.md but never finished"
/// from genuine user content, without ever overwriting the latter.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TemplateStagingRecord {
    #[serde(default)]
    selector: Option<String>,
    started_at: DateTime<Utc>,
    /// SHA-256 of the AGENTS.md bytes the runtime wrote, so user edits after
    /// an interrupted install are detected and preserved.
    agents_md_sha256: String,
    /// Skill destinations created so far, in creation order.
    created_paths: Vec<std::path::PathBuf>,
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn write_template_staging(agent_home: &Path, staging: &TemplateStagingRecord) -> Result<()> {
    let content = serde_json::to_vec_pretty(staging)?;
    write_file_atomically(&template_staging_path(agent_home), &content)
}

fn read_template_staging(agent_home: &Path) -> Result<Option<TemplateStagingRecord>> {
    let path = template_staging_path(agent_home);
    if !path.is_file() {
        return Ok(None);
    }
    let content =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let staging: TemplateStagingRecord = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(Some(staging))
}

fn remove_template_staging(agent_home: &Path) -> Result<()> {
    let path = template_staging_path(agent_home);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(anyhow::Error::new(error).context(format!("failed to remove {}", path.display())))
        }
    }
}

/// Clean up a runtime half-materialized template and report whether recovery
/// happened. Returns `Ok(false)` (touching nothing) when the staging record is
/// absent, selects a different template, or the current AGENTS.md no longer
/// matches the bytes the runtime wrote, which means the file may be user
/// content that must be preserved. An absent AGENTS.md is cleaned up: the run
/// was interrupted before its first artifact landed, so nothing is user
/// content yet.
pub fn recover_template_staging(
    agent_home: &Path,
    expected_selector: Option<&str>,
) -> Result<bool> {
    let Some(staging) = read_template_staging(agent_home)? else {
        return Ok(false);
    };
    // Legacy agents created before blank initialization existed staged the
    // implicit `holon-default` selector while `desired.template` stayed unset.
    // Treat that as a match so an upgrade can still recover an interrupted
    // materialization instead of failing repair forever.
    let selector_matches = staging.selector.as_deref() == expected_selector
        || (expected_selector.is_none()
            && staging.selector.as_deref() == Some(DEFAULT_AGENT_TEMPLATE_ID));
    if !selector_matches {
        return Ok(false);
    }
    let agents_md = agent_home.join(TEMPLATE_AGENTS_FILENAME);
    let agents_md_is_runtime_bytes = match fs::read(&agents_md) {
        Ok(bytes) => sha256_hex(&bytes) == staging.agents_md_sha256,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => {
            return Err(anyhow::Error::new(error)
                .context(format!("failed to read {}", agents_md.display())));
        }
    };
    if !agents_md_is_runtime_bytes {
        return Ok(false);
    }
    for path in staging.created_paths.iter().rev() {
        match remove_materialized_skill_destination(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(anyhow!(
                    "failed to remove staged template artifact {}: {error}",
                    path.display()
                ));
            }
        }
    }
    if agents_md.is_file() {
        fs::remove_file(&agents_md)
            .with_context(|| format!("failed to remove staged {}", agents_md.display()))?;
    }
    remove_template_staging(agent_home)?;
    tracing::info!(target: "holon::agent_template",
        selector = ?staging.selector,
        agent_home = %agent_home.display(),
        "template_staging_recovered: cleaned interrupted template materialization"
    );
    Ok(true)
}

/// Tracks the runtime-owned staging record across one materialization run.
struct TemplateStagingGuard {
    agent_home: std::path::PathBuf,
    record: TemplateStagingRecord,
}

impl TemplateStagingGuard {
    fn begin(agent_home: &Path, selector: Option<&str>, agents_md_content: &str) -> Result<Self> {
        let record = TemplateStagingRecord {
            selector: selector.map(str::to_string),
            started_at: Utc::now(),
            agents_md_sha256: sha256_hex(agents_md_content.as_bytes()),
            created_paths: Vec::new(),
        };
        write_template_staging(agent_home, &record)?;
        Ok(Self {
            agent_home: agent_home.to_path_buf(),
            record,
        })
    }

    fn push_created_path(&mut self, path: std::path::PathBuf) -> Result<()> {
        self.record.created_paths.push(path);
        write_template_staging(&self.agent_home, &self.record)
    }

    /// Remove the staging record after provenance landed (success path only).
    fn finish(self) -> Result<()> {
        remove_template_staging(&self.agent_home)
    }
}

pub fn agent_memory_self_path(agent_home: &Path) -> PathBuf {
    agent_home.join("memory").join("self.md")
}

pub fn agent_memory_operator_path(agent_home: &Path) -> PathBuf {
    agent_home.join("memory").join("operator.md")
}

fn builtin_template_state_path(template_dir: &Path) -> PathBuf {
    template_dir.join(MANAGED_TEMPLATE_STATE_FILENAME)
}

pub fn seed_builtin_templates() -> Result<()> {
    Ok(())
}

pub fn seed_builtin_templates_for_home(home_dir: &Path) -> Result<()> {
    let _ = home_dir;
    Ok(())
}

pub async fn initialize_agent_home_from_template(
    environment: TemplateEnvironment,
    agent_home: &Path,
    template: &str,
) -> Result<TemplateProvenanceRecord> {
    let home_dir = user_home_dir()?;
    initialize_agent_home_from_template_with_home(environment, agent_home, &home_dir, template)
        .await
}

pub async fn initialize_agent_home_from_template_with_catalog(
    environment: TemplateEnvironment,
    agent_home: &Path,
    home_dir: &Path,
    catalog_agent_home: &Path,
    template: &str,
) -> Result<TemplateProvenanceRecord> {
    initialize_agent_home_from_template_with_home_and_catalog(
        environment,
        agent_home,
        home_dir,
        Some(catalog_agent_home),
        template,
    )
    .await
}

pub async fn initialize_agent_home_without_template(
    environment: TemplateEnvironment,
    agent_home: &Path,
) -> Result<TemplateProvenanceRecord> {
    let home_dir = user_home_dir()?;
    initialize_agent_home_without_template_with_home(environment, agent_home, &home_dir).await
}

pub async fn initialize_agent_home_without_template_with_home(
    environment: TemplateEnvironment,
    agent_home: &Path,
    home_dir: &Path,
) -> Result<TemplateProvenanceRecord> {
    // Blank initialization applies no role template, so it needs neither the
    // template environment nor the catalog home. Keep the parameters for API
    // stability with the explicit-template entry points.
    let _ = (environment, home_dir);
    initialize_agent_home_blank(agent_home).await
}

pub async fn initialize_agent_home_from_template_with_home(
    environment: TemplateEnvironment,
    agent_home: &Path,
    home_dir: &Path,
    template: &str,
) -> Result<TemplateProvenanceRecord> {
    initialize_agent_home_from_template_with_home_and_catalog(
        environment,
        agent_home,
        home_dir,
        None,
        template,
    )
    .await
}

async fn initialize_agent_home_from_template_with_home_and_catalog(
    environment: TemplateEnvironment,
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
            environment,
            template,
            home_dir,
            catalog_agent_home.unwrap_or(&agent_home),
        )
        .await?;
        materialize_template(environment, &agent_home, &resolved).await?;
        let record = TemplateProvenanceRecord {
            selector: Some(template.to_string()),
            source: resolved.provenance,
            applied_at: Utc::now(),
            schema_version: resolved.schema_version,
        };
        tracing::info!(target: "holon::agent_template",
            template = ?record.selector,
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

async fn initialize_agent_home_blank(agent_home: &Path) -> Result<TemplateProvenanceRecord> {
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
        materialize_blank_agents_md(&agent_home).await
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

/// Writes the blank agent-home `AGENTS.md` and provenance marker. The caller
/// must have created the agent-home layout and ensured `AGENTS.md` is absent.
async fn materialize_blank_agents_md(agent_home: &Path) -> Result<TemplateProvenanceRecord> {
    let agents_md_path = agent_home.join(TEMPLATE_AGENTS_FILENAME);
    if agents_md_path.exists() {
        bail!(
            "agent home {} already has an AGENTS.md; blank initialization refuses to overwrite live content",
            agent_home.display()
        );
    }
    let agents_md = render_blank_agent_home_agents_md();
    let staging = TemplateStagingGuard::begin(agent_home, None, &agents_md)?;
    let mut wrote_agents_md = false;
    let result: Result<TemplateProvenanceRecord> = async {
        write_file_atomically(&agents_md_path, agents_md.as_bytes())?;
        wrote_agents_md = true;
        let record = TemplateProvenanceRecord {
            selector: None,
            source: TemplateProvenanceSource::Blank,
            applied_at: Utc::now(),
            schema_version: None,
        };
        tracing::info!(target: "holon::agent_template",
            agent_home = %agent_home.display(),
            "template_applied: agent initialized without a role template"
        );
        let content = serde_json::to_vec_pretty(&record)?;
        write_file_atomically(&template_provenance_path(agent_home), &content)?;
        staging.finish()?;
        Ok(record)
    }
    .await;

    if result.is_err() {
        if wrote_agents_md {
            let _ = fs::remove_file(&agents_md_path);
        }
        let _ = fs::remove_file(template_provenance_path(agent_home));
        let _ = remove_template_staging(agent_home);
    }

    result
}

pub async fn ensure_agent_home_agents_md_from_template(
    environment: TemplateEnvironment,
    agent_home: &Path,
    template: &str,
) -> Result<Option<TemplateProvenanceRecord>> {
    let home_dir = user_home_dir()?;
    ensure_agent_home_agents_md_from_template_with_home(
        environment,
        agent_home,
        &home_dir,
        template,
    )
    .await
}

pub async fn ensure_agent_home_agents_md_from_template_with_catalog(
    environment: TemplateEnvironment,
    agent_home: &Path,
    home_dir: &Path,
    catalog_agent_home: &Path,
    template: &str,
) -> Result<Option<TemplateProvenanceRecord>> {
    ensure_agent_home_agents_md_from_template_with_home_and_catalog(
        environment,
        agent_home,
        home_dir,
        Some(catalog_agent_home),
        template,
    )
    .await
}

pub async fn ensure_agent_home_agents_md_from_template_with_home(
    environment: TemplateEnvironment,
    agent_home: &Path,
    home_dir: &Path,
    template: &str,
) -> Result<Option<TemplateProvenanceRecord>> {
    ensure_agent_home_agents_md_from_template_with_home_and_catalog(
        environment,
        agent_home,
        home_dir,
        None,
        template,
    )
    .await
}

/// Ensures `AGENTS.md` from a hidden builtin template, deliberately bypassing
/// visible user templates so a local template that reuses the builtin id
/// cannot shadow the hidden default.
pub async fn ensure_agent_home_agents_md_from_builtin_template(
    environment: TemplateEnvironment,
    agent_home: &Path,
    home_dir: &Path,
    template_id: &str,
) -> Result<Option<TemplateProvenanceRecord>> {
    let agent_home = agent_home.to_path_buf();
    fs::create_dir_all(&agent_home)
        .with_context(|| format!("failed to create {}", agent_home.display()))?;
    ensure_agent_home_layout(&agent_home)?;
    if agent_home.join(TEMPLATE_AGENTS_FILENAME).exists() {
        return Ok(None);
    }
    let resolved = resolve_builtin_template(environment, template_id, home_dir)?;
    materialize_resolved_template(
        environment,
        &agent_home,
        Some(template_id),
        &resolved,
        "hidden builtin template",
    )
    .await
}

pub async fn ensure_agent_home_agents_md_without_template_with_home(
    _environment: TemplateEnvironment,
    agent_home: &Path,
    _home_dir: &Path,
) -> Result<Option<TemplateProvenanceRecord>> {
    ensure_agent_home_agents_md_blank(agent_home).await
}

async fn ensure_agent_home_agents_md_from_template_with_home_and_catalog(
    environment: TemplateEnvironment,
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
        environment,
        template,
        home_dir,
        catalog_agent_home.unwrap_or(&agent_home),
    )
    .await?;
    materialize_resolved_template(
        environment,
        &agent_home,
        Some(template),
        &resolved,
        "template",
    )
    .await
}

async fn ensure_agent_home_agents_md_blank(
    agent_home: &Path,
) -> Result<Option<TemplateProvenanceRecord>> {
    let agent_home = agent_home.to_path_buf();
    fs::create_dir_all(&agent_home)
        .with_context(|| format!("failed to create {}", agent_home.display()))?;
    ensure_agent_home_layout(&agent_home)?;
    let agents_md_path = agent_home.join(TEMPLATE_AGENTS_FILENAME);
    if agents_md_path.exists() {
        return Ok(None);
    }
    materialize_blank_agents_md(&agent_home).await.map(Some)
}

/// Shared materialization body for resolved templates: write AGENTS.md under
/// a runtime staging record, materialize skills (GitHub refs install to the
/// user library in one batched pass), then write the provenance marker and
/// clear staging. On error the created files are removed; if the future is
/// dropped mid-way the staging record stays behind so a later repair can
/// recover the half-materialized state without touching user content.
async fn materialize_resolved_template(
    environment: TemplateEnvironment,
    agent_home: &Path,
    selector: Option<&str>,
    resolved: &ResolvedTemplate,
    source_label: &str,
) -> Result<Option<TemplateProvenanceRecord>> {
    let agents_md_path = agent_home.join(TEMPLATE_AGENTS_FILENAME);
    let mut created_skill_destinations = Vec::new();

    let result: Result<TemplateProvenanceRecord> = async {
        let agents_md = render_agent_home_agents_md(&resolved.agents_md, None);
        // Land the staging record before any visible artifact so an
        // interrupted run is always distinguishable from user content.
        let mut staging = TemplateStagingGuard::begin(agent_home, selector, &agents_md)?;
        write_file_atomically(&agents_md_path, agents_md.as_bytes())?;
        let mut record_destination = |destination: PathBuf| -> Result<()> {
            staging.push_created_path(destination.clone())?;
            created_skill_destinations.push(destination);
            Ok(())
        };
        materialize_template_skill_refs(
            environment,
            agent_home,
            &resolved.skill_refs,
            &mut record_destination,
        )
        .await?;
        let record = TemplateProvenanceRecord {
            selector: selector.map(str::to_string),
            source: resolved.provenance.clone(),
            applied_at: Utc::now(),
            schema_version: resolved.schema_version.clone(),
        };
        tracing::info!(target: "holon::agent_template",
            template = ?record.selector,
            schema_version = ?record.schema_version,
            agent_home = %agent_home.display(),
            "template_applied: agent initialized from {source_label}"
        );
        let content = serde_json::to_vec_pretty(&record)?;
        write_file_atomically(&template_provenance_path(agent_home), &content)?;
        staging.finish()?;
        Ok(record)
    }
    .await;

    match result {
        Ok(record) => Ok(Some(record)),
        Err(err) => {
            let _ = fs::remove_file(&agents_md_path);
            let _ = fs::remove_file(template_provenance_path(agent_home));
            for destination in created_skill_destinations.into_iter().rev() {
                let _ = remove_materialized_skill_destination(&destination);
            }
            let _ = remove_template_staging(agent_home);
            Err(err)
        }
    }
}

pub fn templates_root_for_home(home_dir: &Path) -> PathBuf {
    home_dir.join(".agents").join("agent_templates")
}

fn template_registry_path(templates_root: &Path) -> PathBuf {
    templates_root.join(TEMPLATE_REGISTRY_FILENAME)
}

fn empty_template_registry() -> AgentTemplateRegistry {
    AgentTemplateRegistry {
        version: 1,
        sources: BTreeMap::new(),
        installed: BTreeMap::new(),
    }
}

fn load_template_registry(templates_root: &Path) -> Result<AgentTemplateRegistry> {
    let path = template_registry_path(templates_root);
    if !path.is_file() {
        return Ok(empty_template_registry());
    }
    let content =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    if content.trim().is_empty() {
        return Ok(empty_template_registry());
    }
    let registry: AgentTemplateRegistry = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    if registry.version != 1 {
        bail!(
            "unsupported agent template registry version {} in {}",
            registry.version,
            path.display()
        );
    }
    Ok(registry)
}

fn write_template_registry(templates_root: &Path, registry: &AgentTemplateRegistry) -> Result<()> {
    let content = serde_json::to_vec_pretty(registry)?;
    write_file_atomically(&template_registry_path(templates_root), &content)
}

pub fn discover_agent_templates_catalog(
    environment: TemplateEnvironment,
    user_home: Option<&Path>,
    agent_home: &Path,
) -> Vec<AgentTemplateCatalogEntry> {
    let user_templates_root = user_home.map(templates_root_for_home);
    let user_entries = if let Some(root) = user_templates_root.as_deref() {
        discover_local_templates(
            environment,
            root,
            AgentTemplateSourceKind::UserGlobal,
            false,
        )
    } else {
        Vec::new()
    };
    let agent_home_entries = discover_local_templates(
        environment,
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

fn builtin_template_catalog_entry(
    environment: TemplateEnvironment,
    builtin: &BuiltinTemplate,
) -> AgentTemplateCatalogEntry {
    let manifest = parse_template_manifest(environment, builtin.template_toml);
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
    environment: TemplateEnvironment,
    root: &Path,
    source: AgentTemplateSourceKind,
    use_absolute_selector: bool,
) -> Vec<AgentTemplateCatalogEntry> {
    crate::read_dir(environment.io, root, |read_dir| {
        let mut entries = Vec::new();
        for entry in read_dir.flatten() {
            let path = entry.path();
            let Some(template_id) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if template_id.starts_with('.') || !path.is_dir() {
                continue;
            }
            if validate_template_install_id(template_id).is_err() {
                continue;
            }
            let agents_md_path = path.join(TEMPLATE_AGENTS_FILENAME);
            let Ok(agents_md) = environment.io.read_to_string(&agents_md_path) else {
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
            let manifest = parse_local_template_manifest(environment, &path);
            let display_template_id = manifest
                .as_ref()
                .map(|m| m.id.clone())
                .filter(|id| validate_template_id(id).is_ok())
                .unwrap_or_else(|| template_id.to_string());
            entries.push(AgentTemplateCatalogEntry {
                catalog_id: format!("{}:{}", source.label(), template_id),
                template,
                template_id: display_template_id,
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
                included_skills: local_template_skills(environment, &path),
                source_id: None,
                resolved_ref: None,
                resolved_revision: None,
                source_url: None,
            });
        }
        entries
    })
    .unwrap_or_default()
}

pub struct RemoteTemplateSync {
    pub status: AgentTemplateRemoteSourceStatus,
    pub catalog: Vec<AgentTemplateCatalogEntry>,
    pub diagnostics: Vec<AgentTemplateCatalogDiagnostic>,
}

pub async fn install_remote_template_source(
    environment: TemplateEnvironment,
    user_home: &Path,
    source_id: &str,
    source_url: &str,
    parsed: ParsedGithubRepoUrl,
    requested_ref: Option<&str>,
    configured_token: Option<String>,
) -> Result<RemoteTemplateSync> {
    let github = GitHubTemplateClient::with_token(configured_token).await?;
    let resolved_ref = requested_ref
        .map(str::to_string)
        .or(parsed.git_ref)
        .unwrap_or_else(|| "HEAD".to_string());
    let archive = GitHubArchive::fetch(
        &github.client,
        &parsed.owner,
        &parsed.repo,
        &resolved_ref,
        &(environment.archive_base)(),
    )
    .await?;
    let mut catalog = discover_github_repo_templates_from_archive(
        environment,
        &archive,
        &parsed.owner,
        &parsed.repo,
        &resolved_ref,
    )?;
    for entry in &mut catalog {
        entry.source_id = Some(source_id.to_string());
        entry.resolved_ref = Some(resolved_ref.clone());
        entry.resolved_revision = None;
        entry.catalog_id = format!("remote:{source_id}:{}", entry.template_id);
        entry.template = entry.catalog_id.clone();
    }
    let now = Utc::now();
    let templates_root = templates_root_for_home(user_home);
    fs::create_dir_all(&templates_root)
        .with_context(|| format!("failed to create {}", templates_root.display()))?;
    let mut registry = load_template_registry(&templates_root)?;
    registry.sources.insert(
        source_id.to_string(),
        AgentTemplateRegistrySource {
            kind: "github".into(),
            url: source_url.to_string(),
            requested_ref: requested_ref.map(str::to_string),
            resolved_ref: resolved_ref.clone(),
            catalog_path: DEFAULT_REMOTE_TEMPLATE_DIR.into(),
            catalog_hash: catalog_hash(&catalog)?,
            synced_at: now,
        },
    );
    let mut diagnostics = Vec::new();
    let mut failures = Vec::new();
    let mut installed = 0usize;
    for entry in &catalog {
        let result = install_remote_template_catalog_entry_from_archive(
            environment,
            &archive,
            &templates_root,
            &registry,
            source_id,
            source_url,
            &resolved_ref,
            now,
            entry,
        );
        match result {
            Ok(record) => {
                installed += 1;
                registry.installed.insert(record.local_path.clone(), record);
                write_template_registry(&templates_root, &registry)?;
            }
            Err(error) => {
                let message = format!(
                    "failed to install remote template {}: {error:#}",
                    entry.template_id
                );
                diagnostics.push(AgentTemplateCatalogDiagnostic {
                    source_id: source_id.to_string(),
                    severity: "error".into(),
                    message: message.clone(),
                });
                failures.push(message);
            }
        }
    }
    let status_kind = match (failures.is_empty(), installed > 0) {
        (true, _) => AgentTemplateRemoteSourceSyncStatus::Synced,
        (false, true) => AgentTemplateRemoteSourceSyncStatus::PartiallyFailed,
        (false, false) => AgentTemplateRemoteSourceSyncStatus::Failed,
    };
    let error = (!failures.is_empty()).then(|| failures.join("; "));
    let status = AgentTemplateRemoteSourceStatus {
        source_id: source_id.to_string(),
        kind: "github".into(),
        url: source_url.to_string(),
        requested_ref: requested_ref.map(str::to_string),
        enabled: true,
        status: status_kind,
        last_synced_at: Some(now),
        resolved_ref: Some(resolved_ref),
        resolved_revision: None,
        error,
    };
    Ok(RemoteTemplateSync {
        status,
        catalog,
        diagnostics,
    })
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
fn parse_template_manifest(
    environment: TemplateEnvironment,
    content: &str,
) -> Option<TemplateManifest> {
    crate::parse_template(environment.io, content, || {
        toml::from_str(content)
            .map_err(|error| {
                tracing::warn!(target: "holon::agent_template", %error, "failed to parse template.toml manifest");
            })
            .ok()
    }).ok().flatten()
}

/// Parse `template.toml` from a local template directory.
fn parse_local_template_manifest(
    environment: TemplateEnvironment,
    template_dir: &Path,
) -> Option<TemplateManifest> {
    let manifest_path = template_dir.join(TEMPLATE_MANIFEST_FILENAME);
    environment
        .io
        .read_to_string(&manifest_path)
        .ok()
        .and_then(|content| parse_template_manifest(environment, &content))
}

/// Resolve a catalog entry into a detailed template view.
///
/// Reads AGENTS.md content and skill dependencies for the template, suitable
/// for GUI or daemon API detail responses.
///
#[allow(dead_code)]
pub fn resolve_agent_template_detail(
    environment: TemplateEnvironment,
    entry: &AgentTemplateCatalogEntry,
) -> Option<AgentTemplateDetail> {
    let (agents_md, skills) = match entry.source {
        AgentTemplateSourceKind::Builtin => {
            let builtin = environment
                .builtin_templates
                .iter()
                .find(|t| t.template_id == entry.template_id)?;
            let skills = builtin
                .skill_names
                .iter()
                .map(|&name| AgentTemplateSkillDependency {
                    kind: "github".to_string(),
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
                    tracing::warn!(target: "holon::agent_template",
                        template_path = %agents_md_path.display(),
                        %error,
                        "failed to read AGENTS.md for template detail"
                    );
                    return None;
                }
            };
            let skills = parse_skill_refs(environment, path.join(TEMPLATE_SKILLS_FILENAME))
                .unwrap_or_default()
                .into_iter()
                .map(|skill_ref| match skill_ref {
                    TemplateSkillRef::Local { path } => AgentTemplateSkillDependency {
                        kind: "local".to_string(),
                        reference: path.display().to_string(),
                    },
                    TemplateSkillRef::Github(github_ref) => AgentTemplateSkillDependency {
                        kind: "github".to_string(),
                        reference: github_ref.display_reference(),
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

pub async fn resolve_remote_agent_template_detail(
    environment: TemplateEnvironment,
    entry: &AgentTemplateCatalogEntry,
) -> Result<AgentTemplateDetail> {
    let source_url = entry
        .source_url
        .as_deref()
        .ok_or_else(|| anyhow!("remote template {} has no source URL", entry.catalog_id))?;
    let resolved = resolve_github_template(environment, source_url).await?;
    let skills = resolved
        .skill_refs
        .into_iter()
        .map(|skill_ref| match skill_ref {
            TemplateSkillRef::Local { path } => AgentTemplateSkillDependency {
                kind: "local".to_string(),
                reference: path.display().to_string(),
            },
            TemplateSkillRef::Github(github_ref) => AgentTemplateSkillDependency {
                kind: "github".to_string(),
                reference: github_ref.display_reference(),
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

fn choose_remote_template_install_id(
    templates_root: &Path,
    registry: &AgentTemplateRegistry,
    source_id: &str,
    entry: &AgentTemplateCatalogEntry,
) -> Result<String> {
    for record in registry.installed.values() {
        if record.source_id == source_id && record.template_id == entry.template_id {
            validate_template_install_id(&record.local_path)?;
            return Ok(record.local_path.clone());
        }
    }

    let base = entry.template_id.clone();
    validate_template_id(&base)?;
    if remote_template_install_id_available_for_source(templates_root, &base, source_id)? {
        return Ok(base);
    }

    let suffix = install_id_suffix(source_id);
    for index in 0..1000 {
        let candidate = if index == 0 {
            format!("{}@{}", entry.template_id, suffix)
        } else {
            format!("{}@{}-{}", entry.template_id, suffix, index + 1)
        };
        validate_template_install_id(&candidate)?;
        if remote_template_install_id_available_for_source(templates_root, &candidate, source_id)? {
            return Ok(candidate);
        }
    }
    bail!(
        "failed to allocate install_id for remote template '{}' from source '{}'",
        entry.template_id,
        source_id
    )
}

fn remote_template_install_id_available_for_source(
    templates_root: &Path,
    install_id: &str,
    source_id: &str,
) -> Result<bool> {
    let destination = templates_root.join(install_id);
    if !destination.exists() {
        return Ok(true);
    }
    let Some(state) = read_managed_template_state(&destination)? else {
        return Ok(false);
    };
    Ok(matches!(
        state.owner,
        ManagedTemplateOwner::RemoteSource {
            source_id: ref existing_source,
            ..
        } if existing_source == source_id
    ))
}

fn install_id_suffix(source_id: &str) -> String {
    let suffix = source_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if suffix.is_empty() {
        "remote".into()
    } else {
        suffix
    }
}

fn local_template_skills(environment: TemplateEnvironment, path: &Path) -> Vec<String> {
    match parse_skill_refs(environment, path.join(TEMPLATE_SKILLS_FILENAME)) {
        Ok(skill_refs) => skill_ref_names(skill_refs),
        Err(error) => {
            tracing::warn!(target: "holon::agent_template",
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
            TemplateSkillRef::Github(github_ref) => github_ref.display_reference(),
        })
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

fn official_template_skill_ref(name: &str) -> TemplateGithubSkillRef {
    TemplateGithubSkillRef::Structured {
        repo: "holon-run/holon".to_string(),
        path: format!("skills/{name}"),
        git_ref: Some("main".to_string()),
    }
}

pub fn user_home_dir() -> Result<PathBuf> {
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
    environment: TemplateEnvironment,
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
            environment,
            path,
            TemplateProvenanceSource::LocalPath {
                path: PathBuf::from(template),
            },
        );
    }

    if is_github_tree_url(template)? {
        return resolve_github_template(environment, template).await;
    }

    let entry =
        resolve_template_catalog_entry(environment, template, home_dir, catalog_agent_home)?;
    resolve_catalog_template(environment, entry, home_dir).await
}

/// Read an optional `[execution]` binding from a resolved template manifest.
pub async fn template_execution_binding(
    environment: TemplateEnvironment,
    template: &str,
    home_dir: &Path,
    catalog_agent_home: &Path,
) -> Result<Option<TemplateExecutionBinding>> {
    let resolved = resolve_template(environment, template, home_dir, catalog_agent_home).await?;
    let Some(manifest) = resolved.template_toml.as_deref() else {
        return Ok(None);
    };
    let parsed: TemplateManifest = toml::from_str(manifest)
        .with_context(|| format!("failed to parse template manifest for `{template}`"))?;
    Ok(parsed.execution.map(|binding| *binding).filter(|binding| {
        binding.driver.is_some() || binding.harness_profile.is_some() || binding.role.is_some()
    }))
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

fn validate_template_install_id(install_id: &str) -> Result<()> {
    if install_id.contains('/') || install_id.contains('\\') {
        bail!("template install_id must not be path-like");
    }
    if install_id == "." || install_id == ".." || install_id.contains("..") {
        bail!("template install_id must be a simple stable name");
    }
    if !install_id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | '@'))
    {
        bail!("template install_id contains unsupported characters");
    }
    Ok(())
}

fn validate_template_catalog_selector_id(
    source: AgentTemplateSourceKind,
    selector_id: &str,
) -> Result<()> {
    match source {
        AgentTemplateSourceKind::UserGlobal | AgentTemplateSourceKind::AgentHome => {
            validate_template_install_id(selector_id)
        }
        AgentTemplateSourceKind::Builtin | AgentTemplateSourceKind::Remote => {
            validate_template_id(selector_id)
        }
    }
}

fn resolve_template_catalog_entry(
    environment: TemplateEnvironment,
    template: &str,
    home_dir: &Path,
    catalog_agent_home: &Path,
) -> Result<AgentTemplateCatalogEntry> {
    if let Some((source_label, template_id)) = template.split_once(':') {
        let Some(source) = AgentTemplateSourceKind::from_label(source_label) else {
            return Err(unknown_template_error(
                environment,
                template,
                home_dir,
                catalog_agent_home,
            ));
        };
        validate_template_catalog_selector_id(source, template_id)?;
        return resolve_prefixed_template_catalog_entry(
            environment,
            source,
            template_id,
            home_dir,
            catalog_agent_home,
        )
        .ok_or_else(|| {
            unknown_template_error(environment, template, home_dir, catalog_agent_home)
        });
    }

    validate_template_install_id(template)?;
    let catalog = discover_agent_templates_catalog(environment, Some(home_dir), catalog_agent_home);
    if let Some(entry) = catalog
        .iter()
        .find(|entry| entry.template_id == template || entry.template == template)
        .cloned()
    {
        return Ok(entry);
    }
    environment
        .builtin_templates
        .iter()
        .find(|builtin| builtin.template_id == template)
        .map(|builtin| builtin_template_catalog_entry(environment, builtin))
        .ok_or_else(|| unknown_template_error(environment, template, home_dir, catalog_agent_home))
}

fn resolve_prefixed_template_catalog_entry(
    environment: TemplateEnvironment,
    source: AgentTemplateSourceKind,
    template_id: &str,
    home_dir: &Path,
    catalog_agent_home: &Path,
) -> Option<AgentTemplateCatalogEntry> {
    match source {
        AgentTemplateSourceKind::Builtin => environment
            .builtin_templates
            .iter()
            .find(|template| template.template_id == template_id)
            .map(|builtin| builtin_template_catalog_entry(environment, builtin)),
        AgentTemplateSourceKind::Remote => None,
        AgentTemplateSourceKind::UserGlobal => discover_local_templates(
            environment,
            &templates_root_for_home(home_dir),
            AgentTemplateSourceKind::UserGlobal,
            false,
        )
        .into_iter()
        .find(|entry| entry.template_id == template_id || entry.template == template_id),
        AgentTemplateSourceKind::AgentHome => discover_local_templates(
            environment,
            &catalog_agent_home.join("agent_templates"),
            AgentTemplateSourceKind::AgentHome,
            true,
        )
        .into_iter()
        .find(|entry| {
            entry.template_id == template_id
                || entry
                    .path
                    .as_ref()
                    .and_then(|path| path.file_name())
                    .and_then(|name| name.to_str())
                    == Some(template_id)
        }),
    }
}

async fn resolve_catalog_template(
    environment: TemplateEnvironment,
    entry: AgentTemplateCatalogEntry,
    home_dir: &Path,
) -> Result<ResolvedTemplate> {
    match entry.source {
        AgentTemplateSourceKind::Builtin => {
            resolve_builtin_template(environment, &entry.template_id, home_dir)
        }
        AgentTemplateSourceKind::Remote => {
            unreachable!("remote templates are materialized locally")
        }
        AgentTemplateSourceKind::UserGlobal | AgentTemplateSourceKind::AgentHome => {
            let path = entry.path.clone().ok_or_else(|| {
                anyhow!(
                    "template catalog entry {} has no local source path",
                    entry.catalog_id
                )
            })?;
            let resolved = resolve_local_template(
                environment,
                path.clone(),
                TemplateProvenanceSource::TemplateId {
                    template_id: entry.template_id.clone(),
                    path,
                },
            )?;
            Ok(resolved)
        }
    }
}

fn resolve_builtin_template(
    environment: TemplateEnvironment,
    template_id: &str,
    home_dir: &Path,
) -> Result<ResolvedTemplate> {
    let builtin = environment
        .builtin_templates
        .iter()
        .find(|template| template.template_id == template_id)
        .ok_or_else(|| anyhow!("unknown builtin template id {template_id}"))?;
    let skill_refs: Vec<TemplateSkillRef> = builtin
        .skill_names
        .iter()
        .map(|&name| TemplateSkillRef::Github(official_template_skill_ref(name)))
        .collect();
    Ok(ResolvedTemplate {
        provenance: TemplateProvenanceSource::TemplateId {
            template_id: template_id.to_string(),
            path: templates_root_for_home(home_dir).join(template_id),
        },
        agents_md: builtin.agents_md.to_string(),
        template_toml: Some(builtin.template_toml.to_string()),
        skill_refs,
        schema_version: parse_template_manifest(environment, builtin.template_toml)
            .map(|m| m.schema),
    })
}

fn unknown_template_error(
    environment: TemplateEnvironment,
    template: &str,
    home_dir: &Path,
    catalog_agent_home: &Path,
) -> anyhow::Error {
    tracing::warn!(target: "holon::agent_template",
        template = %template,
        home_dir = %home_dir.display(),
        catalog_agent_home = %catalog_agent_home.display(),
        "unknown template selector — diagnosing home_dir mismatch",
    );
    let catalog = discover_agent_templates_catalog(environment, Some(home_dir), catalog_agent_home);
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
    environment: TemplateEnvironment,
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
    let skill_refs = parse_skill_refs(environment, path.join(TEMPLATE_SKILLS_FILENAME))?;

    Ok(ResolvedTemplate {
        provenance,
        agents_md,
        template_toml: read_template_manifest_content(&path),
        skill_refs,
        schema_version: read_template_schema_version(environment, &path),
    })
}

fn read_template_manifest_content(template_dir: &Path) -> Option<String> {
    let manifest_path = template_dir.join(TEMPLATE_MANIFEST_FILENAME);
    fs::read_to_string(&manifest_path).ok()
}

fn read_template_schema_version(
    environment: TemplateEnvironment,
    template_dir: &Path,
) -> Option<String> {
    let content = read_template_manifest_content(template_dir)?;
    parse_template_manifest(environment, &content).map(|m| m.schema)
}

fn parse_skill_refs(
    environment: TemplateEnvironment,
    path: PathBuf,
) -> Result<Vec<TemplateSkillRef>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = environment.io.read_to_string(&path);
    environment.io.check_active_budget()?;
    let content = content.with_context(|| format!("failed to read {}", path.display()))?;
    crate::parse_template(environment.io, &content, || {
        let manifest: TemplateSkillsManifest = toml::from_str(&content)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        let skill_refs: Vec<TemplateSkillRef> = manifest
            .skills
            .into_iter()
            .map(parse_template_skill_file_ref)
            .collect::<Result<Vec<_>>>()
            .with_context(|| format!("failed to parse {}", path.display()))?;
        for skill_ref in &skill_refs {
            match skill_ref {
                TemplateSkillRef::Github(github_ref) => {
                    template_github_skill_install_kind(github_ref)?;
                }
                TemplateSkillRef::Local { .. } => {}
            }
        }
        Ok(skill_refs)
    })?
}

fn parse_template_skill_file_ref(file_ref: TemplateSkillFileRef) -> Result<TemplateSkillRef> {
    match file_ref {
        TemplateSkillFileRef::Builtin { name } => {
            bail!(
                "skills.toml no longer supports kind = \"builtin\" for '{name}'; \
                 use kind = \"github\" with repo = \"holon-run/holon\" and path = \"skills/{name}\""
            )
        }
        TemplateSkillFileRef::Local { path } => Ok(TemplateSkillRef::Local { path }),
        TemplateSkillFileRef::Github {
            repo,
            path,
            git_ref,
            uses,
            package,
        } => parse_template_github_skill_ref(repo, path, git_ref, uses, package)
            .map(TemplateSkillRef::Github),
    }
}

fn parse_template_github_skill_ref(
    repo: Option<String>,
    path: Option<String>,
    git_ref: Option<String>,
    uses: Option<String>,
    package: Option<String>,
) -> Result<TemplateGithubSkillRef> {
    if let Some(package) = package {
        if repo.is_some() || path.is_some() || git_ref.is_some() || uses.is_some() {
            bail!("github skill ref must use exactly one of legacy package, uses, or structured repo/path/ref fields");
        }
        validate_template_github_skill_package(&package)?;
        return Ok(TemplateGithubSkillRef::LegacyPackage { package });
    }

    if let Some(uses) = uses {
        if repo.is_some() || path.is_some() || git_ref.is_some() {
            bail!("github skill ref must use exactly one of legacy package, uses, or structured repo/path/ref fields");
        }
        return parse_template_github_skill_uses(&uses);
    }

    let repo = repo.context("github skill ref requires repo")?;
    let path = path.context("github skill ref requires path")?;
    validate_template_github_skill_repo(&repo)?;
    validate_template_github_skill_path(&path)?;
    if let Some(git_ref) = &git_ref {
        validate_template_github_skill_ref(git_ref)?;
    }
    Ok(TemplateGithubSkillRef::Structured {
        repo,
        path,
        git_ref,
    })
}

fn parse_template_github_skill_uses(uses: &str) -> Result<TemplateGithubSkillRef> {
    if uses.trim() != uses || uses.is_empty() {
        bail!("github skill ref uses must not be empty or contain leading/trailing whitespace");
    }
    if let Some(path) = uses
        .strip_prefix("https://github.com/")
        .or_else(|| uses.strip_prefix("http://github.com/"))
    {
        return parse_template_github_skill_tree_url(path);
    }

    let (path_ref, git_ref) = split_template_github_skill_uses_ref(uses)?;
    let mut parts = path_ref.split('/');
    let owner = parts.next().unwrap_or_default();
    let repo_name = parts.next().unwrap_or_default();
    let path = parts.collect::<Vec<_>>().join("/");
    if owner.is_empty() || repo_name.is_empty() || path.is_empty() {
        bail!("github skill ref uses must be in owner/repo/path@ref form");
    }
    build_structured_template_github_skill_ref(
        format!("{owner}/{repo_name}"),
        path,
        Some(git_ref.to_string()),
    )
}

fn parse_template_github_skill_tree_url(path: &str) -> Result<TemplateGithubSkillRef> {
    let parts = path.split('/').collect::<Vec<_>>();
    if parts.len() < 5 || parts[0].is_empty() || parts[1].is_empty() || parts[2] != "tree" {
        bail!(
            "github skill ref uses value with a GitHub URL must be a tree URL (owner/repo/tree/ref/path)"
        );
    }
    let repo = format!("{}/{}", parts[0], parts[1].trim_end_matches(".git"));
    let git_ref = parts[3];
    let skill_path = parts[4..].join("/");
    // Delegate empty trailing path validation to the shared path validator.
    build_structured_template_github_skill_ref(repo, skill_path, Some(git_ref.to_string()))
}

fn split_template_github_skill_uses_ref(uses: &str) -> Result<(&str, &str)> {
    if let Some((path_ref, git_ref)) = uses.rsplit_once('@') {
        if !path_ref.is_empty() && !git_ref.is_empty() {
            return Ok((path_ref, git_ref));
        }
    }
    if let Some((path_ref, git_ref)) = uses.rsplit_once('#') {
        if !path_ref.is_empty() && !git_ref.is_empty() {
            return Ok((path_ref, git_ref));
        }
    }
    bail!(
        "github skill ref uses must include a ref via owner/repo/path@ref or owner/repo/path#ref"
    );
}

fn build_structured_template_github_skill_ref(
    repo: String,
    path: String,
    git_ref: Option<String>,
) -> Result<TemplateGithubSkillRef> {
    validate_template_github_skill_repo(&repo)?;
    validate_template_github_skill_path(&path)?;
    if let Some(git_ref) = &git_ref {
        validate_template_github_skill_ref(git_ref)?;
    }
    Ok(TemplateGithubSkillRef::Structured {
        repo,
        path,
        git_ref,
    })
}

fn template_github_skill_install_kind(
    github_ref: &TemplateGithubSkillRef,
) -> Result<SkillInstallKind> {
    let (package, skill) = template_github_skill_package(github_ref)?;
    Ok(SkillInstallKind::Remote {
        package,
        skill,
        mode: SkillInstallMode::Linked,
    })
}

fn template_github_skill_package(
    github_ref: &TemplateGithubSkillRef,
) -> Result<(String, Option<String>)> {
    let package = match github_ref {
        TemplateGithubSkillRef::Structured {
            repo,
            path,
            git_ref,
        } => {
            let git_ref = git_ref.as_deref().unwrap_or("HEAD");
            format!("https://github.com/{repo}/tree/{git_ref}/{path}")
        }
        TemplateGithubSkillRef::LegacyPackage { package } => {
            validate_template_github_skill_package(package)?;
            let (remote_package, skill) = split_template_github_skill_package(package)?;
            return Ok((remote_package.to_string(), skill.map(str::to_string)));
        }
    };
    Ok((package, None))
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

fn validate_template_github_skill_repo(repo: &str) -> Result<()> {
    validate_template_github_skill_package(repo)?;
    let mut parts = repo.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    if owner.is_empty() || name.is_empty() || parts.next().is_some() {
        bail!("github skill ref repo must be in owner/repo form");
    }
    Ok(())
}

fn validate_template_github_skill_path(path: &str) -> Result<()> {
    validate_template_github_skill_package(path)?;
    if path == "." {
        return Ok(());
    }
    if path.starts_with('/') || path.starts_with('-') {
        bail!("github skill ref path must be a relative repository path");
    }
    if path
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        bail!("github skill ref path must not contain empty, '.' or '..' segments");
    }
    Ok(())
}

fn validate_template_github_skill_ref(git_ref: &str) -> Result<()> {
    if git_ref.trim().is_empty() {
        bail!("github skill ref ref must not be empty");
    }
    if git_ref.trim() != git_ref {
        bail!("github skill ref ref must not contain leading or trailing whitespace");
    }
    if git_ref
        .chars()
        .any(|ch| ch.is_control() || ch.is_ascii_whitespace())
    {
        bail!("github skill ref ref must not contain whitespace or control characters");
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

fn read_managed_template_state(template_dir: &Path) -> Result<Option<ManagedTemplateState>> {
    let path = builtin_template_state_path(template_dir);
    if !path.is_file() {
        return Ok(None);
    }
    let content =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    if content.trim().is_empty() {
        return Ok(None);
    }
    serde_json::from_str(&content)
        .map(Some)
        .with_context(|| format!("failed to parse {}", path.display()))
}

fn resolved_template_content_hash(resolved: &ResolvedTemplate) -> String {
    use sha2::{Digest as _, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(TEMPLATE_AGENTS_FILENAME.as_bytes());
    hasher.update(b"\n");
    hasher.update(resolved.agents_md.as_bytes());
    if let Some(schema_version) = resolved.schema_version.as_deref() {
        hasher.update(b"\n");
        hasher.update(b"schema_version\n");
        hasher.update(schema_version.as_bytes());
    }
    if let Some(template_toml) = resolved.template_toml.as_deref() {
        hasher.update(b"\n");
        hasher.update(TEMPLATE_MANIFEST_FILENAME.as_bytes());
        hasher.update(b"\n");
        hasher.update(template_toml.as_bytes());
    }
    for skill_ref in &resolved.skill_refs {
        hasher.update(b"\n");
        match skill_ref {
            TemplateSkillRef::Local { path } => {
                hasher.update(b"local:");
                hasher.update(path.display().to_string().as_bytes());
            }
            TemplateSkillRef::Github(github_ref) => {
                hasher.update(b"github:");
                hasher.update(github_ref.display_reference().as_bytes());
            }
        }
    }
    format!("{:x}", hasher.finalize())
}

fn catalog_hash(catalog: &[AgentTemplateCatalogEntry]) -> Result<String> {
    use sha2::{Digest as _, Sha256};

    let catalog_json = serde_json::to_vec(catalog)?;
    let mut hasher = Sha256::new();
    hasher.update(&catalog_json);
    Ok(format!("{:x}", hasher.finalize()))
}

fn remote_template_source_path(source_url: &str) -> String {
    let Ok(url) = reqwest::Url::parse(source_url) else {
        return source_url.to_string();
    };
    let Some(segments) = url
        .path_segments()
        .map(|segments| segments.collect::<Vec<_>>())
    else {
        return source_url.to_string();
    };
    if segments.len() < 5 || segments[2] != "tree" {
        return source_url.to_string();
    }
    segments[4..].join("/")
}

fn current_managed_template_content_hash(
    environment: TemplateEnvironment,
    template_dir: &Path,
) -> Result<String> {
    let resolved = resolve_local_template(
        environment,
        template_dir.to_path_buf(),
        TemplateProvenanceSource::TemplateId {
            template_id: template_dir
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string(),
            path: template_dir.to_path_buf(),
        },
    )?;
    Ok(resolved_template_content_hash(&resolved))
}

fn managed_template_has_local_changes(
    environment: TemplateEnvironment,
    template_dir: &Path,
    state: &ManagedTemplateState,
) -> Result<bool> {
    match current_managed_template_content_hash(environment, template_dir) {
        Ok(content_hash) => Ok(content_hash != state.content_hash),
        Err(error) => {
            tracing::warn!(target: "holon::agent_template",
                template_dir = %template_dir.display(),
                error = ?error,
                "failed to resolve managed template content; treating it as locally changed"
            );
            Ok(true)
        }
    }
}

fn write_managed_template_dir(
    environment: TemplateEnvironment,
    template_dir: &Path,
    resolved: &ResolvedTemplate,
    state: &ManagedTemplateState,
) -> Result<()> {
    if template_dir.exists() {
        let existing_state = read_managed_template_state(template_dir)?;
        match existing_state {
            Some(existing)
                if matches!(
                    (&existing.owner, &state.owner),
                    (
                        ManagedTemplateOwner::RemoteSource {
                            source_id: existing_source,
                            ..
                        },
                        ManagedTemplateOwner::RemoteSource {
                            source_id: new_source,
                            ..
                        }
                    ) if existing_source == new_source
                ) =>
            {
                if managed_template_has_local_changes(environment, template_dir, &existing)? {
                    bail!(
                        "template '{}' at {} has local changes; refusing to overwrite managed template",
                        existing.template_id,
                        template_dir.display()
                    );
                }
                fs::remove_dir_all(template_dir)
                    .with_context(|| format!("failed to clear {}", template_dir.display()))?;
            }
            Some(existing) => {
                bail!(
                    "template '{}' at {} is managed by {:?}; refusing to overwrite from another source",
                    existing.template_id,
                    template_dir.display(),
                    existing.owner
                );
            }
            None => {
                bail!(
                    "template directory {} already exists without managed metadata; refusing to overwrite user-owned template",
                    template_dir.display()
                );
            }
        }
    }

    fs::create_dir_all(template_dir)
        .with_context(|| format!("failed to create {}", template_dir.display()))?;
    write_file_atomically(
        &template_dir.join(TEMPLATE_AGENTS_FILENAME),
        resolved.agents_md.as_bytes(),
    )?;
    if let Some(manifest) = resolved.template_toml.as_deref() {
        write_file_atomically(
            &template_dir.join(TEMPLATE_MANIFEST_FILENAME),
            manifest.as_bytes(),
        )?;
    }
    if !resolved.skill_refs.is_empty() {
        write_template_skills_file(template_dir, &resolved.skill_refs)?;
    }
    let content = serde_json::to_vec_pretty(state)?;
    write_file_atomically(&builtin_template_state_path(template_dir), &content)?;
    Ok(())
}

fn write_template_skills_file(template_dir: &Path, skill_refs: &[TemplateSkillRef]) -> Result<()> {
    let mut content = String::new();
    for skill_ref in skill_refs {
        match skill_ref {
            TemplateSkillRef::Local { path } => {
                content.push_str("[[skills]]\nkind = \"local\"\npath = \"");
                content.push_str(&toml_escape(&path.display().to_string()));
                content.push_str("\"\n\n");
            }
            TemplateSkillRef::Github(github_ref) => match github_ref {
                TemplateGithubSkillRef::Structured {
                    repo,
                    path,
                    git_ref,
                } => {
                    content.push_str("[[skills]]\nkind = \"github\"\nrepo = \"");
                    content.push_str(&toml_escape(repo));
                    content.push_str("\"\npath = \"");
                    content.push_str(&toml_escape(path));
                    content.push('"');
                    if let Some(git_ref) = git_ref {
                        content.push_str("\nref = \"");
                        content.push_str(&toml_escape(git_ref));
                        content.push('"');
                    }
                    content.push_str("\n\n");
                }
                TemplateGithubSkillRef::LegacyPackage { package } => {
                    content.push_str("[[skills]]\nkind = \"github\"\npackage = \"");
                    content.push_str(&toml_escape(package));
                    content.push_str("\"\n\n");
                }
            },
        }
    }
    write_file_atomically(
        &template_dir.join(TEMPLATE_SKILLS_FILENAME),
        content.as_bytes(),
    )
}

fn toml_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
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

async fn resolve_github_template(
    environment: TemplateEnvironment,
    template: &str,
) -> Result<ResolvedTemplate> {
    resolve_github_template_archive_first(environment, template).await
}

struct GitHubTemplateClient {
    client: AsyncClient,
}

impl GitHubTemplateClient {
    async fn new() -> Result<Self> {
        Self::with_token(None).await
    }

    async fn with_token(configured_token: Option<String>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .build()
            .context("failed to build GitHub template client")?;
        Ok(Self {
            client: AsyncClient::new(
                client,
                GitHubAuth::resolve(configured_token, GITHUB_TEMPLATE_TOKEN_ENV_VARS).await,
                &github_template_api_base(),
            )?,
        })
    }

    fn get(&self, url: reqwest::Url) -> reqwest::RequestBuilder {
        self.client
            .get(url)
            .header(USER_AGENT, GITHUB_TEMPLATE_USER_AGENT)
    }
}

fn github_template_failure(failure: ApiFailure, context: String) -> anyhow::Error {
    if let Some(rate_limit) = failure.rate_limit {
        anyhow::Error::new(rate_limit).context(context)
    } else {
        anyhow!(
            "{context} with status {}: {}",
            failure.status,
            failure.diagnostic
        )
    }
}

fn github_template_api_base() -> String {
    env::var(GITHUB_TEMPLATE_API_BASE_ENV).unwrap_or_else(|_| "https://api.github.com".to_string())
}

fn archive_base_for_install(environment: TemplateEnvironment, api_base: &str) -> String {
    if api_base == "https://api.github.com" {
        (environment.archive_base)()
    } else {
        api_base.to_string()
    }
}

/// Parsed components from a GitHub repository URL.
#[derive(Debug)]
pub struct ParsedGithubRepoUrl {
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
pub fn parse_github_repo_url(url: &str) -> Result<ParsedGithubRepoUrl> {
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
#[cfg_attr(not(test), allow(dead_code))]
async fn fetch_default_branch(owner: &str, repo: &str) -> Result<String> {
    let github = GitHubTemplateClient::new().await?;
    fetch_default_branch_with_client(&github, owner, repo).await
}

async fn fetch_default_branch_with_client(
    github: &GitHubTemplateClient,
    owner: &str,
    repo: &str,
) -> Result<String> {
    let base = github_template_api_base();
    let url = reqwest::Url::parse(&format!("{base}/repos/{owner}/{repo}"))
        .with_context(|| format!("failed to build GitHub repo URL for {owner}/{repo}"))?;
    let response = github
        .get(url)
        .send()
        .await
        .with_context(|| format!("failed to fetch repo info for {owner}/{repo}"))?;
    if !response.status().is_success() {
        return Err(github_template_failure(
            github.client.response_error(response).await,
            format!("GitHub repo info fetch failed for {owner}/{repo}"),
        ));
    }
    response
        .json::<GitHubRepoInfo>()
        .await
        .map(|info| info.default_branch)
        .with_context(|| format!("failed to decode repo info for {owner}/{repo}"))
}

/// Discover all AgentTemplates under a GitHub repository's conventional
/// `agent_templates/` collection.
#[cfg_attr(not(test), allow(dead_code))]
pub async fn discover_github_repo_templates(
    environment: TemplateEnvironment,
    owner: &str,
    repo: &str,
    git_ref: &str,
) -> Result<Vec<AgentTemplateCatalogEntry>> {
    let github = GitHubTemplateClient::new().await?;
    let archive = GitHubArchive::fetch(
        &github.client,
        owner,
        repo,
        git_ref,
        &(environment.archive_base)(),
    )
    .await?;
    discover_github_repo_templates_from_archive(environment, &archive, owner, repo, git_ref)
}

fn discover_github_repo_templates_from_archive(
    environment: TemplateEnvironment,
    archive: &GitHubArchive,
    owner: &str,
    repo: &str,
    git_ref: &str,
) -> Result<Vec<AgentTemplateCatalogEntry>> {
    let root = archive.root().join(DEFAULT_REMOTE_TEMPLATE_DIR);
    if !root.is_dir() {
        return Ok(Vec::new());
    }

    let mut catalog_entries = Vec::new();
    let mut entries = fs::read_dir(&root)
        .with_context(|| format!("failed to read {}", root.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let template_id = entry.file_name().to_string_lossy().to_string();
        let template_dir = entry.path();
        let Some(manifest) = parse_local_template_manifest(environment, &template_dir) else {
            tracing::warn!(target: "holon::agent_template",
                template = %template_id,
                "skipping remote template: invalid or missing template.toml"
            );
            continue;
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
                "https://github.com/{owner}/{repo}/tree/{git_ref}/{DEFAULT_REMOTE_TEMPLATE_DIR}/{template_id}"
            )),
        });
    }
    Ok(catalog_entries)
}

fn resolve_github_template_from_archive(
    environment: TemplateEnvironment,
    archive: &GitHubArchive,
    template_url: &str,
    owner: &str,
    repo: &str,
    git_ref: &str,
    template_path: &str,
) -> Result<ResolvedTemplate> {
    let template_dir = archive
        .directory(template_path)
        .with_context(|| format!("template directory not found at {template_path}"))?;
    let agents_md = fs::read_to_string(template_dir.join(TEMPLATE_AGENTS_FILENAME))
        .with_context(|| format!("failed to read {TEMPLATE_AGENTS_FILENAME}"))?;
    if agents_md.trim().is_empty() {
        bail!("GitHub template {template_url} resolved to an empty AGENTS.md");
    }
    let skill_refs = parse_skill_refs(environment, template_dir.join(TEMPLATE_SKILLS_FILENAME))?;
    let template_toml = read_template_manifest_content(&template_dir);
    let schema_version = template_toml
        .as_deref()
        .and_then(|content| parse_template_manifest(environment, content))
        .map(|m| m.schema);
    Ok(ResolvedTemplate {
        provenance: TemplateProvenanceSource::GitHubUrl {
            url: template_url.to_string(),
            owner: owner.to_string(),
            repo: repo.to_string(),
            git_ref: git_ref.to_string(),
            template_path: template_path.to_string(),
        },
        agents_md,
        template_toml,
        skill_refs,
        schema_version,
    })
}

async fn resolve_github_template_archive_first(
    environment: TemplateEnvironment,
    template: &str,
) -> Result<ResolvedTemplate> {
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
    let owner = segments[0];
    let repo = segments[1];
    let ref_and_path = &segments[3..];
    let github = GitHubTemplateClient::new().await?;
    for split in 1..ref_and_path.len() {
        let git_ref = ref_and_path[..split].join("/");
        let template_path = ref_and_path[split..].join("/");
        let archive = match GitHubArchive::fetch(
            &github.client,
            owner,
            repo,
            &git_ref,
            &(environment.archive_base)(),
        )
        .await
        {
            Ok(archive) => archive,
            Err(error) if github_archive_not_found(&error) => continue,
            Err(error) => return Err(error),
        };
        if let Ok(resolved) = resolve_github_template_from_archive(
            environment,
            &archive,
            template,
            owner,
            repo,
            &git_ref,
            &template_path,
        ) {
            return Ok(resolved);
        }
    }
    bail!("GitHub template URL did not resolve to a readable template directory: {template}")
}

fn github_archive_not_found(error: &anyhow::Error) -> bool {
    holon_github::archive::is_not_found(error)
}

fn install_remote_template_catalog_entry_from_archive(
    environment: TemplateEnvironment,
    archive: &GitHubArchive,
    templates_root: &Path,
    registry: &AgentTemplateRegistry,
    source_id: &str,
    source_url: &str,
    resolved_ref: &str,
    synced_at: DateTime<Utc>,
    entry: &AgentTemplateCatalogEntry,
) -> Result<AgentTemplateRegistryInstalled> {
    let parsed = parse_github_repo_url(source_url)?;
    let template_path = format!("{DEFAULT_REMOTE_TEMPLATE_DIR}/{}", entry.template_id);
    let template_url = entry
        .source_url
        .as_deref()
        .ok_or_else(|| anyhow!("remote template {} has no source URL", entry.catalog_id))?;
    let resolved = resolve_github_template_from_archive(
        environment,
        archive,
        template_url,
        &parsed.owner,
        &parsed.repo,
        resolved_ref,
        &template_path,
    )?;
    let install_id = choose_remote_template_install_id(templates_root, registry, source_id, entry)?;
    let destination = templates_root.join(&install_id);
    let content_hash = resolved_template_content_hash(&resolved);
    let state = ManagedTemplateState {
        template_id: entry.template_id.clone(),
        content_hash: content_hash.clone(),
        owner: ManagedTemplateOwner::RemoteSource {
            source_id: source_id.to_string(),
            url: source_url.to_string(),
            resolved_ref: resolved_ref.to_string(),
            source_url: template_url.to_string(),
            synced_at,
        },
    };
    write_managed_template_dir(environment, &destination, &resolved, &state)
        .with_context(|| format!("failed to install remote template {}", entry.template_id))?;
    Ok(AgentTemplateRegistryInstalled {
        template_id: entry.template_id.clone(),
        source_id: source_id.to_string(),
        source_url: source_url.to_string(),
        source_resolved_ref: resolved_ref.to_string(),
        template_path: remote_template_source_path(template_url),
        local_path: install_id,
        content_hash,
        synced_at,
    })
}

async fn materialize_template(
    environment: TemplateEnvironment,
    agent_home: &Path,
    template: &ResolvedTemplate,
) -> Result<()> {
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
        materialize_skill_ref(environment, agent_home, &skills_root, skill_ref).await?;
    }
    Ok(())
}

pub fn ensure_agent_home_layout(agent_home: &Path) -> Result<()> {
    ensure_private_dir(agent_home)?;
    for dir in [
        agent_home.join("memory"),
        agent_home.join("notes"),
        agent_home.join("work"),
        agent_home.join("tmp"),
        agent_home.join("work-items"),
        agent_home.join("skills"),
        agent_home.join(".holon/state"),
        agent_home.join(".holon/ledger"),
        agent_home.join(".holon/indexes"),
        agent_home.join(".holon/cache"),
    ] {
        ensure_private_dir(&dir)?;
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

/// Creates a private directory, keeping an existing one usable.
///
/// Narrowing the mode of a directory that already exists is best-effort: an
/// agent home on a read-only mount or owned by another uid must still produce
/// a usable layout, matching the other private write sites that only warn.
/// Failing to create a missing directory is still an error.
fn ensure_private_dir(path: &Path) -> Result<()> {
    match holon_domain::private_fs::create_private_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if path.is_dir() => {
            tracing::warn!(
                target: "holon::agent_template",
                path = %path.display(),
                %error,
                "failed to restrict agent home directory permissions; keeping existing mode"
            );
            Ok(())
        }
        Err(error) => Err(error).with_context(|| format!("failed to create {}", path.display())),
    }
}

fn create_file_if_missing(path: &Path, content: &[u8]) -> Result<()> {
    if path.exists() {
        // Never overwrite seeded memory, but keep an existing file private.
        // Permission correction is best-effort for the same reason.
        if let Err(error) = holon_domain::private_fs::set_private_file_mode(path) {
            tracing::warn!(
                target: "holon::agent_template",
                path = %path.display(),
                %error,
                "failed to restrict agent home file permissions; keeping existing mode"
            );
        }
        return Ok(());
    }
    holon_domain::private_fs::write_private_file(path, content)
        .with_context(|| format!("failed to write {}", path.display()))
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

fn render_blank_agent_home_agents_md() -> String {
    render_agent_home_agents_md(BLANK_AGENT_HOME_GUIDANCE, None)
}

async fn materialize_skill_ref(
    environment: TemplateEnvironment,
    agent_home: &Path,
    skills_root: &Path,
    skill_ref: &TemplateSkillRef,
) -> Result<PathBuf> {
    match skill_ref {
        TemplateSkillRef::Local { path } => materialize_local_skill_ref(skills_root, path),
        TemplateSkillRef::Github(github_ref) => {
            let agent_home = agent_home.to_path_buf();
            let github_ref = github_ref.clone();
            tokio::task::spawn_blocking(move || {
                materialize_github_skill_ref(environment, &agent_home, &github_ref)
            })
            .await
            .context("GitHub skill materialization task failed")?
        }
    }
}

/// Materialize every template skill ref, reporting each destination to
/// `on_created` in ref order as soon as it lands.
///
/// GitHub refs are installed into the user skill library in one batched pass
/// first, so skills sharing a repository download (and cache) its archive
/// once instead of once per skill.
/// Partial creations stay visible to the caller even when a later ref fails,
/// so rollback and staging journals observe the same set.
async fn materialize_template_skill_refs(
    environment: TemplateEnvironment,
    agent_home: &Path,
    skill_refs: &[TemplateSkillRef],
    mut on_created: impl FnMut(PathBuf) -> Result<()>,
) -> Result<()> {
    let mut github_specs = Vec::new();
    for skill_ref in skill_refs {
        if let TemplateSkillRef::Github(github_ref) = skill_ref {
            let (package, skill) = template_github_skill_package(github_ref)?;
            github_specs.push(crate::skills::GithubSkillInstallRequest { package, skill });
        }
    }
    let installed_names = if github_specs.is_empty() {
        Vec::new()
    } else {
        tokio::task::spawn_blocking(move || {
            let user_home = user_home_dir()?;
            crate::skills::add_github_remote_skills_to_library(
                environment.io,
                &user_home,
                &github_specs,
            )
        })
        .await
        .context("GitHub skill materialization task failed")??
    };
    let mut github_installed = installed_names.into_iter();
    let local_skills_root = agent_home.join("skills");
    for skill_ref in skill_refs {
        let destination = match skill_ref {
            TemplateSkillRef::Local { path } => {
                materialize_local_skill_ref(&local_skills_root, path)?
            }
            TemplateSkillRef::Github(_) => {
                let skill_name = github_installed
                    .next()
                    .ok_or_else(|| anyhow!("github skill install result missing"))?;
                materialize_github_library_skill(environment, agent_home, &skill_name)?
            }
        };
        on_created(destination)?;
    }
    Ok(())
}

/// Link an already-installed user-library skill into the agent home and
/// return its agent-side destination.
fn materialize_github_library_skill(
    environment: TemplateEnvironment,
    agent_home: &Path,
    skill_name: &str,
) -> Result<PathBuf> {
    let user_home = user_home_dir()?;
    crate::skills::install_skill_with_user_home(
        environment.io,
        agent_home,
        Some(user_home.as_path()),
        &SkillInstallKind::Named {
            name: skill_name.to_string(),
            mode: SkillInstallMode::Linked,
        },
    )?;
    Ok(crate::skills::agent_skills_root(agent_home).join(skill_name))
}

fn materialize_github_skill_ref(
    environment: TemplateEnvironment,
    agent_home: &Path,
    github_ref: &TemplateGithubSkillRef,
) -> Result<PathBuf> {
    let install_kind = template_github_skill_install_kind(github_ref)?;
    let user_home = user_home_dir()?;
    let skill_name = crate::skills::install_skill_with_user_home(
        environment.io,
        agent_home,
        Some(user_home.as_path()),
        &install_kind,
    )?;
    Ok(agent_home.join("skills").join(skill_name))
}

fn builtin_skill(environment: TemplateEnvironment, name: &str) -> Option<&'static BuiltinSkill> {
    environment
        .builtin_skills
        .iter()
        .find(|skill| skill.name == name)
}

pub fn builtin_skill_names(environment: TemplateEnvironment) -> Vec<&'static str> {
    environment
        .builtin_skills
        .iter()
        .map(|skill| skill.name)
        .collect()
}

pub fn materialize_builtin_skill_ref(
    environment: TemplateEnvironment,
    skills_root: &Path,
    name: &str,
) -> Result<PathBuf> {
    let skill = builtin_skill(environment, name)
        .ok_or_else(|| anyhow!("unknown builtin skill ref: {name}"))?;
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
pub fn parse_template_manifest_for_api(
    environment: TemplateEnvironment,
    template_dir: &Path,
) -> Option<serde_json::Value> {
    parse_local_template_manifest(environment, template_dir).map(|manifest| {
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
    tracing::info!(target: "holon::agent_template", template_id, "removed user global template");
    Ok(())
}

/// Install a template package from a GitHub tree URL into the user global
/// library.
///
/// Expected URL: `https://github.com/<owner>/<repo>/tree/<ref>[/<path>]`
pub fn install_template_from_github(
    environment: TemplateEnvironment,
    user_home: &Path,
    github_url: &str,
) -> Result<String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .user_agent(format!("holon/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .context("failed to create HTTP client for template install")?;
    let client = BlockingClient::new(
        client,
        GitHubAuth::resolve_blocking(None, &["GITHUB_TOKEN", "GH_TOKEN"]),
        "https://api.github.com",
    )?;
    install_template_from_github_with_client(
        environment,
        user_home,
        github_url,
        &client,
        "https://api.github.com",
    )
}

fn install_template_from_github_with_client(
    environment: TemplateEnvironment,
    user_home: &Path,
    github_url: &str,
    client: &BlockingClient,
    api_base: &str,
) -> Result<String> {
    let source = ParsedGithubTemplateUrl::parse(github_url)?;
    let templates_root = templates_root_for_home(user_home);
    fs::create_dir_all(&templates_root)
        .with_context(|| format!("failed to create {}", templates_root.display()))?;

    let segments: Vec<&str> = source.all_after_tree.split('/').collect();
    let mut selected_archive = None;
    let mut search_dir = None;
    for i in (1..=segments.len()).rev() {
        let candidate_ref = segments[..i].join("/");
        let candidate_path = if i < segments.len() {
            segments[i..].join("/")
        } else {
            String::new()
        };
        let archive = match GitHubArchive::fetch_blocking(
            client,
            &source.owner,
            &source.repo,
            &candidate_ref,
            &archive_base_for_install(environment, api_base),
        ) {
            Ok(archive) => archive,
            Err(error) if github_archive_not_found(&error) => continue,
            Err(error) => return Err(error),
        };
        if let Ok(path) = archive.directory(&candidate_path) {
            selected_archive = Some(archive);
            search_dir = Some(path);
            break;
        }
    }
    let _selected_archive = selected_archive.ok_or_else(|| {
        anyhow!(
            "GitHub archive download failed: no valid ref/path found for '{}'",
            source.all_after_tree
        )
    })?;
    let search_dir = search_dir.ok_or_else(|| {
        anyhow!(
            "GitHub archive download failed: no valid ref/path found for '{}'",
            source.all_after_tree
        )
    })?;

    if !search_dir.join(TEMPLATE_MANIFEST_FILENAME).exists()
        && !search_dir.join("AGENTS.md").exists()
    {
        bail!(
            "the specified path does not contain template.toml or AGENTS.md; \
             not a valid template directory"
        );
    }

    let template_id = parse_local_template_manifest(environment, &search_dir)
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
        if let Some(state) = read_managed_template_state(&destination)? {
            bail!(
                "template '{}' at {} is managed by {:?}; refusing to overwrite it with an explicit install",
                state.template_id,
                destination.display(),
                state.owner
            );
        }
        bail!(
            "template '{}' already exists at {}; refusing to overwrite existing user-owned template",
            template_id,
            destination.display()
        );
    }
    copy_template_dir(&search_dir, &destination)?;

    tracing::info!(target: "holon::agent_template", template_id = %template_id, "installed user global template from GitHub");
    Ok(template_id)
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

#[allow(dead_code)]
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
        parsed.owner, parsed.repo, parsed.all_after_tree,
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

#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod test_support;

#[cfg(test)]
mod tests;
