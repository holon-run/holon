use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{bail, Context, Result};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tracing::warn;

use crate::types::{
    ActiveSkillRecord, SkillCatalogEntry, SkillInstallMode, SkillRootRegistration,
    SkillRootScanStatus, SkillRootSourceKind, SkillRootView, SkillRootWatchStatus, SkillScope,
    SkillsRuntimeView,
};

const SKILL_ENTRYPOINT: &str = "SKILL.md";
const INSTALL_METADATA_FILENAME: &str = ".holon-skill-install.json";
pub const REMOTE_SKILL_INSTALL_TIMEOUT_SECONDS: u64 = 120;
pub(crate) const SKILL_ROOT_SUFFIXES: [&str; 4] = [
    "skills",
    ".agents/skills",
    ".codex/skills",
    ".claude/skills",
];
pub(crate) const COMPAT_SKILL_ROOT_SUFFIXES: [&str; 3] =
    [".agents/skills", ".codex/skills", ".claude/skills"];
const SKILL_LOCK_FILENAME: &str = ".agents/.skill-lock.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillVisibility {
    DefaultAgent,
    NonDefaultAgent,
}

pub fn load_skills_runtime_view(
    visibility: SkillVisibility,
    user_home: Option<&Path>,
    agent_home: &Path,
    workspace_anchor: Option<&Path>,
    active_skills: &[ActiveSkillRecord],
) -> Result<SkillsRuntimeView> {
    let mut discovered_roots = Vec::new();
    let mut discoverable_skills = Vec::new();

    if matches!(visibility, SkillVisibility::DefaultAgent) {
        if let Some(root) = select_skill_root(user_home, &COMPAT_SKILL_ROOT_SUFFIXES) {
            discoverable_skills.extend(load_catalog_for_scope(SkillScope::UserGlobal, &root)?);
            discovered_roots.push(SkillRootView {
                scope: SkillScope::UserGlobal,
                path: root,
            });
        }
    }

    for root in existing_skill_roots(Some(agent_home), &SKILL_ROOT_SUFFIXES) {
        discoverable_skills.extend(load_catalog_for_scope(SkillScope::Agent, &root)?);
        discovered_roots.push(SkillRootView {
            scope: SkillScope::Agent,
            path: root,
        });
    }

    if let Some(root) = select_skill_root(workspace_anchor, &COMPAT_SKILL_ROOT_SUFFIXES) {
        discoverable_skills.extend(load_catalog_for_scope(SkillScope::Workspace, &root)?);
        discovered_roots.push(SkillRootView {
            scope: SkillScope::Workspace,
            path: root,
        });
    }

    discoverable_skills = retain_highest_precedence_skills(discoverable_skills);
    discoverable_skills.sort_by(|left, right| left.skill_id.cmp(&right.skill_id));
    let attached_skills = discoverable_skills.clone();
    let active_skills = retain_active_skills_for_catalog(&discoverable_skills, active_skills);

    Ok(SkillsRuntimeView {
        agent_templates_catalog: Vec::new(),
        discovered_roots,
        discoverable_skills,
        attached_skills,
        active_skills,
    })
}

pub fn find_skill_by_entrypoint<'a>(
    skills: &'a [SkillCatalogEntry],
    path: &Path,
) -> Option<&'a SkillCatalogEntry> {
    skills.iter().find(|entry| entry.path == path)
}

pub fn find_skill_by_script_path<'a>(
    skills: &'a [SkillCatalogEntry],
    path: &Path,
) -> Option<&'a SkillCatalogEntry> {
    skills.iter().find(|entry| {
        entry
            .path
            .parent()
            .map(|skill_root| path.starts_with(skill_root.join("scripts")))
            .unwrap_or(false)
    })
}

fn select_skill_root(base: Option<&Path>, suffixes: &[&str]) -> Option<PathBuf> {
    let base = base?;
    for suffix in suffixes {
        let candidate = base.join(suffix);
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    None
}

pub(crate) fn existing_skill_roots(base: Option<&Path>, suffixes: &[&str]) -> Vec<PathBuf> {
    let Some(base) = base else {
        return Vec::new();
    };
    suffixes
        .iter()
        .map(|suffix| base.join(suffix))
        .filter(|candidate| candidate.is_dir())
        .collect()
}

pub fn load_catalog_for_scope(scope: SkillScope, root: &Path) -> Result<Vec<SkillCatalogEntry>> {
    load_catalog_for_root(scope, root, skill_root_id_for_scope(scope, root))
}

pub(crate) fn load_catalog_for_root(
    scope: SkillScope,
    root: &Path,
    root_id: String,
) -> Result<Vec<SkillCatalogEntry>> {
    let mut entries = Vec::new();
    let read_dir = match fs::read_dir(root) {
        Ok(read_dir) => read_dir,
        Err(error) => {
            warn!("skipping unreadable skill root {}: {error}", root.display());
            return Ok(entries);
        }
    };
    for child in read_dir {
        let child = match child {
            Ok(child) => child,
            Err(error) => {
                warn!(
                    "skipping unreadable skill entry in {}: {error}",
                    root.display()
                );
                continue;
            }
        };
        let file_type = match child.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                warn!(
                    "skipping skill entry with unreadable type {}: {error}",
                    child.path().display()
                );
                continue;
            }
        };
        // Accept both regular directories and symlinks-to-directories as skill entries.
        if !file_type.is_dir() && !file_type.is_symlink() {
            continue;
        }
        let skill_name = child.file_name().to_string_lossy().to_string();
        let skill_path = child.path().join(SKILL_ENTRYPOINT);
        if !skill_path.is_file() {
            continue;
        }
        let content = match fs::read_to_string(&skill_path) {
            Ok(content) => content,
            Err(error) => {
                warn!(
                    "skipping unreadable skill file {}: {error}",
                    skill_path.display()
                );
                continue;
            }
        };
        let parsed = parse_skill_metadata(&content);
        let name = parsed.name.unwrap_or_else(|| skill_name.clone());
        let description = parsed
            .description
            .unwrap_or_else(|| first_body_paragraph(&content));
        entries.push(SkillCatalogEntry {
            skill_id: format!("{root_id}:{skill_name}"),
            root_id: root_id.clone(),
            skill_dir: skill_name,
            name,
            description,
            path: skill_path,
            scope,
        });
    }
    Ok(entries)
}

pub fn effective_skill_root_registrations(
    visibility: SkillVisibility,
    user_home: Option<&Path>,
    agent_id: &str,
    agent_home: &Path,
    workspace_anchor: Option<&Path>,
) -> Vec<SkillRootRegistration> {
    let mut roots = Vec::new();
    if matches!(visibility, SkillVisibility::DefaultAgent) {
        roots.extend(
            existing_skill_roots(user_home, &COMPAT_SKILL_ROOT_SUFFIXES)
                .into_iter()
                .map(|root_path| {
                    skill_root_registration(SkillRootSourceKind::UserGlobal, None, root_path)
                }),
        );
    }
    roots.extend(
        existing_skill_roots(Some(agent_home), &SKILL_ROOT_SUFFIXES)
            .into_iter()
            .map(|root_path| {
                skill_root_registration(
                    SkillRootSourceKind::AgentHome,
                    Some(agent_id.to_string()),
                    root_path,
                )
            }),
    );
    roots.extend(
        existing_skill_roots(workspace_anchor, &COMPAT_SKILL_ROOT_SUFFIXES)
            .into_iter()
            .map(|root_path| {
                skill_root_registration(SkillRootSourceKind::Workspace, None, root_path)
            }),
    );
    roots
}

pub fn skills_runtime_view_from_catalog(
    mut catalog: Vec<SkillCatalogEntry>,
    roots: &[SkillRootRegistration],
    active_skills: &[ActiveSkillRecord],
) -> SkillsRuntimeView {
    catalog.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.skill_id.cmp(&right.skill_id))
            .then_with(|| left.path.cmp(&right.path))
    });
    let active_skills = retain_active_skills_for_catalog(&catalog, active_skills);
    SkillsRuntimeView {
        agent_templates_catalog: Vec::new(),
        discovered_roots: collect_discovered_roots_from_registrations(roots),
        discoverable_skills: catalog.clone(),
        attached_skills: catalog,
        active_skills,
    }
}

fn collect_discovered_roots_from_registrations(
    registrations: &[SkillRootRegistration],
) -> Vec<SkillRootView> {
    let mut roots = registrations
        .iter()
        .filter(|registration| registration.root_path.is_dir())
        .map(|registration| SkillRootView {
            scope: match registration.source_kind {
                SkillRootSourceKind::UserGlobal => SkillScope::UserGlobal,
                SkillRootSourceKind::AgentHome => SkillScope::Agent,
                SkillRootSourceKind::Workspace => SkillScope::Workspace,
            },
            path: registration.root_path.clone(),
        })
        .collect::<Vec<_>>();
    roots.sort_by(|left, right| {
        skill_precedence(left.scope)
            .cmp(&skill_precedence(right.scope))
            .then_with(|| left.path.cmp(&right.path))
    });
    roots.dedup_by(|left, right| left.scope == right.scope && left.path == right.path);
    roots
}

pub fn skill_root_registration(
    source_kind: SkillRootSourceKind,
    owner_agent_id: Option<String>,
    root_path: PathBuf,
) -> SkillRootRegistration {
    SkillRootRegistration {
        source_kind,
        owner_agent_id,
        root_path,
        scan_status: SkillRootScanStatus::NeverScanned,
        watch_status: SkillRootWatchStatus::NotWatched,
    }
}

pub(crate) fn skill_root_id(registration: &SkillRootRegistration) -> String {
    let source = match registration.source_kind {
        SkillRootSourceKind::UserGlobal => "user_global",
        SkillRootSourceKind::AgentHome => "agent_home",
        SkillRootSourceKind::Workspace => "workspace",
    };
    let hash = short_root_hash(&registration.root_path);
    match (
        registration.source_kind,
        registration.owner_agent_id.as_deref(),
    ) {
        (SkillRootSourceKind::AgentHome, Some(owner)) => format!("{source}:{owner}:{hash}"),
        _ => format!("{source}:{hash}"),
    }
}

pub(crate) fn skill_root_id_for_scope(scope: SkillScope, root: &Path) -> String {
    let source = match scope {
        SkillScope::UserGlobal => "user_global",
        SkillScope::Agent => "agent_home",
        SkillScope::Workspace => "workspace",
    };
    format!("{source}:{}", short_root_hash(root))
}

fn short_root_hash(root: &Path) -> String {
    let normalized = root.components().collect::<PathBuf>();
    let digest = Sha256::digest(normalized.to_string_lossy().as_bytes());
    format!("{digest:x}")[..12].to_string()
}

fn retain_highest_precedence_skills(skills: Vec<SkillCatalogEntry>) -> Vec<SkillCatalogEntry> {
    let mut selected_by_name: std::collections::BTreeMap<String, SkillCatalogEntry> =
        std::collections::BTreeMap::new();
    for skill in skills {
        match selected_by_name.get(&skill.name) {
            Some(existing) if !skill_wins_catalog_selection(&skill, existing) => {}
            _ => {
                selected_by_name.insert(skill.name.clone(), skill);
            }
        }
    }
    selected_by_name.into_values().collect()
}

fn retain_active_skills_for_catalog(
    catalog: &[SkillCatalogEntry],
    active_skills: &[ActiveSkillRecord],
) -> Vec<ActiveSkillRecord> {
    active_skills
        .iter()
        .filter_map(|record| {
            catalog
                .iter()
                .find(|entry| entry.skill_id == record.skill_id)
                .map(|entry| {
                    let mut record = record.clone();
                    record.skill_id = entry.skill_id.clone();
                    record.name = entry.name.clone();
                    record.path = entry.path.clone();
                    record.scope = entry.scope;
                    record
                })
        })
        .collect()
}

fn skill_wins_catalog_selection(
    candidate: &SkillCatalogEntry,
    existing: &SkillCatalogEntry,
) -> bool {
    let candidate_precedence = skill_precedence(candidate.scope);
    let existing_precedence = skill_precedence(existing.scope);
    candidate_precedence > existing_precedence
        || (candidate_precedence == existing_precedence
            && (&candidate.skill_id, &candidate.path) < (&existing.skill_id, &existing.path))
}

fn skill_precedence(scope: SkillScope) -> u8 {
    match scope {
        SkillScope::Agent => 3,
        SkillScope::Workspace => 2,
        SkillScope::UserGlobal => 1,
    }
}

#[derive(Default)]
struct ParsedSkillMetadata {
    name: Option<String>,
    description: Option<String>,
}

