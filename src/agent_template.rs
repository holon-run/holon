use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

const TEMPLATE_AGENTS_FILENAME: &str = "AGENTS.md";
const TEMPLATE_SKILLS_FILENAME: &str = "skills.json";
const TEMPLATE_PROVENANCE_FILENAME: &str = "template-provenance.json";
const BUILTIN_TEMPLATE_STATE_FILENAME: &str = ".holon-builtin-template.json";
pub const DEFAULT_AGENT_TEMPLATE_ID: &str = "holon-default";
const MEMORY_SELF_INITIAL: &str = "# Self Memory\n\n";
const MEMORY_OPERATOR_INITIAL: &str = "# Operator Memory\n\n";

pub const REQUIRED_AGENT_HOME_GUIDANCE: &str = r#"## Holon Agent Home

- `agent_home` is this agent's default workspace. Use it for agent-local state, notes, memory, and non-project-local work.
- `AGENTS.md` is automatically loaded as concise agent guidance. Keep durable behavior here, not transient plans or copied project docs.
- `memory/self.md` and `memory/operator.md` are curated agent-scoped Markdown memory. They are searched or retrieved on demand and are not the same as always-loaded guidance.
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
        skills_json: None,
    },
    BuiltinTemplate {
        template_id: "holon-developer",
        version: 1,
        agents_md: include_str!("../builtin_templates/holon-developer/AGENTS.md"),
        skills_json: None,
    },
    BuiltinTemplate {
        template_id: "holon-reviewer",
        version: 1,
        agents_md: include_str!("../builtin_templates/holon-reviewer/AGENTS.md"),
        skills_json: None,
    },
    BuiltinTemplate {
        template_id: "holon-release",
        version: 1,
        agents_md: include_str!("../builtin_templates/holon-release/AGENTS.md"),
        skills_json: None,
    },
    BuiltinTemplate {
        template_id: "holon-github-solve",
        version: 1,
        agents_md: include_str!("../builtin_templates/holon-github-solve/AGENTS.md"),
        skills_json: Some(include_str!(
            "../builtin_templates/holon-github-solve/skills.json"
        )),
    },
];