fn parse_skill_metadata(content: &str) -> ParsedSkillMetadata {
    let mut parsed = ParsedSkillMetadata::default();
    let mut lines = content.lines();
    if lines.next() != Some("---") {
        return parsed;
    }

    for line in lines {
        if line == "---" {
            break;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .to_string();
        match key.trim() {
            "name" if !value.is_empty() => parsed.name = Some(value),
            "description" if !value.is_empty() => parsed.description = Some(value),
            _ => {}
        }
    }
    parsed
}

fn first_body_paragraph(content: &str) -> String {
    let body = if content.starts_with("---\n") || content.starts_with("---\r\n") {
        content
            .split_once("\n---\n")
            .map(|(_, body)| body)
            .or_else(|| content.split_once("\r\n---\r\n").map(|(_, body)| body))
            .unwrap_or(content)
    } else {
        content
    };
    body.split("\n\n")
        .map(str::trim)
        .find(|paragraph| !paragraph.is_empty())
        .unwrap_or_default()
        .replace('\n', " ")
}

const SKILL_ROOT_SUFFIX_AGENT: &str = "skills";
const SKILL_ROOT_SUFFIX_USER_LIBRARY: &str = ".agents/skills";

fn agent_skills_root(agent_home: &Path) -> PathBuf {
    // Prefer an existing skill root if one already exists (e.g. .agents/skills, .codex/skills).
    // This avoids creating a new `skills/` root that would shadow legacy roots.
    if let Some(existing) = select_skill_root(Some(agent_home), &SKILL_ROOT_SUFFIXES) {
        existing
    } else {
        agent_home.join(SKILL_ROOT_SUFFIX_AGENT)
    }
}

fn user_library_skills_root(user_home: &Path) -> PathBuf {
    if let Some(existing) = select_skill_root(Some(user_home), &COMPAT_SKILL_ROOT_SUFFIXES) {
        existing
    } else {
        user_home.join(SKILL_ROOT_SUFFIX_USER_LIBRARY)
    }
}

#[derive(Debug, Clone)]
pub struct SkillInstallConflict {
    pub skill_name: String,
    pub destination: PathBuf,
}

impl std::fmt::Display for SkillInstallConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "skill '{}' is already installed at {}; uninstall it first or choose a different skill name",
            self.skill_name,
            self.destination.display()
        )
    }
}

impl std::error::Error for SkillInstallConflict {}

#[derive(Debug, Clone)]
pub struct RemoteSkillInstallFailed {
    pub package: String,
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl std::fmt::Display for RemoteSkillInstallFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            Some(status) => write!(
                f,
                "remote skill install for '{}' failed with exit status {}",
                self.package, status
            ),
            None => write!(
                f,
                "remote skill install for '{}' failed before process exit",
                self.package
            ),
        }
    }
}

impl std::error::Error for RemoteSkillInstallFailed {}

#[derive(Debug, Clone)]
pub struct RemoteSkillInstallTimedOut {
    pub package: String,
    pub timeout_seconds: u64,
}

impl std::fmt::Display for RemoteSkillInstallTimedOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "remote skill install for '{}' timed out after {} seconds",
            self.package, self.timeout_seconds
        )
    }
}

impl std::error::Error for RemoteSkillInstallTimedOut {}

trait RemoteSkillInstaller {
    fn add_global(
        &self,
        user_home: &Path,
        package: &str,
        skill: Option<&str>,
    ) -> Result<Vec<String>>;
}

struct RustRemoteSkillInstaller;

impl RemoteSkillInstaller for RustRemoteSkillInstaller {
    fn add_global(
        &self,
        user_home: &Path,
        package: &str,
        skill: Option<&str>,
    ) -> Result<Vec<String>> {
        if skill.is_none() && remote_package_ref_installs_all_skills(package) {
            let source = RemoteSkillSetSource::parse(package)?;
            return install_github_remote_skill_set(user_home, package, &source);
        }
        let source = RemoteSkillSource::parse(package, skill)?;
        install_github_remote_skill(user_home, package, &source)?;
        Ok(vec![source.skill_name])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RemoteSkillSetSource {
    owner: String,
    repo: String,
    reference: String,
    path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RemoteSkillSource {
    owner: String,
    repo: String,
    reference: String,
    path: String,
    skill_name: String,
}

impl RemoteSkillSetSource {
    fn parse(package: &str) -> Result<Self> {
        let trimmed = package.trim_end_matches('/');
        if let Some(url) = trimmed.strip_prefix("https://github.com/") {
            return Self::parse_github_path(url);
        }
        if let Some(url) = trimmed.strip_prefix("http://github.com/") {
            return Self::parse_github_path(url);
        }
        let mut parts = trimmed.split('/');
        let owner = parts.next().unwrap_or_default();
        let repo = parts.next().unwrap_or_default().trim_end_matches(".git");
        if owner.is_empty() || repo.is_empty() || parts.next().is_some() {
            bail!("remote skill package must be a GitHub owner/repo ref or GitHub tree URL");
        }
        Ok(Self {
            owner: owner.to_string(),
            repo: repo.to_string(),
            reference: String::new(),
            path: "skills".to_string(),
        })
    }

    fn parse_github_path(path: &str) -> Result<Self> {
        let parts = path.split('/').collect::<Vec<_>>();
        if parts.len() < 2 || parts[0].is_empty() || parts[1].is_empty() {
            bail!("remote GitHub skill URL must include owner and repo");
        }
        let owner = parts[0].to_string();
        let repo = parts[1].trim_end_matches(".git").to_string();
        let mut reference = String::new();
        let mut skill_path = "skills".to_string();
        if parts.get(2) == Some(&"tree") {
            if let Some(part) = parts.get(3).filter(|part| !part.is_empty()) {
                reference = part.to_string();
            }
            let tree_path = parts.get(4..).unwrap_or(&[]).join("/");
            if !tree_path.is_empty() {
                skill_path = tree_path;
            }
        }
        Ok(Self {
            owner,
            repo,
            reference,
            path: skill_path,
        })
    }
}

impl RemoteSkillSource {
    fn parse(package: &str, skill: Option<&str>) -> Result<Self> {
        let trimmed = package.trim_end_matches('/');
        if let Some(url) = trimmed.strip_prefix("https://github.com/") {
            return Self::parse_github_path(url, skill);
        }
        if let Some(url) = trimmed.strip_prefix("http://github.com/") {
            return Self::parse_github_path(url, skill);
        }
        Self::parse_package_ref(trimmed, skill)
    }

    fn parse_github_path(path: &str, skill: Option<&str>) -> Result<Self> {
        let parts = path.split('/').collect::<Vec<_>>();
        if parts.len() < 2 || parts[0].is_empty() || parts[1].is_empty() {
            bail!("remote GitHub skill URL must include owner and repo");
        }
        let owner = parts[0].to_string();
        let repo = parts[1].trim_end_matches(".git").to_string();
        let mut reference = String::new();
        let mut skill_path = String::new();
        if parts.get(2) == Some(&"tree") {
            if let Some(part) = parts.get(3).filter(|part| !part.is_empty()) {
                reference = part.to_string();
            }
            skill_path = parts.get(4..).unwrap_or(&[]).join("/");
        }
        if let Some(skill) = skill {
            validate_skill_name(skill)?;
            if skill_path.is_empty() {
                skill_path = format!("skills/{skill}");
            }
        }
        let skill_name = skill
            .map(str::to_string)
            .or_else(|| {
                skill_path
                    .rsplit('/')
                    .next()
                    .filter(|name| !name.is_empty())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| repo.clone());
        validate_skill_name(&skill_name)?;
        if skill_path.is_empty() {
            skill_path = format!("skills/{skill_name}");
        }
        Ok(Self {
            owner,
            repo,
            reference,
            path: skill_path,
            skill_name,
        })
    }

    fn parse_package_ref(package: &str, skill: Option<&str>) -> Result<Self> {
        let (repo_ref, embedded_skill) = package
            .rsplit_once('@')
            .filter(|(repo, skill)| !repo.is_empty() && !skill.is_empty())
            .map(|(repo, skill)| (repo, Some(skill)))
            .unwrap_or((package, None));
        let mut parts = repo_ref.split('/');
        let owner = parts.next().unwrap_or_default();
        let repo = parts.next().unwrap_or_default().trim_end_matches(".git");
        if owner.is_empty() || repo.is_empty() || parts.next().is_some() {
            bail!("remote skill package must be a GitHub owner/repo ref or GitHub tree URL");
        }
        let skill_name = skill
            .or(embedded_skill)
            .map(str::to_string)
            .unwrap_or_else(|| repo.to_string());
        validate_skill_name(&skill_name)?;
        Ok(Self {
            owner: owner.to_string(),
            repo: repo.to_string(),
            reference: String::new(),
            path: format!("skills/{skill_name}"),
            skill_name,
        })
    }
}

fn remote_package_ref_installs_all_skills(package: &str) -> bool {
    let trimmed = package.trim_end_matches('/');
    if trimmed
        .rsplit_once('@')
        .is_some_and(|(repo, skill)| !repo.is_empty() && !skill.is_empty())
    {
        return false;
    }
    if let Some(path) = trimmed
        .strip_prefix("https://github.com/")
        .or_else(|| trimmed.strip_prefix("http://github.com/"))
    {
        return path.split('/').nth(2) != Some("tree");
    }
    true
}

/// Download a remote skill directory and finalize the installation.
/// Downloads to `tmp`, verifies `SKILL_ENTRYPOINT` exists, records install
/// metadata, then renames to `destination`.
fn finalize_remote_skill_download(
    client: &reqwest::blocking::Client,
    source: &RemoteSkillSource,
    skill_path: &str,
    tmp: &Path,
    destination: &Path,
    package: &str,
) -> Result<()> {
    download_github_directory(client, source, skill_path, tmp)?;
    if !tmp.join(SKILL_ENTRYPOINT).is_file() {
        bail!(
            "remote skill '{}' did not contain {} at {}",
            package,
            SKILL_ENTRYPOINT,
            skill_path
        );
    }
    record_install_metadata(tmp, "remote")?;
    fs::rename(tmp, destination).with_context(|| {
        format!(
            "failed to install remote skill {} -> {}",
            package,
            destination.display()
        )
    })
}

fn install_github_remote_skill(
    user_home: &Path,
    package: &str,
    source: &RemoteSkillSource,
) -> Result<()> {
    const REMOTE_SKILL_INSTALL_TIMEOUT: Duration =
        Duration::from_secs(REMOTE_SKILL_INSTALL_TIMEOUT_SECONDS);

    let skills_root = user_library_skills_root(user_home);
    fs::create_dir_all(&skills_root)
        .with_context(|| format!("failed to create {}", skills_root.display()))?;
    let destination = install_destination(&skills_root, &source.skill_name)?;
    let tmp = skills_root.join(format!(
        ".tmp-holon-skill-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    if tmp.exists() {
        fs::remove_dir_all(&tmp).with_context(|| format!("failed to clear {}", tmp.display()))?;
    }
    fs::create_dir_all(&tmp).with_context(|| format!("failed to create {}", tmp.display()))?;
    let client = reqwest::blocking::Client::builder()
        .timeout(REMOTE_SKILL_INSTALL_TIMEOUT)
        .user_agent(format!("holon/{}", env!("CARGO_PKG_VERSION")))
        .default_headers(github_auth_headers())
        .build()
        .context("failed to create remote skill HTTP client")?;

    let effective_ref = resolve_effective_reference(
        &client,
        &source.owner,
        &source.repo,
        &source.reference,
        package,
    )?;
    let resolved_source = RemoteSkillSource {
        reference: effective_ref.clone(),
        ..source.clone()
    };

    // Try the candidate path first (e.g. `skills/my-skill`).
    let result = finalize_remote_skill_download(
        &client,
        &resolved_source,
        &source.path,
        &tmp,
        &destination,
        package,
    );

    // Fallback: if the candidate path failed, search the full tree for the
    // actual skill directory. This handles non-flat catalog layouts where
    // SKILL.md lives deeper than `skills/<name>/`.
    let result = match result {
        Ok(()) => Ok(()),
        Err(original_error) => {
            let _ = fs::remove_dir_all(&tmp);
            fs::create_dir_all(&tmp)
                .with_context(|| format!("failed to create {}", tmp.display()))?;
            match discover_skills_via_tree(
                &client,
                &source.owner,
                &source.repo,
                &effective_ref,
                "",
                package,
            ) {
                Ok(skills) => match skills
                    .iter()
                    .find(|(name, _)| name == &source.skill_name)
                    .map(|(_, path)| path.clone())
                    .filter(|path| path != &source.path)
                {
                    Some(found_path) => finalize_remote_skill_download(
                        &client,
                        &resolved_source,
                        &found_path,
                        &tmp,
                        &destination,
                        package,
                    ),
                    None => Err(original_error),
                },
                Err(_) => Err(original_error),
            }
        }
    };
    if result.is_err() {
        let _ = fs::remove_dir_all(&tmp);
    }
    result
}

fn install_github_remote_skill_set(
    user_home: &Path,
    package: &str,
    source: &RemoteSkillSetSource,
) -> Result<Vec<String>> {
    const REMOTE_SKILL_INSTALL_TIMEOUT: Duration =
        Duration::from_secs(REMOTE_SKILL_INSTALL_TIMEOUT_SECONDS);

    let client = reqwest::blocking::Client::builder()
        .timeout(REMOTE_SKILL_INSTALL_TIMEOUT)
        .user_agent(format!("holon/{}", env!("CARGO_PKG_VERSION")))
        .default_headers(github_auth_headers())
        .build()
        .context("failed to create remote skill HTTP client")?;
    let effective_ref = resolve_effective_reference(
        &client,
        &source.owner,
        &source.repo,
        &source.reference,
        package,
    )?;
    let skills = discover_skills_via_tree(
        &client,
        &source.owner,
        &source.repo,
        &effective_ref,
        &source.path,
        package,
    )?;
    if skills.is_empty() {
        bail!(
            "remote skill package '{}' did not contain any skill directories under {}",
            package,
            source.path
        );
    }

    let mut installed = Vec::new();
    for (skill_name, skill_path) in skills {
        let source = RemoteSkillSource {
            owner: source.owner.clone(),
            repo: source.repo.clone(),
            reference: effective_ref.clone(),
            path: skill_path,
            skill_name,
        };
        install_github_remote_skill(user_home, package, &source)?;
        installed.push(source.skill_name);
    }
    Ok(installed)
}

#[derive(Debug, Deserialize)]
struct GithubContentEntry {
    name: String,
    path: String,
    #[serde(rename = "type")]
    kind: String,
    download_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GithubRepoInfo {
    default_branch: String,
}

#[derive(Debug, Deserialize)]
struct GithubTreeEntry {
    path: String,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Debug, Deserialize)]
struct GithubTreeResponse {
    tree: Vec<GithubTreeEntry>,
    truncated: bool,
}

/// Resolve the effective Git reference for a remote skill source.
/// If the reference is empty (unset by parse), queries the repository's
/// default branch via `GET /repos/{owner}/{repo}`.
fn resolve_effective_reference(
    client: &reqwest::blocking::Client,
    owner: &str,
    repo: &str,
    reference: &str,
    package: &str,
) -> Result<String> {
    if !reference.is_empty() {
        return Ok(reference.to_string());
    }
    resolve_default_branch(client, owner, repo, package)
}

fn resolve_default_branch(
    client: &reqwest::blocking::Client,
    owner: &str,
    repo: &str,
    package: &str,
) -> Result<String> {
    let url = format!(
        "https://api.github.com/repos/{}/{}",
        utf8_percent_encode(owner, NON_ALPHANUMERIC),
        utf8_percent_encode(repo, NON_ALPHANUMERIC),
    );
    let response = remote_skill_request(client.get(&url).send(), package, || {
        format!("failed to fetch repository metadata for {url}")
    })?;
    if !response.status().is_success() {
        return Err(RemoteSkillInstallFailed {
            package: package.to_string(),
            status: Some(response.status().as_u16().into()),
            stdout: String::new(),
            stderr: response.text().unwrap_or_default(),
        }
        .into());
    }
    let info: GithubRepoInfo = response
        .json()
        .with_context(|| format!("failed to parse repository metadata for {url}"))?;
    Ok(info.default_branch)
}

/// Discover all skill directories (paths containing `SKILL.md`) via the Git
/// Trees API. Returns `(skill_name, skill_dir_path)` pairs filtered by
/// `path_prefix`.
fn discover_skills_via_tree(
    client: &reqwest::blocking::Client,
    owner: &str,
    repo: &str,
    reference: &str,
    path_prefix: &str,
    package: &str,
) -> Result<Vec<(String, String)>> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/git/trees/{}?recursive=1",
        utf8_percent_encode(owner, NON_ALPHANUMERIC),
        utf8_percent_encode(repo, NON_ALPHANUMERIC),
        utf8_percent_encode(reference, NON_ALPHANUMERIC),
    );
    let response = remote_skill_request(client.get(&url).send(), package, || {
        format!("failed to fetch repository tree for {url}")
    })?;
    if !response.status().is_success() {
        return Err(RemoteSkillInstallFailed {
            package: package.to_string(),
            status: Some(response.status().as_u16().into()),
            stdout: String::new(),
            stderr: response.text().unwrap_or_default(),
        }
        .into());
    }
    let tree_data: GithubTreeResponse = response
        .json()
        .with_context(|| format!("failed to parse GitHub tree response for {url}"))?;
    if tree_data.truncated {
        bail!(
            "repository tree for '{}' is too large for recursive skill discovery; \
             specify skill paths explicitly via GitHub tree URLs",
            package
        );
    }
    let prefix = if path_prefix.is_empty() {
        String::new()
    } else {
        format!("{}/", path_prefix.trim_end_matches('/'))
    };
    let mut skills = Vec::new();
    for entry in tree_data.tree {
        if entry.kind != "blob" {
            continue;
        }
        let Some(dir) = entry.path.strip_suffix("/SKILL.md") else {
            continue;
        };
        if !prefix.is_empty() && !dir.starts_with(&prefix) {
            continue;
        }
        let Some(name) = dir.rsplit('/').next() else {
            continue;
        };
        if validate_skill_name(name).is_err() {
            continue;
        }
        skills.push((name.to_string(), dir.to_string()));
    }
    Ok(skills)
}

fn github_auth_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Some(token) = std::env::var("GITHUB_TOKEN")
        .ok()
        .filter(|token| !token.trim().is_empty())
        .or_else(|| {
            std::env::var("GH_TOKEN")
                .ok()
                .filter(|token| !token.trim().is_empty())
        })
    {
        let value = format!("Bearer {token}");
        match HeaderValue::from_str(&value) {
            Ok(value) => {
                headers.insert(AUTHORIZATION, value);
            }
            Err(error) => {
                warn!("ignoring invalid GitHub token header value: {error}");
            }
        }
    }
    headers
}

fn download_github_directory(
    client: &reqwest::blocking::Client,
    source: &RemoteSkillSource,
    remote_path: &str,
    destination: &Path,
) -> Result<()> {
    let url = github_contents_url(source, remote_path);
    let package = format!("{}/{}", source.owner, source.repo);
    let response = remote_skill_request(client.get(&url).send(), &package, || {
        format!("failed to fetch remote skill directory {url}")
    })?;
    if !response.status().is_success() {
        return Err(RemoteSkillInstallFailed {
            package,
            status: Some(response.status().as_u16().into()),
            stdout: String::new(),
            stderr: response.text().unwrap_or_default(),
        }
        .into());
    }
    let entries: Vec<GithubContentEntry> = response
        .json()
        .with_context(|| format!("failed to parse GitHub contents response for {url}"))?;
    for entry in entries {
        validate_skill_archive_entry(&entry.name)?;
        let child = destination.join(&entry.name);
        match entry.kind.as_str() {
            "dir" => {
                fs::create_dir_all(&child)
                    .with_context(|| format!("failed to create {}", child.display()))?;
                download_github_directory(client, source, &entry.path, &child)?;
            }
            "file" => {
                let download_url = entry.download_url.ok_or_else(|| {
                    anyhow::anyhow!("GitHub file {} has no download URL", entry.path)
                })?;
                validate_github_download_url(&download_url)?;
                let response =
                    remote_skill_request(client.get(&download_url).send(), &package, || {
                        format!("failed to download {download_url}")
                    })?;
                let response = remote_skill_request(response.error_for_status(), &package, || {
                    format!("GitHub file download failed for {download_url}")
                })?;
                let bytes = remote_skill_request(response.bytes(), &package, || {
                    format!("failed to read {download_url}")
                })?;
                fs::write(&child, bytes)
                    .with_context(|| format!("failed to write {}", child.display()))?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn remote_skill_request<T, F>(
    result: std::result::Result<T, reqwest::Error>,
    package: &str,
    context: F,
) -> Result<T>
where
    F: FnOnce() -> String,
{
    result.map_err(|error| {
        if error.is_timeout() {
            RemoteSkillInstallTimedOut {
                package: package.to_string(),
                timeout_seconds: REMOTE_SKILL_INSTALL_TIMEOUT_SECONDS,
            }
            .into()
        } else {
            anyhow::Error::new(error).context(context())
        }
    })
}

fn validate_github_download_url(download_url: &str) -> Result<()> {
    let url = reqwest::Url::parse(download_url)
        .with_context(|| format!("invalid GitHub download URL {download_url}"))?;
    if url.scheme() != "https" || url.host_str() != Some("raw.githubusercontent.com") {
        bail!("GitHub download URL uses unexpected host: {download_url}");
    }
    Ok(())
}

fn github_contents_url(source: &RemoteSkillSource, remote_path: &str) -> String {
    github_contents_url_parts(&source.owner, &source.repo, &source.reference, remote_path)
}

fn github_contents_url_parts(
    owner: &str,
    repo: &str,
    reference: &str,
    remote_path: &str,
) -> String {
    let encoded_path = remote_path
        .split('/')
        .map(|part| utf8_percent_encode(part, NON_ALPHANUMERIC).to_string())
        .collect::<Vec<_>>()
        .join("/");
    let reference = utf8_percent_encode(reference, NON_ALPHANUMERIC);
    format!(
        "https://api.github.com/repos/{}/{}/contents/{}?ref={}",
        owner, repo, encoded_path, reference
    )
}

fn validate_skill_archive_entry(name: &str) -> Result<()> {
    if name.is_empty() || name.contains(std::path::is_separator) || name == "." || name == ".." {
        bail!("remote skill archive contains invalid entry name: {name}");
    }
    Ok(())
}

#[cfg(test)]
#[derive(Clone)]
struct RecordedRemoteSkillInstall {
    user_home: PathBuf,
    package: String,
    skill: Option<String>,
}

#[cfg(test)]
struct RecordingRemoteSkillInstaller {
    calls: std::sync::Arc<std::sync::Mutex<Vec<RecordedRemoteSkillInstall>>>,
}

#[cfg(test)]
impl RemoteSkillInstaller for RecordingRemoteSkillInstaller {
    fn add_global(
        &self,
        user_home: &Path,
        package: &str,
        skill: Option<&str>,
    ) -> Result<Vec<String>> {
        let skill_name = skill.unwrap_or(remote_package_default_skill_name(package)?);
        self.calls.lock().unwrap().push(RecordedRemoteSkillInstall {
            user_home: user_home.into(),
            package: package.into(),
            skill: skill.map(str::to_string),
        });
        Ok(vec![skill_name.to_string()])
    }
}

#[cfg(test)]
struct UnavailableRemoteSkillInstaller;

#[cfg(test)]
impl RemoteSkillInstaller for UnavailableRemoteSkillInstaller {
    fn add_global(
        &self,
        _user_home: &Path,
        _package: &str,
        _skill: Option<&str>,
    ) -> Result<Vec<String>> {
        bail!("remote installer unavailable")
    }
}

/// Validate that a skill name is a single path component with no traversal.
fn validate_skill_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.contains(std::path::is_separator)
        || name == "."
        || name == ".."
        || name.contains("..")
    {
        bail!(
            "invalid skill name '{}': must be a single path component without traversal",
            name
        );
    }
    Ok(())
}

fn install_destination(skills_root: &Path, skill_name: &str) -> Result<PathBuf> {
    validate_skill_name(skill_name)?;
    let destination = skills_root.join(skill_name);
    if fs::symlink_metadata(&destination).is_ok() {
        return Err(SkillInstallConflict {
            skill_name: skill_name.to_string(),
            destination,
        }
        .into());
    }
    Ok(destination)
}

pub fn install_skill(agent_home: &Path, kind: &crate::types::SkillInstallKind) -> Result<String> {
    install_skill_with_user_home(agent_home, None, kind)
}

pub fn install_skill_with_user_home(
    agent_home: &Path,
    user_home: Option<&Path>,
    kind: &crate::types::SkillInstallKind,
) -> Result<String> {
    install_skill_with_user_home_and_remote_installer(
        agent_home,
        user_home,
        kind,
        &RustRemoteSkillInstaller,
    )
}

fn install_skill_with_user_home_and_remote_installer(
    agent_home: &Path,
    user_home: Option<&Path>,
    kind: &crate::types::SkillInstallKind,
    remote_installer: &dyn RemoteSkillInstaller,
) -> Result<String> {
    let skills_root = agent_skills_root(agent_home);
    fs::create_dir_all(&skills_root)
        .with_context(|| format!("failed to create {}", skills_root.display()))?;
    let name = match kind {
        crate::types::SkillInstallKind::Builtin { name } => {
            validate_skill_name(name)?;
            let _ = install_destination(&skills_root, name)?;
            let destination =
                crate::agent_template::materialize_builtin_skill_ref(&skills_root, name)?;
            record_install_metadata(&destination, "builtin")?;
            name.clone()
        }
        crate::types::SkillInstallKind::Named { name, mode } => {
            validate_skill_name(name)?;
            if crate::agent_template::builtin_skill_names().contains(&name.as_str()) {
                let _ = install_destination(&skills_root, name)?;
                let destination =
                    crate::agent_template::materialize_builtin_skill_ref(&skills_root, name)?;
                record_install_metadata(&destination, "builtin")?;
                name.clone()
            } else {
                let path = resolve_user_skill_by_name(user_home, name)?;
                materialize_local_skill(&skills_root, &path, mode)?
            }
        }
        crate::types::SkillInstallKind::Local { path, mode } => {
            materialize_local_skill(&skills_root, path, mode)?
        }
        crate::types::SkillInstallKind::Remote {
            package,
            skill,
            mode,
        } => {
            validate_remote_package(package)?;
            if let Some(skill) = skill {
                validate_skill_name(skill)?;
            }
            let skill_names = remote_installer.add_global(
                user_home.context("remote skill install requires a user home")?,
                package,
                skill.as_deref(),
            )?;
            let skill_name = skill_names
                .first()
                .context("remote skill install did not install any skills")?;
            let path = resolve_user_skill_by_name(user_home, &skill_name)?;
            materialize_local_skill(&skills_root, &path, mode)?
        }
    };
    Ok(name)
}

pub fn add_library_skill(
    user_home: &Path,
    kind: &crate::types::SkillInstallKind,
) -> Result<String> {
    add_library_skill_with_remote_installer(user_home, kind, &RustRemoteSkillInstaller)
}

fn add_library_skill_with_remote_installer(
    user_home: &Path,
    kind: &crate::types::SkillInstallKind,
    remote_installer: &dyn RemoteSkillInstaller,
) -> Result<String> {
    let skills_root = user_library_skills_root(user_home);
    fs::create_dir_all(&skills_root)
        .with_context(|| format!("failed to create {}", skills_root.display()))?;
    let name = match kind {
        crate::types::SkillInstallKind::Builtin { name } => {
            validate_skill_name(name)?;
            let _ = install_destination(&skills_root, name)?;
            let destination =
                crate::agent_template::materialize_builtin_skill_ref(&skills_root, name)?;
            record_install_metadata(&destination, "builtin")?;
            name.clone()
        }
        crate::types::SkillInstallKind::Named { name, mode } => {
            if crate::agent_template::builtin_skill_names().contains(&name.as_str()) {
                validate_skill_name(name)?;
                let _ = install_destination(&skills_root, name)?;
                let destination =
                    crate::agent_template::materialize_builtin_skill_ref(&skills_root, name)?;
                record_install_metadata(&destination, "builtin")?;
                name.clone()
            } else {
                let path = resolve_user_skill_by_name(Some(user_home), name)?;
                materialize_local_skill(&skills_root, &path, mode)?
            }
        }
        crate::types::SkillInstallKind::Local { path, mode } => {
            materialize_local_skill(&skills_root, path, mode)?
        }
        crate::types::SkillInstallKind::Remote {
            package,
            skill,
            mode: _,
        } => {
            validate_remote_package(package)?;
            if let Some(skill) = skill {
                validate_skill_name(skill)?;
            }
            let skill_names = remote_installer.add_global(user_home, package, skill.as_deref())?;
            let first_skill_name = skill_names
                .first()
                .context("remote skill install did not install any skills")?
                .clone();
            for skill_name in &skill_names {
                sync_skill_lock_entry(user_home, skill_name)?;
            }
            first_skill_name
        }
    };
    if !matches!(kind, crate::types::SkillInstallKind::Remote { .. }) {
        sync_skill_lock_entry(user_home, &name)?;
    }
    Ok(name)
}

pub fn enable_agent_skill(
    agent_home: &Path,
    user_home: Option<&Path>,
    name: &str,
    mode: &SkillInstallMode,
) -> Result<String> {
    install_skill_with_user_home(
        agent_home,
        user_home,
        &crate::types::SkillInstallKind::Named {
            name: name.to_string(),
            mode: mode.clone(),
        },
    )
}

fn validate_remote_package(package: &str) -> Result<()> {
    if package.trim().is_empty() {
        bail!("remote skill package must not be empty");
    }
    if package.trim() != package {
        bail!("remote skill package must not contain leading or trailing whitespace");
    }
    if package.starts_with('-') {
        bail!("remote skill package must not start with '-'");
    }
    if package
        .chars()
        .any(|ch| ch.is_control() || ch.is_ascii_whitespace())
    {
        bail!("remote skill package must not contain whitespace or control characters");
    }
    Ok(())
}

#[cfg(test)]
fn remote_package_default_skill_name(package: &str) -> Result<&str> {
    package
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| anyhow::anyhow!("remote skill package has no usable default skill name"))
}

fn materialize_local_skill(
    skills_root: &Path,
    path: &Path,
    mode: &SkillInstallMode,
) -> Result<String> {
    let path = normalize_local_skill_path(path)?;
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    validate_skill_name(file_name)?;
    let _ = install_destination(skills_root, file_name)?;
    let destination = match mode {
        SkillInstallMode::Linked => materialize_linked_skill_ref(skills_root, &path)?,
        SkillInstallMode::Copied => materialize_copied_skill_ref(skills_root, &path)?,
    };
    if *mode == SkillInstallMode::Copied {
        record_install_metadata(&destination, "copied")?;
    }
    Ok(destination
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .unwrap_or_default())
}

fn normalize_local_skill_path(path: &Path) -> Result<PathBuf> {
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("failed to resolve current directory for local skill path")?
            .join(path)
    };
    if !resolved.is_dir() {
        bail!("local skill directory does not exist: {}", path.display());
    }
    let entrypoint = resolved.join(SKILL_ENTRYPOINT);
    if !entrypoint.is_file() {
        bail!(
            "local skill ref {} does not contain {}",
            resolved.display(),
            SKILL_ENTRYPOINT
        );
    }
    Ok(resolved)
}

fn resolve_user_skill_by_name(user_home: Option<&Path>, name: &str) -> Result<PathBuf> {
    for root in existing_skill_roots(user_home, &COMPAT_SKILL_ROOT_SUFFIXES) {
        let mut root_matches = Vec::new();
        for skill in load_catalog_for_scope(SkillScope::UserGlobal, &root)? {
            let dir_name = skill
                .path
                .parent()
                .and_then(|path| path.file_name())
                .and_then(|name| name.to_str())
                .unwrap_or("");
            if skill.name == name || dir_name == name {
                if let Some(skill_dir) = skill.path.parent() {
                    root_matches.push(skill_dir.to_path_buf());
                }
            }
        }
        root_matches.sort();
        root_matches.dedup();
        match root_matches.as_slice() {
            [] => {}
            [path] => return Ok(path.clone()),
            paths => bail!(
                "skill '{}' matched multiple user-global skill directories under {}: {}; use an explicit path",
                name,
                root.display(),
                paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
    bail!(
        "skill '{}' is not a builtin skill and was not found in the user-global skill catalog; use an explicit path",
        name
    )
}

fn materialize_linked_skill_ref(skills_root: &Path, path: &Path) -> Result<PathBuf> {
    crate::agent_template::materialize_local_skill_ref(skills_root, path)
}

fn materialize_copied_skill_ref(skills_root: &Path, path: &Path) -> Result<PathBuf> {
    fs::create_dir_all(skills_root)
        .with_context(|| format!("failed to create {}", skills_root.display()))?;
    let skill_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "local skill ref {} has no usable directory name",
                path.display()
            )
        })?;
    let destination = skills_root.join(skill_name);
    if destination.exists() {
        bail!(
            "local skill destination {} already exists",
            destination.display()
        );
    }
    if let Err(error) = copy_dir_all(path, &destination).with_context(|| {
        format!(
            "failed to copy local skill ref {} -> {}",
            path.display(),
            destination.display()
        )
    }) {
        let _ = fs::remove_dir_all(&destination);
        return Err(error);
    }
    Ok(destination)
}

fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).with_context(|| format!("failed to create {}", dst.display()))?;
    for entry in fs::read_dir(src).with_context(|| format!("failed to read {}", src.display()))? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_all(&src_path, &dst_path)?;
        } else if file_type.is_file() {
            fs::copy(&src_path, &dst_path).with_context(|| {
                format!(
                    "failed to copy {} -> {}",
                    src_path.display(),
                    dst_path.display()
                )
            })?;
        } else if file_type.is_symlink() {
            bail!(
                "copy mode does not support symlinks inside skill directories: {}",
                src_path.display()
            );
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct SkillInstallMetadata {
    install_mode: String,
}

fn record_install_metadata(destination: &Path, install_mode: &str) -> Result<()> {
    let payload = serde_json::to_vec_pretty(&SkillInstallMetadata {
        install_mode: install_mode.into(),
    })?;
    fs::write(destination.join(INSTALL_METADATA_FILENAME), payload).with_context(|| {
        format!(
            "failed to write install metadata for {}",
            destination.display()
        )
    })
}

fn read_install_metadata(skill_dir: &Path) -> Option<SkillInstallMetadata> {
    let content = fs::read(skill_dir.join(INSTALL_METADATA_FILENAME)).ok()?;
    serde_json::from_slice(&content).ok()
}

pub fn uninstall_skill(agent_home: &Path, name: &str) -> Result<()> {
    validate_skill_name(name)?;
    let mut matches = Vec::new();
    for skills_root in existing_skill_roots(Some(agent_home), &SKILL_ROOT_SUFFIXES) {
        let destination = skills_root.join(name);
        if let Ok(meta) = fs::symlink_metadata(&destination) {
            matches.push((destination, meta));
        }
    }
    let [(destination, meta)] = matches.as_slice() else {
        if matches.is_empty() {
            bail!(
                "skill '{}' is not installed in agent skills directory",
                name
            );
        }
        bail!(
            "skill '{}' is installed in multiple agent skill roots: {}; remove the duplicate directories manually",
            name,
            matches
                .iter()
                .map(|(path, _)| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    };
    // Verify this looks like a skill installation by checking for SKILL_ENTRYPOINT.
    if !destination.join(SKILL_ENTRYPOINT).exists() && !meta.is_symlink() {
        bail!(
            "directory '{}' does not appear to be a skill installation (missing {})",
            destination.display(),
            SKILL_ENTRYPOINT
        );
    }
    crate::agent_template::remove_materialized_skill_destination(&destination)?;
    Ok(())
}

pub fn remove_library_skill(user_home: &Path, name: &str) -> Result<()> {
    remove_skill_from_roots(
        user_home,
        name,
        &COMPAT_SKILL_ROOT_SUFFIXES,
        "Skill Library",
    )?;
    remove_skill_lock_entry(user_home, name)?;
    Ok(())
}

pub fn disable_agent_skill(agent_home: &Path, name: &str) -> Result<()> {
    uninstall_skill(agent_home, name)
}

fn remove_skill_from_roots(base: &Path, name: &str, suffixes: &[&str], label: &str) -> Result<()> {
    validate_skill_name(name)?;
    let mut matches = Vec::new();
    for skills_root in existing_skill_roots(Some(base), suffixes) {
        let destination = skills_root.join(name);
        if let Ok(meta) = fs::symlink_metadata(&destination) {
            matches.push((destination, meta));
        }
    }
    let [(destination, meta)] = matches.as_slice() else {
        if matches.is_empty() {
            bail!("skill '{}' is not present in the {}", name, label);
        }
        bail!(
            "skill '{}' is present in multiple {} roots: {}; remove the duplicate directories manually",
            name,
            label,
            matches
                .iter()
                .map(|(path, _)| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    };
    if !destination.join(SKILL_ENTRYPOINT).exists() && !meta.is_symlink() {
        bail!(
            "directory '{}' does not appear to be a skill installation (missing {})",
            destination.display(),
            SKILL_ENTRYPOINT
        );
    }
    crate::agent_template::remove_materialized_skill_destination(destination)?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SkillLibraryCheckResult {
    pub skills_root: PathBuf,
    pub lock_path: PathBuf,
    pub skill_count: usize,
    pub lock_skill_count: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SkillLibraryReconcileResult {
    pub skills_root: PathBuf,
    pub lock_path: PathBuf,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub checked: SkillLibraryCheckResult,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SkillLibraryUpdateResult {
    pub skills_root: PathBuf,
    pub lock_path: PathBuf,
    pub statuses: Vec<SkillLibraryUpdateStatus>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SkillLibraryUpdateStatus {
    pub name: String,
    pub status: SkillLibraryUpdateState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SkillLibraryUpdateState {
    Updated,
    Unchanged,
    Skipped,
    Failed,
}

pub fn check_library_skills(
    user_home: &Path,
    name_filter: Option<&str>,
) -> Result<SkillLibraryCheckResult> {
    if let Some(name) = name_filter {
        validate_skill_name(name)?;
    }
    let skills_root = user_library_skills_root(user_home);
    let lock_path = skill_lock_path(user_home);
    let present = library_skill_dirs(&skills_root, name_filter)?;
    let lock = read_skill_lock(user_home)?;
    let locked = locked_skill_names(&lock, name_filter);
    let present_names = present.keys().cloned().collect::<BTreeSet<_>>();
    let mut warnings = Vec::new();
    for name in present_names.difference(&locked) {
        warnings.push(format!(
            "skill '{name}' is present in ~/.agents/skills but missing from .skill-lock.json"
        ));
    }
    for name in locked.difference(&present_names) {
        warnings.push(format!(
            "skill '{name}' is present in .skill-lock.json but missing from ~/.agents/skills"
        ));
    }
    Ok(SkillLibraryCheckResult {
        skills_root,
        lock_path,
        skill_count: present_names.len(),
        lock_skill_count: locked.len(),
        warnings,
    })
}

pub fn reconcile_library_skills(
    user_home: &Path,
    name_filter: Option<&str>,
) -> Result<SkillLibraryReconcileResult> {
    if let Some(name) = name_filter {
        validate_skill_name(name)?;
    }
    let skills_root = user_library_skills_root(user_home);
    let lock_path = skill_lock_path(user_home);
    let present = library_skill_dirs(&skills_root, name_filter)?;
    let mut lock = read_skill_lock(user_home)?;
    let locked = locked_skill_names(&lock, name_filter);
    let present_names = present.keys().cloned().collect::<BTreeSet<_>>();
    let mut added = Vec::new();
    for name in present_names.difference(&locked) {
        upsert_skill_lock_entry(
            &mut lock,
            name,
            present.get(name).expect("present skill path"),
        )?;
        added.push(name.clone());
    }
    let mut removed = Vec::new();
    for name in locked.difference(&present_names) {
        remove_skill_lock_name(&mut lock, name);
        removed.push(name.clone());
    }
    if !added.is_empty() || !removed.is_empty() {
        write_skill_lock(user_home, &lock)?;
    }
    let checked = check_library_skills(user_home, name_filter)?;
    Ok(SkillLibraryReconcileResult {
        skills_root,
        lock_path,
        added,
        removed,
        checked,
    })
}

pub fn update_library_skills(
    user_home: &Path,
    name_filter: Option<&str>,
) -> Result<SkillLibraryUpdateResult> {
    update_library_skills_with_remote_updater(user_home, name_filter, &GithubSkillRemoteUpdater)
}

trait SkillRemoteUpdater {
    fn latest_hash(&self, source: &LockedRemoteSkillSource) -> Result<String>;
    fn install(&self, source: &LockedRemoteSkillSource, destination: &Path) -> Result<()>;
}

struct GithubSkillRemoteUpdater;

impl SkillRemoteUpdater for GithubSkillRemoteUpdater {
    fn latest_hash(&self, source: &LockedRemoteSkillSource) -> Result<String> {
        let tmp = temp_skill_update_dir("hash");
        if tmp.exists() {
            fs::remove_dir_all(&tmp)
                .with_context(|| format!("failed to clear {}", tmp.display()))?;
        }
        fs::create_dir_all(&tmp).with_context(|| format!("failed to create {}", tmp.display()))?;
        let result = self
            .install(source, &tmp)
            .and_then(|()| hash_skill_folder(&tmp));
        let _ = fs::remove_dir_all(&tmp);
        result
    }

    fn install(&self, source: &LockedRemoteSkillSource, destination: &Path) -> Result<()> {
        const REMOTE_SKILL_INSTALL_TIMEOUT: Duration =
            Duration::from_secs(REMOTE_SKILL_INSTALL_TIMEOUT_SECONDS);
        let client = reqwest::blocking::Client::builder()
            .timeout(REMOTE_SKILL_INSTALL_TIMEOUT)
            .user_agent(format!("holon/{}", env!("CARGO_PKG_VERSION")))
            .default_headers(github_auth_headers())
            .build()
            .context("failed to create remote skill HTTP client")?;
        let package = format!("{}/{}", source.owner, source.repo);
        let effective_ref = resolve_effective_reference(
            &client,
            &source.owner,
            &source.repo,
            &source.reference,
            &package,
        )?;
        download_github_directory(
            &client,
            &RemoteSkillSource {
                owner: source.owner.clone(),
                repo: source.repo.clone(),
                reference: effective_ref,
                path: source.skill_path.clone(),
                skill_name: source.skill_name.clone(),
            },
            &source.skill_path,
            destination,
        )
    }
}

fn update_library_skills_with_remote_updater(
    user_home: &Path,
    name_filter: Option<&str>,
    updater: &dyn SkillRemoteUpdater,
) -> Result<SkillLibraryUpdateResult> {
    if let Some(name) = name_filter {
        validate_skill_name(name)?;
    }
    let skills_root = user_library_skills_root(user_home);
    let lock_path = skill_lock_path(user_home);
    let mut lock = read_skill_lock(user_home)?;
    let Some(skills) = lock_skills(&lock) else {
        return Ok(SkillLibraryUpdateResult {
            skills_root,
            lock_path,
            statuses: Vec::new(),
        });
    };
    let entries = skills
        .iter()
        .filter(|(name, _)| name_filter.is_none_or(|filter| name.as_str() == filter))
        .map(|(name, entry)| (name.clone(), entry.clone()))
        .collect::<Vec<_>>();

    let mut statuses = Vec::new();
    let mut lock_changed = false;
    for (name, entry) in entries {
        let Some(source) = LockedRemoteSkillSource::from_lock_entry(&name, &entry) else {
            statuses.push(SkillLibraryUpdateStatus {
                name,
                status: SkillLibraryUpdateState::Skipped,
                previous_hash: None,
                latest_hash: None,
                reason: Some("lock entry is not a supported remote GitHub v3 skill source".into()),
            });
            continue;
        };
        let previous_hash = source.skill_folder_hash.clone();
        let latest_hash = match updater.latest_hash(&source) {
            Ok(hash) => hash,
            Err(error) => {
                statuses.push(SkillLibraryUpdateStatus {
                    name,
                    status: SkillLibraryUpdateState::Failed,
                    previous_hash: Some(previous_hash),
                    latest_hash: None,
                    reason: Some(error.to_string()),
                });
                continue;
            }
        };
        if latest_hash == previous_hash {
            statuses.push(SkillLibraryUpdateStatus {
                name,
                status: SkillLibraryUpdateState::Unchanged,
                previous_hash: Some(previous_hash),
                latest_hash: Some(latest_hash),
                reason: None,
            });
            continue;
        }
        let destination = skills_root.join(&source.skill_name);
        let install_result = update_skill_destination(updater, &source, &destination);
        match install_result {
            Ok(()) => {
                refresh_updated_lock_entry(&mut lock, &source.skill_name, &latest_hash)?;
                lock_changed = true;
                statuses.push(SkillLibraryUpdateStatus {
                    name,
                    status: SkillLibraryUpdateState::Updated,
                    previous_hash: Some(previous_hash),
                    latest_hash: Some(latest_hash),
                    reason: None,
                });
            }
            Err(error) => {
                statuses.push(SkillLibraryUpdateStatus {
                    name,
                    status: SkillLibraryUpdateState::Failed,
                    previous_hash: Some(previous_hash),
                    latest_hash: Some(latest_hash),
                    reason: Some(error.to_string()),
                });
            }
        }
    }
    if lock_changed {
        write_skill_lock(user_home, &lock)?;
    }
    Ok(SkillLibraryUpdateResult {
        skills_root,
        lock_path,
        statuses,
    })
}

#[derive(Debug, Clone)]
struct LockedRemoteSkillSource {
    skill_name: String,
    owner: String,
    repo: String,
    reference: String,
    skill_path: String,
    skill_folder_hash: String,
}

impl LockedRemoteSkillSource {
    fn from_lock_entry(name: &str, entry: &serde_json::Value) -> Option<Self> {
        let object = entry.as_object()?;
        let source_type = object
            .get("sourceType")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        if !source_type.eq_ignore_ascii_case("github") {
            return None;
        }
        let skill_path = object
            .get("skillPath")
            .and_then(|value| value.as_str())
            .filter(|path| !path.is_empty())?
            .to_string();
        let skill_folder_hash = object
            .get("skillFolderHash")
            .and_then(|value| value.as_str())
            .filter(|hash| !hash.is_empty())?
            .to_string();
        let reference = object
            .get("ref")
            .and_then(|value| value.as_str())
            .filter(|reference| !reference.is_empty())
            .unwrap_or("")
            .to_string();
        let (owner, repo) = object
            .get("sourceUrl")
            .and_then(|value| value.as_str())
            .and_then(github_owner_repo_from_url)
            .or_else(|| {
                object
                    .get("source")
                    .and_then(|value| value.as_str())
                    .and_then(github_owner_repo_from_ref)
            })?;
        Some(Self {
            skill_name: name.to_string(),
            owner,
            repo,
            reference,
            skill_path,
            skill_folder_hash,
        })
    }
}

fn github_owner_repo_from_url(url: &str) -> Option<(String, String)> {
    let url = reqwest::Url::parse(url).ok()?;
    match url.host_str()? {
        "github.com" | "www.github.com" => {}
        _ => return None,
    }
    let mut segments = url.path_segments()?;
    let owner = segments.next()?.to_string();
    let repo = segments.next()?.trim_end_matches(".git").to_string();
    if owner.is_empty() || repo.is_empty() {
        None
    } else {
        Some((owner, repo))
    }
}

fn github_owner_repo_from_ref(source: &str) -> Option<(String, String)> {
    if source.starts_with("http://") || source.starts_with("https://") {
        return github_owner_repo_from_url(source);
    }
    let mut parts = source.trim_end_matches('/').split('/');
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.trim_end_matches(".git").to_string();
    if owner.is_empty() || repo.is_empty() || parts.next().is_some() {
        None
    } else {
        Some((owner, repo))
    }
}

fn update_skill_destination(
    updater: &dyn SkillRemoteUpdater,
    source: &LockedRemoteSkillSource,
    destination: &Path,
) -> Result<()> {
    let parent = destination.parent().ok_or_else(|| {
        anyhow::anyhow!("skill destination {} has no parent", destination.display())
    })?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let tmp = temp_skill_update_dir(&source.skill_name);
    let backup = temp_skill_update_dir(&format!("backup-{}", source.skill_name));
    if tmp.exists() {
        fs::remove_dir_all(&tmp).with_context(|| format!("failed to clear {}", tmp.display()))?;
    }
    if backup.exists() {
        fs::remove_dir_all(&backup)
            .with_context(|| format!("failed to clear {}", backup.display()))?;
    }
    fs::create_dir_all(&tmp).with_context(|| format!("failed to create {}", tmp.display()))?;
    let result = updater.install(source, &tmp).and_then(|()| {
        if !tmp.join(SKILL_ENTRYPOINT).is_file() {
            bail!(
                "updated remote skill '{}' did not contain {} at {}",
                source.skill_name,
                SKILL_ENTRYPOINT,
                source.skill_path
            );
        }
        record_install_metadata(&tmp, "remote")?;
        if destination.exists() {
            fs::rename(destination, &backup).with_context(|| {
                format!(
                    "failed to move current skill {} -> {}",
                    destination.display(),
                    backup.display()
                )
            })?;
        }
        if let Err(error) = fs::rename(&tmp, destination).with_context(|| {
            format!(
                "failed to install updated skill {} -> {}",
                tmp.display(),
                destination.display()
            )
        }) {
            if backup.exists() {
                let _ = fs::rename(&backup, destination);
            }
            return Err(error);
        }
        if backup.exists() {
            fs::remove_dir_all(&backup)
                .with_context(|| format!("failed to remove {}", backup.display()))?;
        }
        Ok(())
    });
    if result.is_err() {
        let _ = fs::remove_dir_all(&tmp);
        let _ = fs::remove_dir_all(&backup);
    }
    result
}

fn temp_skill_update_dir(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "holon-skill-update-{}-{}-{}",
        std::process::id(),
        label,
        uuid::Uuid::new_v4()
    ))
}

fn refresh_updated_lock_entry(
    lock: &mut serde_json::Value,
    name: &str,
    latest_hash: &str,
) -> Result<()> {
    let skills = lock_skills_mut(lock)?;
    let Some(entry) = skills.get_mut(name).and_then(|value| value.as_object_mut()) else {
        bail!("skill lock entry '{name}' disappeared during update");
    };
    entry.insert(
        "skillFolderHash".into(),
        serde_json::Value::String(latest_hash.into()),
    );
    entry.insert(
        "updatedAt".into(),
        serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
    );
    Ok(())
}

fn hash_skill_folder(skill_dir: &Path) -> Result<String> {
    let mut files = Vec::new();
    collect_skill_files(skill_dir, skill_dir, &mut files)?;
    let mut hasher = Sha256::new();
    for (relative_path, path) in files {
        hasher.update(relative_path.to_string_lossy().as_bytes());
        hasher.update([0]);
        hasher
            .update(fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?);
        hasher.update([0]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn collect_skill_files(root: &Path, dir: &Path, files: &mut Vec<(PathBuf, PathBuf)>) -> Result<()> {
    let mut entries = fs::read_dir(dir)
        .with_context(|| format!("failed to read {}", dir.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let file_type = entry.file_type()?;
        let path = entry.path();
        if file_type.is_dir() {
            collect_skill_files(root, &path, files)?;
        } else if file_type.is_file() {
            let relative_path = path
                .strip_prefix(root)
                .with_context(|| {
                    format!(
                        "failed to compute relative path for {} under {}",
                        path.display(),
                        root.display()
                    )
                })?
                .to_path_buf();
            if relative_path == Path::new(INSTALL_METADATA_FILENAME) {
                continue;
            }
            files.push((relative_path, path));
        }
    }
    Ok(())
}

fn sync_skill_lock_entry(user_home: &Path, name: &str) -> Result<()> {
    validate_skill_name(name)?;
    let destination = user_library_skills_root(user_home).join(name);
    let mut lock = read_skill_lock(user_home)?;
    upsert_skill_lock_entry(&mut lock, name, &destination)?;
    write_skill_lock(user_home, &lock)
}

fn remove_skill_lock_entry(user_home: &Path, name: &str) -> Result<()> {
    let mut lock = read_skill_lock(user_home)?;
    remove_skill_lock_name(&mut lock, name);
    write_skill_lock(user_home, &lock)
}

fn skill_lock_path(user_home: &Path) -> PathBuf {
    user_home.join(SKILL_LOCK_FILENAME)
}

fn read_skill_lock(user_home: &Path) -> Result<serde_json::Value> {
    let path = skill_lock_path(user_home);
    if !path.exists() {
        return Ok(serde_json::json!({
            "version": 1,
            "skills": {},
        }));
    }
    let value: serde_json::Value = serde_json::from_slice(
        &fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?,
    )
    .with_context(|| format!("failed to parse {}", path.display()))?;
    match value {
        serde_json::Value::Object(_) => Ok(value),
        _ => bail!("skill lock {} must contain a JSON object", path.display()),
    }
}

fn write_skill_lock(user_home: &Path, lock: &serde_json::Value) -> Result<()> {
    let path = skill_lock_path(user_home);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(&path, serde_json::to_vec_pretty(lock)?)
        .with_context(|| format!("failed to write {}", path.display()))
}

fn lock_skills_mut(
    lock: &mut serde_json::Value,
) -> Result<&mut serde_json::Map<String, serde_json::Value>> {
    let object = lock
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("skill lock must contain a JSON object"))?;
    let skills = object
        .entry("skills")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    if !skills.is_object() {
        *skills = serde_json::Value::Object(serde_json::Map::new());
    }
    Ok(skills.as_object_mut().expect("skills is an object"))
}

fn lock_skills(lock: &serde_json::Value) -> Option<&serde_json::Map<String, serde_json::Value>> {
    lock.get("skills").and_then(|value| value.as_object())
}

fn upsert_skill_lock_entry(
    lock: &mut serde_json::Value,
    name: &str,
    skill_dir: &Path,
) -> Result<()> {
    let metadata = fs::symlink_metadata(skill_dir)
        .with_context(|| format!("failed to inspect {}", skill_dir.display()))?;
    let path = if metadata.file_type().is_symlink() {
        fs::read_link(skill_dir)
            .with_context(|| format!("failed to read {}", skill_dir.display()))?
    } else {
        skill_dir.to_path_buf()
    };
    let source = if metadata.file_type().is_symlink() {
        "linked"
    } else if read_install_metadata(skill_dir)
        .as_ref()
        .is_some_and(|metadata| metadata.install_mode == "builtin")
    {
        "builtin"
    } else {
        "local"
    };
    let skills = lock_skills_mut(lock)?;
    let mut entry = skills
        .remove(name)
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    entry.insert("name".into(), serde_json::Value::String(name.into()));
    entry.insert(
        "path".into(),
        serde_json::Value::String(path.to_string_lossy().to_string()),
    );
    entry.insert("source".into(), serde_json::Value::String(source.into()));
    entry.insert(
        "updated_at".into(),
        serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
    );
    skills.insert(name.into(), serde_json::Value::Object(entry));
    Ok(())
}

fn remove_skill_lock_name(lock: &mut serde_json::Value, name: &str) {
    if let Ok(skills) = lock_skills_mut(lock) {
        skills.remove(name);
    }
}

fn locked_skill_names(lock: &serde_json::Value, name_filter: Option<&str>) -> BTreeSet<String> {
    lock_skills(lock)
        .into_iter()
        .flat_map(|skills| skills.keys())
        .filter(|name| name_filter.is_none_or(|filter| name.as_str() == filter))
        .cloned()
        .collect()
}

fn library_skill_dirs(
    skills_root: &Path,
    name_filter: Option<&str>,
) -> Result<BTreeMap<String, PathBuf>> {
    let mut skills = BTreeMap::new();
    let read_dir = match fs::read_dir(skills_root) {
        Ok(read_dir) => read_dir,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(skills),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", skills_root.display()))
        }
    };
    for child in read_dir {
        let child = child?;
        let file_type = child.file_type()?;
        if !file_type.is_dir() && !file_type.is_symlink() {
            continue;
        }
        let name = child.file_name().to_string_lossy().to_string();
        if name_filter.is_some_and(|filter| name != filter) {
            continue;
        }
        skills.insert(name, child.path());
    }
    Ok(skills)
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct InstalledSkillView {
    #[serde(flatten)]
    pub catalog: SkillCatalogEntry,
    pub install_mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_target: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

pub fn list_installed_skills(agent_home: &Path) -> Result<Vec<InstalledSkillView>> {
    let mut entries = Vec::new();
    for skills_root in existing_skill_roots(Some(agent_home), &SKILL_ROOT_SUFFIXES) {
        let root_id = skill_root_id_for_scope(SkillScope::Agent, &skills_root);
        let read_dir = match fs::read_dir(&skills_root) {
            Ok(read_dir) => read_dir,
            Err(error) => {
                warn!(
                    "skipping unreadable installed skill root {}: {error}",
                    skills_root.display()
                );
                continue;
            }
        };
        for child in read_dir {
            let child = child?;
            let file_type = child.file_type()?;
            if !file_type.is_dir() && !file_type.is_symlink() {
                continue;
            }
            let skill_name = child.file_name().to_string_lossy().to_string();
            let skill_path = child.path().join(SKILL_ENTRYPOINT);
            let catalog = if skill_path.is_file() {
                let content = fs::read_to_string(&skill_path).with_context(|| {
                    format!("failed to read installed skill {}", skill_path.display())
                })?;
                let parsed = parse_skill_metadata(&content);
                SkillCatalogEntry {
                    skill_id: format!("{root_id}:{skill_name}"),
                    root_id: root_id.clone(),
                    skill_dir: skill_name.clone(),
                    name: parsed.name.unwrap_or_else(|| skill_name.clone()),
                    description: parsed
                        .description
                        .unwrap_or_else(|| first_body_paragraph(&content)),
                    path: skill_path,
                    scope: SkillScope::Agent,
                }
            } else {
                SkillCatalogEntry {
                    skill_id: format!("{root_id}:{skill_name}"),
                    root_id: root_id.clone(),
                    skill_dir: skill_name.clone(),
                    name: skill_name.clone(),
                    description: String::new(),
                    path: skill_path,
                    scope: SkillScope::Agent,
                }
            };
            entries.push(installed_skill_view(catalog)?);
        }
    }
    entries.sort_by(|left, right| {
        left.catalog
            .name
            .cmp(&right.catalog.name)
            .then_with(|| left.catalog.skill_id.cmp(&right.catalog.skill_id))
            .then_with(|| left.catalog.path.cmp(&right.catalog.path))
    });
    Ok(entries)
}

fn installed_skill_view(catalog: SkillCatalogEntry) -> Result<InstalledSkillView> {
    let skill_dir = catalog
        .path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("skill path {} has no parent", catalog.path.display()))?;
    let metadata = fs::symlink_metadata(skill_dir)
        .with_context(|| format!("failed to inspect {}", skill_dir.display()))?;
    let link_target = if metadata.file_type().is_symlink() {
        Some(
            fs::read_link(skill_dir)
                .with_context(|| format!("failed to read link {}", skill_dir.display()))?,
        )
    } else {
        None
    };
    let install_metadata = if link_target.is_none() {
        read_install_metadata(skill_dir)
    } else {
        None
    };
    let warning = link_target
        .as_ref()
        .and_then(|target| {
            let target_path = if target.is_absolute() {
                target.clone()
            } else {
                skill_dir
                    .parent()
                    .unwrap_or_else(|| Path::new(""))
                    .join(target)
            };
            if !target_path.exists() {
                Some(format!("link target does not exist: {}", target.display()))
            } else if !target_path.join(SKILL_ENTRYPOINT).is_file() {
                Some(format!(
                    "link target does not contain {}: {}",
                    SKILL_ENTRYPOINT,
                    target.display()
                ))
            } else {
                None
            }
        })
        .or_else(|| {
            if !catalog.path.is_file() {
                Some(format!(
                    "installed skill does not contain {}: {}",
                    SKILL_ENTRYPOINT,
                    skill_dir.display()
                ))
            } else {
                None
            }
        });
    Ok(InstalledSkillView {
        install_mode: if link_target.is_some() {
            "linked".into()
        } else if matches!(
            install_metadata
                .as_ref()
                .map(|metadata| metadata.install_mode.as_str()),
            Some("builtin")
        ) {
            "builtin".into()
        } else if matches!(
            install_metadata
                .as_ref()
                .map(|metadata| metadata.install_mode.as_str()),
            Some("copied")
        ) {
            "copied".into()
        } else if crate::agent_template::builtin_skill_names()
            .contains(&catalog.skill_id.trim_start_matches("agent:"))
        {
            "builtin".into()
        } else {
            "copied".into()
        },
        catalog,
        link_target,
        warning,
    })
}

#[cfg(all(test, unix))]
fn create_symlink(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

#[cfg(all(test, windows))]
fn create_symlink(src: &Path, dst: &Path) -> std::io::Result<()> {
    if src.is_dir() {
        std::os::windows::fs::symlink_dir(src, dst)
    } else {
        std::os::windows::fs::symlink_file(src, dst)
    }
}
#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use tempfile::tempdir;

    use super::*;
    use crate::types::{SkillActivationSource, SkillActivationState};

    struct FakeSkillRemoteUpdater {
        contents: HashMap<String, String>,
        latest_hashes: HashMap<String, String>,
    }

    impl SkillRemoteUpdater for FakeSkillRemoteUpdater {
        fn latest_hash(&self, source: &LockedRemoteSkillSource) -> Result<String> {
            self.latest_hashes
                .get(&source.skill_name)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("missing fake hash for {}", source.skill_name))
        }

        fn install(&self, source: &LockedRemoteSkillSource, destination: &Path) -> Result<()> {
            fs::create_dir_all(destination)
                .with_context(|| format!("failed to create {}", destination.display()))?;
            fs::write(
                destination.join(SKILL_ENTRYPOINT),
                self.contents
                    .get(&source.skill_name)
                    .cloned()
                    .unwrap_or_else(|| format!("# {}", source.skill_name)),
            )?;
            Ok(())
        }
    }

    fn write_v3_lock_entry(user_home: &Path, name: &str, hash: &str, extra: serde_json::Value) {
        fs::create_dir_all(user_home.join(".agents")).unwrap();
        let mut entry = serde_json::json!({
            "name": name,
            "source": "vercel-labs/agent-skills",
            "sourceType": "github",
            "sourceUrl": "https://github.com/vercel-labs/agent-skills",
            "skillPath": format!("skills/{name}"),
            "skillFolderHash": hash,
            "ref": "",
            "installedAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-01-01T00:00:00Z"
        });
        if let (Some(entry), Some(extra)) = (entry.as_object_mut(), extra.as_object()) {
            for (key, value) in extra {
                entry.insert(key.clone(), value.clone());
            }
        }
        fs::write(
            user_home.join(SKILL_LOCK_FILENAME),
            serde_json::to_vec_pretty(&serde_json::json!({
                "version": 3,
                "skills": {
                    name: entry,
                }
            }))
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn agent_skill_discovery_merges_compatible_roots() {
        let dir = tempdir().unwrap();
        let base = dir.path();
        let first_root = base.join("skills");
        let fallback_root = base.join(".codex/skills");
        fs::create_dir_all(first_root.join("alpha")).unwrap();
        fs::create_dir_all(fallback_root.join("beta")).unwrap();
        fs::write(
            first_root.join("alpha").join(SKILL_ENTRYPOINT),
            "---\nname: alpha\ndescription: first\n---\nbody",
        )
        .unwrap();
        fs::write(
            fallback_root.join("beta").join(SKILL_ENTRYPOINT),
            "---\nname: beta\ndescription: second\n---\nbody",
        )
        .unwrap();

        let view =
            load_skills_runtime_view(SkillVisibility::NonDefaultAgent, None, base, None, &[])
                .unwrap();

        assert_eq!(view.discovered_roots.len(), 2);
        let ids = view
            .discoverable_skills
            .iter()
            .map(|skill| skill.skill_id.as_str())
            .collect::<Vec<_>>();
        assert!(ids.iter().all(|id| id.starts_with("agent_home:")));
        let mut names = view
            .discoverable_skills
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, vec!["alpha", "beta"]);
    }

    #[test]
    fn default_agent_sees_user_scope_but_non_default_does_not() {
        let dir = tempdir().unwrap();
        let user_home = dir.path().join("user");
        let agent_home = dir.path().join("agent");
        fs::create_dir_all(user_home.join(".agents/skills/ghx")).unwrap();
        fs::create_dir_all(agent_home.join("skills/local")).unwrap();
        fs::write(
            user_home.join(".agents/skills/ghx").join(SKILL_ENTRYPOINT),
            "---\nname: ghx\ndescription: user skill\n---\nbody",
        )
        .unwrap();
        fs::write(
            agent_home.join("skills/local").join(SKILL_ENTRYPOINT),
            "---\nname: local\ndescription: agent skill\n---\nbody",
        )
        .unwrap();

        let default_view = load_skills_runtime_view(
            SkillVisibility::DefaultAgent,
            Some(&user_home),
            &agent_home,
            None,
            &[],
        )
        .unwrap();
        let non_default_view = load_skills_runtime_view(
            SkillVisibility::NonDefaultAgent,
            Some(&user_home),
            &agent_home,
            None,
            &[],
        )
        .unwrap();

        assert!(default_view
            .discoverable_skills
            .iter()
            .any(|entry| entry.scope == SkillScope::UserGlobal));
        assert!(non_default_view
            .discoverable_skills
            .iter()
            .all(|entry| entry.scope != SkillScope::UserGlobal));
    }

    #[test]
    fn same_name_skills_keep_highest_precedence_catalog_and_active_record() {
        let dir = tempdir().unwrap();
        let user_home = dir.path().join("user");
        let agent_home = dir.path().join("agent");
        let workspace = dir.path().join("workspace");
        let user_skill = user_home.join(".agents/skills/ghx");
        let agent_skill = agent_home.join("skills/ghx");
        let workspace_skill = workspace.join(".agents/skills/ghx");
        for (skill_dir, description) in [
            (&user_skill, "user skill"),
            (&agent_skill, "agent skill"),
            (&workspace_skill, "workspace skill"),
        ] {
            fs::create_dir_all(skill_dir).unwrap();
            fs::write(
                skill_dir.join(SKILL_ENTRYPOINT),
                format!("---\nname: ghx\ndescription: {description}\n---\nbody"),
            )
            .unwrap();
        }

        let agent_skill_root = agent_home.join("skills");
        let agent_ghx_skill_id = format!(
            "{}:ghx",
            skill_root_id_for_scope(SkillScope::Agent, &agent_skill_root)
        );
        let workspace_skill_root = workspace.join(".agents/skills");
        let workspace_ghx_skill_id = format!(
            "{}:ghx",
            skill_root_id_for_scope(SkillScope::Workspace, &workspace_skill_root)
        );

        let view = load_skills_runtime_view(
            SkillVisibility::DefaultAgent,
            Some(&user_home),
            &agent_home,
            Some(&workspace),
            &[
                ActiveSkillRecord {
                    skill_id: workspace_ghx_skill_id,
                    name: "ghx".into(),
                    path: workspace_skill.join(SKILL_ENTRYPOINT),
                    scope: SkillScope::Workspace,
                    agent_id: "default".into(),
                    activation_source: SkillActivationSource::ImplicitFromCatalog,
                    activation_state: SkillActivationState::SessionActive,
                    activated_at_turn: 1,
                },
                ActiveSkillRecord {
                    skill_id: agent_ghx_skill_id,
                    name: "ghx".into(),
                    path: agent_skill.join(SKILL_ENTRYPOINT),
                    scope: SkillScope::Agent,
                    agent_id: "default".into(),
                    activation_source: SkillActivationSource::ImplicitFromCatalog,
                    activation_state: SkillActivationState::SessionActive,
                    activated_at_turn: 2,
                },
            ],
        )
        .unwrap();

        assert_eq!(view.discoverable_skills.len(), 1);
        assert!(view.discoverable_skills[0]
            .skill_id
            .starts_with("agent_home:"));
        assert!(view.discoverable_skills[0].skill_id.ends_with(":ghx"));
        assert_eq!(view.discoverable_skills[0].description, "agent skill");
        assert_eq!(view.active_skills.len(), 1);
        assert_eq!(
            view.active_skills[0].skill_id,
            view.discoverable_skills[0].skill_id
        );
    }

    #[test]
    fn same_name_same_scope_skills_use_stable_tie_breaker() {
        let dir = tempdir().unwrap();
        let later_id_path = dir.path().join("z-skill").join(SKILL_ENTRYPOINT);
        let earlier_id_path = dir.path().join("a-skill").join(SKILL_ENTRYPOINT);
        let same_id_later_path = dir.path().join("z-root/skill").join(SKILL_ENTRYPOINT);
        let same_id_earlier_path = dir.path().join("a-root/skill").join(SKILL_ENTRYPOINT);

        let selected = retain_highest_precedence_skills(vec![
            SkillCatalogEntry {
                skill_id: "agent:z-skill".into(),
                root_id: "agent_home:z-root".into(),
                skill_dir: "z-skill".into(),
                name: "shared".into(),
                description: "later id".into(),
                path: later_id_path,
                scope: SkillScope::Agent,
            },
            SkillCatalogEntry {
                skill_id: "agent:a-skill".into(),
                root_id: "agent_home:a-root".into(),
                skill_dir: "a-skill".into(),
                name: "shared".into(),
                description: "earlier id".into(),
                path: earlier_id_path,
                scope: SkillScope::Agent,
            },
            SkillCatalogEntry {
                skill_id: "workspace:skill".into(),
                root_id: "workspace:z-root".into(),
                skill_dir: "skill".into(),
                name: "same-id".into(),
                description: "later path".into(),
                path: same_id_later_path,
                scope: SkillScope::Workspace,
            },
            SkillCatalogEntry {
                skill_id: "workspace:skill".into(),
                root_id: "workspace:a-root".into(),
                skill_dir: "skill".into(),
                name: "same-id".into(),
                description: "earlier path".into(),
                path: same_id_earlier_path,
                scope: SkillScope::Workspace,
            },
        ]);

        let by_name = selected
            .iter()
            .map(|entry| (entry.name.as_str(), entry.description.as_str()))
            .collect::<std::collections::BTreeMap<_, _>>();

        assert_eq!(by_name.get("shared"), Some(&"earlier id"));
        assert_eq!(by_name.get("same-id"), Some(&"earlier path"));
    }

    #[test]
    fn frontmatter_description_wins_over_body() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("skills/demo");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join(SKILL_ENTRYPOINT),
            "---\nname: demo\ndescription: frontmatter description\n---\n\nParagraph description",
        )
        .unwrap();

        let view = load_skills_runtime_view(
            SkillVisibility::NonDefaultAgent,
            None,
            dir.path(),
            None,
            &[],
        )
        .unwrap();

        assert_eq!(
            view.discoverable_skills[0].description,
            "frontmatter description"
        );
    }

    #[test]
    fn filters_active_skills_to_visible_catalog() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("skills/demo");
        fs::create_dir_all(&root).unwrap();
        let skill_path = root.join(SKILL_ENTRYPOINT);
        fs::write(
            &skill_path,
            "---\nname: demo\ndescription: demo skill\n---\nbody",
        )
        .unwrap();

        let agent_skill_root = dir.path().join("skills");
        let agent_demo_skill_id = format!(
            "{}:demo",
            skill_root_id_for_scope(SkillScope::Agent, &agent_skill_root)
        );

        let view = load_skills_runtime_view(
            SkillVisibility::NonDefaultAgent,
            None,
            dir.path(),
            None,
            &[ActiveSkillRecord {
                skill_id: agent_demo_skill_id,
                name: "demo".into(),
                path: skill_path,
                scope: SkillScope::Agent,
                agent_id: "default".into(),
                activation_source: SkillActivationSource::ImplicitFromCatalog,
                activation_state: SkillActivationState::TurnActive,
                activated_at_turn: 1,
            }],
        )
        .unwrap();

        assert_eq!(view.active_skills.len(), 1);
    }

    #[test]
    fn catalog_runtime_view_filters_active_skills_to_effective_catalog_snapshot() {
        let visible_path = PathBuf::from("/agent/skills/demo/SKILL.md");
        let view = skills_runtime_view_from_catalog(
            vec![SkillCatalogEntry {
                skill_id: "agent_home:test-root:demo".into(),
                root_id: "agent_home:test-root".into(),
                skill_dir: "demo".into(),
                name: "demo".into(),
                description: "visible".into(),
                path: visible_path.clone(),
                scope: SkillScope::Agent,
            }],
            &[skill_root_registration(
                SkillRootSourceKind::AgentHome,
                Some("default".into()),
                PathBuf::from("/agent/skills"),
            )],
            &[
                ActiveSkillRecord {
                    skill_id: "agent_home:test-root:demo".into(),
                    name: "demo".into(),
                    path: visible_path,
                    scope: SkillScope::Agent,
                    agent_id: "default".into(),
                    activation_source: SkillActivationSource::ImplicitFromCatalog,
                    activation_state: SkillActivationState::SessionActive,
                    activated_at_turn: 1,
                },
                ActiveSkillRecord {
                    skill_id: "workspace:stale".into(),
                    name: "stale".into(),
                    path: PathBuf::from("/workspace/.agents/skills/stale/SKILL.md"),
                    scope: SkillScope::Workspace,
                    agent_id: "default".into(),
                    activation_source: SkillActivationSource::ImplicitFromCatalog,
                    activation_state: SkillActivationState::SessionActive,
                    activated_at_turn: 1,
                },
            ],
        );

        assert_eq!(view.discoverable_skills.len(), 1);
        assert_eq!(view.active_skills.len(), 1);
        assert_eq!(view.active_skills[0].skill_id, "agent_home:test-root:demo");
    }

    #[test]
    fn skips_unreadable_skill_entries_without_failing_catalog_load() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("skills");
        fs::create_dir_all(root.join("good")).unwrap();
        fs::create_dir_all(root.join("bad")).unwrap();
        fs::write(
            root.join("good").join(SKILL_ENTRYPOINT),
            "---\nname: good\ndescription: good skill\n---\nbody",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::write(root.join("bad").join(SKILL_ENTRYPOINT), "secret").unwrap();
            let mut perms = fs::metadata(root.join("bad").join(SKILL_ENTRYPOINT))
                .unwrap()
                .permissions();
            perms.set_mode(0o000);
            fs::set_permissions(root.join("bad").join(SKILL_ENTRYPOINT), perms).unwrap();
        }

        let view = load_skills_runtime_view(
            SkillVisibility::NonDefaultAgent,
            None,
            dir.path(),
            None,
            &[],
        )
        .unwrap();

        assert_eq!(view.discoverable_skills.len(), 1);
        assert_eq!(view.discoverable_skills[0].name, "good");
    }

    #[test]
    fn named_install_links_unique_user_global_skill_for_non_default_agent() {
        let dir = tempdir().unwrap();
        let user_home = dir.path().join("user");
        let agent_home = dir.path().join("agent");
        let source = user_home.join(".agents/skills/global-demo");
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join(SKILL_ENTRYPOINT),
            "---\nname: global-demo\ndescription: global skill\n---\nbody",
        )
        .unwrap();

        let installed = install_skill_with_user_home(
            &agent_home,
            Some(&user_home),
            &crate::types::SkillInstallKind::Named {
                name: "global-demo".into(),
                mode: SkillInstallMode::Linked,
            },
        )
        .unwrap();

        assert_eq!(installed, "global-demo");
        let destination = agent_home.join("skills/global-demo");
        assert!(fs::symlink_metadata(&destination)
            .unwrap()
            .file_type()
            .is_symlink());
        let view = load_skills_runtime_view(
            SkillVisibility::NonDefaultAgent,
            Some(&user_home),
            &agent_home,
            None,
            &[],
        )
        .unwrap();
        assert_eq!(view.discoverable_skills.len(), 1);
        assert_eq!(view.discoverable_skills[0].scope, SkillScope::Agent);
        assert_eq!(view.discoverable_skills[0].name, "global-demo");
    }

    #[test]
    fn local_install_can_copy_skill_directory() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("source/demo");
        let agent_home = dir.path().join("agent");
        fs::create_dir_all(source.join("references")).unwrap();
        fs::write(
            source.join(SKILL_ENTRYPOINT),
            "---\nname: demo\ndescription: copied skill\n---\nbody",
        )
        .unwrap();
        fs::write(source.join("references/notes.md"), "notes").unwrap();

        let installed = install_skill_with_user_home(
            &agent_home,
            None,
            &crate::types::SkillInstallKind::Local {
                path: source.clone(),
                mode: SkillInstallMode::Copied,
            },
        )
        .unwrap();

        assert_eq!(installed, "demo");
        let destination = agent_home.join("skills/demo");
        assert!(destination.join(SKILL_ENTRYPOINT).is_file());
        assert!(destination.join("references/notes.md").is_file());
        assert!(!fs::symlink_metadata(&destination)
            .unwrap()
            .file_type()
            .is_symlink());
        let installed = list_installed_skills(&agent_home).unwrap();
        assert_eq!(installed[0].install_mode, "copied");
        assert!(installed[0].link_target.is_none());
    }

    #[test]
    fn copied_local_skill_named_like_builtin_reports_copied_mode() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("source/ghx");
        let agent_home = dir.path().join("agent");
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join(SKILL_ENTRYPOINT),
            "---\nname: ghx\ndescription: local ghx\n---\nbody",
        )
        .unwrap();

        install_skill_with_user_home(
            &agent_home,
            None,
            &crate::types::SkillInstallKind::Local {
                path: source,
                mode: SkillInstallMode::Copied,
            },
        )
        .unwrap();

        let installed = list_installed_skills(&agent_home).unwrap();
        assert_eq!(installed[0].catalog.name, "ghx");
        assert_eq!(installed[0].install_mode, "copied");
    }

    #[test]
    fn copy_mode_rejects_symlinks_inside_skill_directory() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("source/demo");
        let agent_home = dir.path().join("agent");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join(SKILL_ENTRYPOINT), "# Demo").unwrap();
        let target = dir.path().join("target.txt");
        fs::write(&target, "target").unwrap();
        create_symlink(&target, &source.join("linked.txt")).unwrap();

        let error = install_skill_with_user_home(
            &agent_home,
            None,
            &crate::types::SkillInstallKind::Local {
                path: source,
                mode: SkillInstallMode::Copied,
            },
        )
        .unwrap_err();

        assert!(format!("{error:?}").contains("does not support symlinks"));
        assert!(!agent_home.join("skills/demo").exists());
    }

    #[test]
    fn list_installed_skills_reports_builtin_mode() {
        let dir = tempdir().unwrap();
        let agent_home = dir.path().join("agent");
        install_skill_with_user_home(
            &agent_home,
            None,
            &crate::types::SkillInstallKind::Builtin { name: "ghx".into() },
        )
        .unwrap();

        let installed = list_installed_skills(&agent_home).unwrap();

        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].catalog.name, "ghx");
        assert_eq!(installed[0].install_mode, "builtin");
        assert!(installed[0].link_target.is_none());
        assert!(installed[0].warning.is_none());
    }