struct BuiltinTemplate {
    template_id: &'static str,
    version: u32,
    agents_md: &'static str,
    skills_json: Option<&'static str>,
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TemplateSkillsManifest {
    #[serde(default)]
    skill_refs: Vec<TemplateSkillRef>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum TemplateSkillRef {
    Local { path: PathBuf },
    Github { package: String },
    Builtin { name: String },
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
        let resolved = resolve_template(template, home_dir).await?;
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

pub async fn ensure_agent_home_agents_md_from_template_with_home(
    agent_home: &Path,
    home_dir: &Path,
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
    let resolved = resolve_template(template, home_dir).await?;
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
    home_dir.join(".agents").join("templates")
}

fn user_home_dir() -> Result<PathBuf> {
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
        .ok_or_else(|| anyhow!("HOME is not set; cannot resolve ~/.agents/templates"))
}

async fn resolve_template(template: &str, home_dir: &Path) -> Result<ResolvedTemplate> {
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

    validate_template_id(template)?;
    let path = templates_root_for_home(home_dir).join(template);
    resolve_local_template(
        path.clone(),
        TemplateProvenanceSource::TemplateId {
            template_id: template.to_string(),
            path,
        },
    )
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
    let manifest: TemplateSkillsManifest = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    for skill_ref in &manifest.skill_refs {
        match skill_ref {
            TemplateSkillRef::Github { package } => {
                bail!(
                    "github skill refs are not supported in phase 1; use local or builtin skill refs instead: {package}"
                );
            }
            TemplateSkillRef::Builtin { name } => {
                if builtin_skill(name).is_none() {
                    bail!("unknown builtin skill ref: {name}");
                }
            }
            TemplateSkillRef::Local { .. } => {}
        }
    }
    Ok(manifest.skill_refs)
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
    if let Some(skills_json) = template.skills_json {
        hasher.update(b"\n");
        hasher.update(TEMPLATE_SKILLS_FILENAME.as_bytes());
        hasher.update(b"\n");
        hasher.update(skills_json.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn current_builtin_template_content_hash(
    template_dir: &Path,
    expect_skills_json: bool,
) -> Result<String> {
    use sha2::{Digest as _, Sha256};

    let agents_md_path = template_dir.join(TEMPLATE_AGENTS_FILENAME);
    let agents_md = fs::read_to_string(&agents_md_path)
        .with_context(|| format!("failed to read {}", agents_md_path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(TEMPLATE_AGENTS_FILENAME.as_bytes());
    hasher.update(b"\n");
    hasher.update(agents_md.as_bytes());
    let skills_path = template_dir.join(TEMPLATE_SKILLS_FILENAME);
    if expect_skills_json {
        let skills_json = fs::read_to_string(&skills_path)
            .with_context(|| format!("failed to read {}", skills_path.display()))?;
        hasher.update(b"\n");
        hasher.update(TEMPLATE_SKILLS_FILENAME.as_bytes());
        hasher.update(b"\n");
        hasher.update(skills_json.as_bytes());
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn builtin_template_is_managed(template_dir: &Path, state: &BuiltinTemplateState) -> Result<bool> {
    let expect_skills_json = template_dir.join(TEMPLATE_SKILLS_FILENAME).exists();
    let current_hash = current_builtin_template_content_hash(template_dir, expect_skills_json)?;
    Ok(current_hash == state.content_hash)
}

fn write_builtin_template(template_dir: &Path, builtin: &BuiltinTemplate) -> Result<()> {
    fs::create_dir_all(template_dir)
        .with_context(|| format!("failed to create {}", template_dir.display()))?;
    let agents_md_path = template_dir.join(TEMPLATE_AGENTS_FILENAME);
    write_file_atomically(&agents_md_path, builtin.agents_md.as_bytes())?;
    let skills_path = template_dir.join(TEMPLATE_SKILLS_FILENAME);
    match builtin.skills_json {
        Some(content) => write_file_atomically(&skills_path, content.as_bytes())?,
        None if skills_path.exists() => fs::remove_file(&skills_path)
            .with_context(|| format!("failed to remove {}", skills_path.display()))?,
        None => {}
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
        let skills_json = fetch_github_file(&owner, &repo, &git_ref, &skills_path).await?;
        let skill_refs = match skills_json {
            Some(content) => {
                let manifest: TemplateSkillsManifest = serde_json::from_str(&content)
                    .with_context(|| {
                        format!("failed to parse {template}::{TEMPLATE_SKILLS_FILENAME}")
                    })?;
                manifest.skill_refs
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
    _agent_home: &Path,
    skills_root: &Path,
    skill_ref: &TemplateSkillRef,
) -> Result<PathBuf> {
    match skill_ref {
        TemplateSkillRef::Local { path } => materialize_local_skill_ref(skills_root, path),
        TemplateSkillRef::Builtin { name } => materialize_builtin_skill_ref(skills_root, name),
        TemplateSkillRef::Github { package } => bail!(
            "github skill refs are not supported in phase 1; use local or builtin skill refs instead: {package}"
        ),
    }
}

fn builtin_skill(name: &str) -> Option<&'static BuiltinSkill> {
    BUILTIN_SKILLS.iter().find(|skill| skill.name == name)
}

fn materialize_builtin_skill_ref(skills_root: &Path, name: &str) -> Result<PathBuf> {
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

fn materialize_local_skill_ref(skills_root: &Path, path: &Path) -> Result<PathBuf> {
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

fn remove_materialized_skill_destination(destination: &Path) -> std::io::Result<()> {
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
        sync::{Arc, LazyLock, Mutex},
        thread,
    };

    use tempfile::tempdir;

    use super::*;

    static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

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
        let _lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempdir().unwrap();
        let _guard = EnvGuard::set("HOME", home.path().display().to_string());

        seed_builtin_templates().unwrap();
        let developer_agents = templates_root().unwrap().join("holon-developer/AGENTS.md");
        assert!(developer_agents.is_file());

        fs::write(&developer_agents, "custom").unwrap();
        seed_builtin_templates().unwrap();
        assert_eq!(fs::read_to_string(&developer_agents).unwrap(), "custom");
    }

    #[test]
    fn seed_builtin_templates_writes_prefixed_default_template() {
        let _lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempdir().unwrap();
        let _guard = EnvGuard::set("HOME", home.path().display().to_string());

        seed_builtin_templates().unwrap();
        let agents_md = templates_root().unwrap().join("holon-default/AGENTS.md");
        assert!(agents_md.is_file());
        assert!(fs::read_to_string(agents_md)
            .unwrap()
            .contains("Holon Default Agent"));
    }

    #[test]
    fn seed_builtin_templates_upgrades_managed_builtin_content() {
        let _lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempdir().unwrap();
        let _guard = EnvGuard::set("HOME", home.path().display().to_string());

        seed_builtin_templates().unwrap();
        let template_dir = templates_root().unwrap().join("holon-reviewer");
        let agents_md = template_dir.join("AGENTS.md");
        let state_path = builtin_template_state_path(&template_dir);
        let original = fs::read_to_string(&agents_md).unwrap();
        let mut state: BuiltinTemplateState =
            serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
        state.version = 0;
        fs::write(&state_path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();

        seed_builtin_templates().unwrap();

        assert_eq!(fs::read_to_string(&agents_md).unwrap(), original);
        let upgraded: BuiltinTemplateState =
            serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
        assert_eq!(upgraded.version, 1);
    }

    #[test]
    fn seed_builtin_templates_tolerates_empty_builtin_state_file() {
        let _lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempdir().unwrap();
        let _guard = EnvGuard::set("HOME", home.path().display().to_string());

        seed_builtin_templates().unwrap();
        let template_dir = templates_root().unwrap().join("holon-default");
        fs::write(builtin_template_state_path(&template_dir), "").unwrap();

        seed_builtin_templates().unwrap();

        assert!(template_dir.join("AGENTS.md").is_file());
    }

    #[test]
    fn builtin_template_is_managed_accounts_for_skills_json() {
        let home = tempdir().unwrap();
        let template_dir = home.path().join("template");
        fs::create_dir_all(&template_dir).unwrap();
        fs::write(template_dir.join(TEMPLATE_AGENTS_FILENAME), "agents").unwrap();
        fs::write(
            template_dir.join(TEMPLATE_SKILLS_FILENAME),
            r#"{"skill_refs":[]}"#,
        )
        .unwrap();

        let builtin = BuiltinTemplate {
            template_id: "holon-test",
            version: 1,
            agents_md: "agents",
            skills_json: Some(r#"{"skill_refs":[]}"#),
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
        let _lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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
        let _lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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
            template_dir.join("skills.json"),
            serde_json::json!({
                "skill_refs": [
                    { "kind": "local", "path": source_skill }
                ]
            })
            .to_string(),
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
    async fn template_without_directory_guidance_still_gets_required_agent_home_guidance() {
        let home = tempdir().unwrap();
        let template_dir = home.path().join("template");
        fs::create_dir_all(&template_dir).unwrap();
        fs::write(template_dir.join("AGENTS.md"), "role only").unwrap();

        let agent_home = home.path().join("agent");
        initialize_agent_home_from_template(&agent_home, template_dir.to_str().unwrap())
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
        let _lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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
        let _lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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
            template_dir.join("skills.json"),
            serde_json::json!({
                "skill_refs": [
                    {"kind": "local", "path": valid_skill},
                    {"kind": "local", "path": "relative/path"}
                ]
            })
            .to_string(),
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
        let _lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempdir().unwrap();
        let _guard = EnvGuard::set("HOME", home.path().display().to_string());
        let templates = templates_root().unwrap();
        fs::create_dir_all(&templates).unwrap();
        let template_dir = templates.join("broken");
        fs::create_dir_all(&template_dir).unwrap();
        fs::write(template_dir.join("AGENTS.md"), "broken").unwrap();
        fs::write(
            template_dir.join("skills.json"),
            r#"{"skill_refs":[{"kind":"local","path":"relative/path"}]}"#,
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
        let _lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempdir().unwrap();
        let _guard = EnvGuard::set("HOME", home.path().display().to_string());
        let templates = templates_root().unwrap();
        fs::create_dir_all(&templates).unwrap();
        let template_dir = templates.join("broken");
        fs::create_dir_all(&template_dir).unwrap();
        fs::write(template_dir.join("AGENTS.md"), "broken").unwrap();
        fs::write(
            template_dir.join("skills.json"),
            r#"{"skill_refs":[{"kind":"local","path":"relative/path"}]}"#,
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
    fn parse_skill_refs_rejects_github_skill_refs_in_phase_one() {
        let home = tempdir().unwrap();
        let manifest_path = home.path().join("skills.json");
        fs::write(
            &manifest_path,
            r#"{"skill_refs":[{"kind":"github","package":"owner/repo@skill"}]}"#,
        )
        .unwrap();

        let err = parse_skill_refs(manifest_path).unwrap_err();
        assert!(err
            .to_string()
            .contains("github skill refs are not supported in phase 1"));
    }

    #[test]
    fn user_home_dir_falls_back_to_userprofile() {
        let _lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let profile = tempdir().unwrap();
        let _home = EnvGuard::set("HOME", String::new());
        let _userprofile = EnvGuard::set("USERPROFILE", profile.path().display().to_string());

        assert_eq!(user_home_dir().unwrap(), profile.path());
    }

    #[tokio::test]
    async fn initialize_agent_home_from_github_url_works() {
        let _lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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
                "/repos/owner/repo/contents/templates/reviewer/skills.json?ref=main",
            ]
        );
    }
}