    #[test]
    fn list_installed_skills_merges_compatible_agent_roots() {
        let dir = tempdir().unwrap();
        let agent_home = dir.path().join("agent");
        for (root, name) in [("skills", "alpha"), (".codex/skills", "beta")] {
            let skill_dir = agent_home.join(root).join(name);
            fs::create_dir_all(&skill_dir).unwrap();
            fs::write(
                skill_dir.join(SKILL_ENTRYPOINT),
                format!("---\nname: {name}\ndescription: installed\n---\nbody"),
            )
            .unwrap();
        }

        let installed = list_installed_skills(&agent_home).unwrap();

        let names = installed
            .iter()
            .map(|skill| skill.catalog.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["alpha", "beta"]);
    }

    #[test]
    fn install_existing_destination_returns_structured_conflict() {
        let dir = tempdir().unwrap();
        let agent_home = dir.path().join("agent");
        let destination = agent_home.join("skills/ghx");
        fs::create_dir_all(&destination).unwrap();
        fs::write(destination.join(SKILL_ENTRYPOINT), "# Existing ghx").unwrap();

        let error = install_skill_with_user_home(
            &agent_home,
            None,
            &crate::types::SkillInstallKind::Builtin { name: "ghx".into() },
        )
        .unwrap_err();
        let conflict = error
            .downcast_ref::<SkillInstallConflict>()
            .expect("existing destination should return a structured conflict");

        assert_eq!(conflict.skill_name, "ghx");
        assert_eq!(conflict.destination, destination);
    }

    #[test]
    fn install_existing_broken_symlink_returns_structured_conflict() {
        let dir = tempdir().unwrap();
        let agent_home = dir.path().join("agent");
        let skills_root = agent_home.join("skills");
        fs::create_dir_all(&skills_root).unwrap();
        let destination = skills_root.join("demo");
        create_symlink(&dir.path().join("missing-demo"), &destination).unwrap();
        let source = dir.path().join("source/demo");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join(SKILL_ENTRYPOINT), "# Demo").unwrap();

        let error = install_skill_with_user_home(
            &agent_home,
            None,
            &crate::types::SkillInstallKind::Local {
                path: source,
                mode: SkillInstallMode::Linked,
            },
        )
        .unwrap_err();
        let conflict = error
            .downcast_ref::<SkillInstallConflict>()
            .expect("dangling destination symlink should return a structured conflict");

        assert_eq!(conflict.skill_name, "demo");
        assert_eq!(conflict.destination, destination);
    }

    #[test]
    fn uninstall_linked_skill_removes_only_agent_local_link() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("source/demo");
        let agent_home = dir.path().join("agent");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join(SKILL_ENTRYPOINT), "# Demo").unwrap();
        install_skill_with_user_home(
            &agent_home,
            None,
            &crate::types::SkillInstallKind::Local {
                path: source.clone(),
                mode: SkillInstallMode::Linked,
            },
        )
        .unwrap();

        uninstall_skill(&agent_home, "demo").unwrap();

        assert!(source.join(SKILL_ENTRYPOINT).is_file());
        assert!(!agent_home.join("skills/demo").exists());
    }

    #[test]
    fn uninstall_skill_searches_compatible_agent_roots() {
        let dir = tempdir().unwrap();
        let agent_home = dir.path().join("agent");
        let source = dir.path().join("source/demo");
        let legacy_destination = agent_home.join(".codex/skills/demo");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join(SKILL_ENTRYPOINT), "# Demo").unwrap();
        fs::create_dir_all(legacy_destination.parent().unwrap()).unwrap();
        create_symlink(&source, &legacy_destination).unwrap();

        uninstall_skill(&agent_home, "demo").unwrap();

        assert!(source.join(SKILL_ENTRYPOINT).is_file());
        assert!(!legacy_destination.exists());
    }

    #[test]
    fn uninstall_skill_rejects_duplicate_installs_across_roots() {
        let dir = tempdir().unwrap();
        let agent_home = dir.path().join("agent");
        for root in ["skills", ".codex/skills"] {
            let skill_dir = agent_home.join(root).join("demo");
            fs::create_dir_all(&skill_dir).unwrap();
            fs::write(skill_dir.join(SKILL_ENTRYPOINT), "# Demo").unwrap();
        }

        let error = uninstall_skill(&agent_home, "demo").unwrap_err();

        assert!(error.to_string().contains("multiple agent skill roots"));
        assert!(agent_home.join("skills/demo").exists());
        assert!(agent_home.join(".codex/skills/demo").exists());
    }

    #[test]
    fn list_installed_skills_reports_link_mode_and_broken_link_warning() {
        let dir = tempdir().unwrap();
        let agent_home = dir.path().join("agent");
        let skills_root = agent_home.join("skills");
        fs::create_dir_all(&skills_root).unwrap();
        let missing = dir.path().join("missing-skill");
        create_symlink(&missing, &skills_root.join("missing")).unwrap();

        let installed = list_installed_skills(&agent_home).unwrap();

        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].catalog.name, "missing");
        assert_eq!(installed[0].install_mode, "linked");
        assert_eq!(installed[0].link_target.as_deref(), Some(missing.as_path()));
        assert!(installed[0]
            .warning
            .as_deref()
            .unwrap_or_default()
            .contains("link target does not exist"));
    }

    #[test]
    fn named_install_prefers_agents_user_global_skill_over_compat_roots() {
        let dir = tempdir().unwrap();
        let user_home = dir.path().join("user");
        let agent_home = dir.path().join("agent");
        for (root, description) in [(".agents/skills", "preferred"), (".codex/skills", "compat")] {
            let source = user_home.join(root).join("demo");
            fs::create_dir_all(&source).unwrap();
            fs::write(
                source.join(SKILL_ENTRYPOINT),
                format!("---\nname: demo\ndescription: {description}\n---\nbody"),
            )
            .unwrap();
        }

        let installed = install_skill_with_user_home(
            &agent_home,
            Some(&user_home),
            &crate::types::SkillInstallKind::Named {
                name: "demo".into(),
                mode: SkillInstallMode::Linked,
            },
        )
        .unwrap();

        assert_eq!(installed, "demo");
        assert_eq!(
            fs::read_link(agent_home.join("skills/demo")).unwrap(),
            user_home.join(".agents/skills/demo")
        );
    }

    #[test]
    fn remote_install_surfaces_installer_errors() {
        let dir = tempdir().unwrap();
        let error = install_skill_with_user_home_and_remote_installer(
            &dir.path().join("agent"),
            Some(&dir.path().join("user")),
            &crate::types::SkillInstallKind::Remote {
                package: "vercel-labs/agent-skills".into(),
                skill: Some("demo".into()),
                mode: SkillInstallMode::Linked,
            },
            &UnavailableRemoteSkillInstaller,
        )
        .unwrap_err();

        assert!(error.to_string().contains("remote installer unavailable"));
    }

    #[test]
    fn remote_package_validation_rejects_option_like_and_whitespace_refs() {
        for package in [
            "", " ", "--help", "-x", " demo", "demo ", "foo bar", "foo\nbar",
        ] {
            assert!(
                validate_remote_package(package).is_err(),
                "package should be rejected: {package:?}"
            );
        }
    }

    #[test]
    fn remote_package_validation_accepts_common_package_refs() {
        for package in ["vercel-labs/agent-skills", "@scope/package", "agent-skills"] {
            validate_remote_package(package).unwrap();
        }
    }

    #[test]
    fn remote_skill_source_parses_github_tree_url() {
        let source = RemoteSkillSource::parse(
            "https://github.com/user/repo/tree/main/skills/my-skill",
            None,
        )
        .unwrap();

        assert_eq!(
            source,
            RemoteSkillSource {
                owner: "user".into(),
                repo: "repo".into(),
                reference: "main".into(),
                path: "skills/my-skill".into(),
                skill_name: "my-skill".into(),
            }
        );
    }

    #[test]
    fn remote_skill_source_parses_owner_repo_skill_package() {
        let source = RemoteSkillSource::parse("vercel-labs/agent-skills@pr-review", None).unwrap();

        assert_eq!(source.owner, "vercel-labs");
        assert_eq!(source.repo, "agent-skills");
        assert_eq!(source.reference, "");
        assert_eq!(source.path, "skills/pr-review");
        assert_eq!(source.skill_name, "pr-review");
    }

    #[test]
    fn remote_install_adds_global_skill_non_interactively_then_links_to_agent() {
        let dir = tempdir().unwrap();
        let user_home = dir.path().join("user");
        let agent_home = dir.path().join("agent");
        let source = user_home.join(".agents/skills/demo");
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join(SKILL_ENTRYPOINT),
            "---\nname: demo\ndescription: remote\n---\nbody",
        )
        .unwrap();
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let installer = RecordingRemoteSkillInstaller {
            calls: calls.clone(),
        };

        let installed = install_skill_with_user_home_and_remote_installer(
            &agent_home,
            Some(&user_home),
            &crate::types::SkillInstallKind::Remote {
                package: "vercel-labs/agent-skills".into(),
                skill: Some("demo".into()),
                mode: SkillInstallMode::Linked,
            },
            &installer,
        )
        .unwrap();

        assert_eq!(installed, "demo");
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].user_home, user_home);
        assert_eq!(calls[0].package, "vercel-labs/agent-skills");
        assert_eq!(calls[0].skill.as_deref(), Some("demo"));
        assert_eq!(
            fs::read_link(agent_home.join("skills/demo")).unwrap(),
            user_home.join(".agents/skills/demo")
        );
    }

    #[test]
    fn update_library_skills_reports_unchanged_remote_skill() {
        let dir = tempdir().unwrap();
        let user_home = dir.path().join("user");
        write_v3_lock_entry(
            &user_home,
            "demo",
            "same-hash",
            serde_json::json!({"xUnknown": "preserved"}),
        );
        fs::create_dir_all(user_home.join(".agents/skills/demo")).unwrap();
        fs::write(
            user_home.join(".agents/skills/demo").join(SKILL_ENTRYPOINT),
            "# old",
        )
        .unwrap();
        let updater = FakeSkillRemoteUpdater {
            latest_hashes: HashMap::from([("demo".into(), "same-hash".into())]),
            contents: HashMap::new(),
        };

        let result = update_library_skills_with_remote_updater(&user_home, None, &updater).unwrap();

        assert_eq!(result.statuses.len(), 1);
        assert_eq!(result.statuses[0].name, "demo");
        assert_eq!(
            result.statuses[0].status,
            SkillLibraryUpdateState::Unchanged
        );
        assert_eq!(
            fs::read_to_string(user_home.join(".agents/skills/demo").join(SKILL_ENTRYPOINT))
                .unwrap(),
            "# old"
        );
        let lock = read_skill_lock(&user_home).unwrap();
        let entry = lock
            .get("skills")
            .and_then(|skills| skills.get("demo"))
            .unwrap();
        assert_eq!(
            entry.get("xUnknown").and_then(|value| value.as_str()),
            Some("preserved")
        );
        assert_eq!(
            entry.get("source").and_then(|value| value.as_str()),
            Some("vercel-labs/agent-skills")
        );
    }

    #[test]
    fn skill_folder_hash_ignores_holon_install_metadata() {
        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("demo");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join(SKILL_ENTRYPOINT), "# demo").unwrap();
        let before = hash_skill_folder(&skill_dir).unwrap();

        record_install_metadata(&skill_dir, "remote").unwrap();

        assert_eq!(hash_skill_folder(&skill_dir).unwrap(), before);
    }

    #[test]
    fn update_library_skills_updates_changed_remote_skill_and_preserves_lock_fields() {
        let dir = tempdir().unwrap();
        let user_home = dir.path().join("user");
        write_v3_lock_entry(
            &user_home,
            "demo",
            "old-hash",
            serde_json::json!({"custom": {"nested": true}}),
        );
        fs::create_dir_all(user_home.join(".agents/skills/demo")).unwrap();
        fs::write(
            user_home.join(".agents/skills/demo").join(SKILL_ENTRYPOINT),
            "# old",
        )
        .unwrap();
        let updater = FakeSkillRemoteUpdater {
            latest_hashes: HashMap::from([("demo".into(), "new-hash".into())]),
            contents: HashMap::from([("demo".into(), "# new".into())]),
        };

        let result =
            update_library_skills_with_remote_updater(&user_home, Some("demo"), &updater).unwrap();

        assert_eq!(result.statuses.len(), 1);
        assert_eq!(result.statuses[0].status, SkillLibraryUpdateState::Updated);
        assert_eq!(
            fs::read_to_string(user_home.join(".agents/skills/demo").join(SKILL_ENTRYPOINT))
                .unwrap(),
            "# new"
        );
        let lock = read_skill_lock(&user_home).unwrap();
        let entry = lock
            .get("skills")
            .and_then(|skills| skills.get("demo"))
            .unwrap();
        assert_eq!(
            entry
                .get("skillFolderHash")
                .and_then(|value| value.as_str()),
            Some("new-hash")
        );
        assert_eq!(
            entry
                .get("custom")
                .and_then(|value| value.get("nested"))
                .and_then(|value| value.as_bool()),
            Some(true)
        );
        assert_eq!(
            entry.get("sourceType").and_then(|value| value.as_str()),
            Some("github")
        );
    }

    #[test]
    fn update_library_skills_skips_unsupported_and_legacy_lock_entries() {
        let dir = tempdir().unwrap();
        let user_home = dir.path().join("user");
        fs::create_dir_all(user_home.join(".agents")).unwrap();
        fs::write(
            user_home.join(SKILL_LOCK_FILENAME),
            serde_json::to_vec_pretty(&serde_json::json!({
                "version": 3,
                "skills": {
                    "local": {
                        "name": "local",
                        "sourceType": "local",
                        "path": "/tmp/local"
                    },
                    "legacy": {
                        "name": "legacy",
                        "source": "local"
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let updater = FakeSkillRemoteUpdater {
            latest_hashes: HashMap::new(),
            contents: HashMap::new(),
        };

        let result = update_library_skills_with_remote_updater(&user_home, None, &updater).unwrap();

        assert_eq!(result.statuses.len(), 2);
        assert!(result
            .statuses
            .iter()
            .all(|status| status.status == SkillLibraryUpdateState::Skipped));
    }

    #[test]
    fn reconcile_remains_local_only_and_update_preserves_remote_source_semantics() {
        let dir = tempdir().unwrap();
        let user_home = dir.path().join("user");
        let skill_dir = user_home.join(".agents/skills/demo");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join(SKILL_ENTRYPOINT), "# demo").unwrap();
        write_v3_lock_entry(&user_home, "demo", "old-hash", serde_json::json!({}));

        let reconcile = reconcile_library_skills(&user_home, Some("demo")).unwrap();
        assert!(reconcile.added.is_empty());
        assert!(reconcile.removed.is_empty());
        let lock = read_skill_lock(&user_home).unwrap();
        let entry = lock
            .get("skills")
            .and_then(|skills| skills.get("demo"))
            .unwrap();
        assert_eq!(
            entry.get("source").and_then(|value| value.as_str()),
            Some("vercel-labs/agent-skills")
        );
        assert_eq!(
            entry
                .get("skillFolderHash")
                .and_then(|value| value.as_str()),
            Some("old-hash")
        );

        let updater = FakeSkillRemoteUpdater {
            latest_hashes: HashMap::from([("demo".into(), "new-hash".into())]),
            contents: HashMap::from([("demo".into(), "# updated".into())]),
        };
        update_library_skills_with_remote_updater(&user_home, Some("demo"), &updater).unwrap();
        let lock = read_skill_lock(&user_home).unwrap();
        let entry = lock
            .get("skills")
            .and_then(|skills| skills.get("demo"))
            .unwrap();
        assert_eq!(
            entry.get("source").and_then(|value| value.as_str()),
            Some("vercel-labs/agent-skills")
        );
        assert_eq!(
            entry
                .get("skillFolderHash")
                .and_then(|value| value.as_str()),
            Some("new-hash")
        );
    }
}
