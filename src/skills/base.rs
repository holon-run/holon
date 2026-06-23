use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::Duration,
};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::types::{
    ActiveSkillRecord, SkillCatalogEntry, SkillInstallMode, SkillRootRegistration,
    SkillRootScanStatus, SkillRootSourceKind, SkillRootView, SkillRootWatchStatus, SkillScope,
    SkillsRuntimeView,
};

const SKILL_ENTRYPOINT: &str = "SKILL.md";
const INSTALL_METADATA_FILENAME: &str = ".holon-skill-install.json";
pub(crate) const SKILL_ROOT_SUFFIXES: [&str; 4] = [
    "skills",
    ".agents/skills",
    ".codex/skills",
    ".claude/skills",
];
pub(crate) const COMPAT_SKILL_ROOT_SUFFIXES: [&str; 3] =
    [".agents/skills", ".codex/skills", ".claude/skills"];

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
            discoverable_skills.extend(load_catalog_for_scope(SkillScope::User, &root)?);
            discovered_roots.push(SkillRootView {
                scope: SkillScope::User,
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
    let active_skill_ids = discoverable_skills
        .iter()
        .map(|entry| entry.skill_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let active_skills = active_skills
        .iter()
        .filter(|record| active_skill_ids.contains(record.skill_id.as_str()))
        .cloned()
        .collect::<Vec<_>>();

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
            skill_id: format!("{}:{skill_name}", scope_label(scope)),
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
    let active_skill_ids = catalog
        .iter()
        .map(|entry| entry.skill_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let active_skills = active_skills
        .iter()
        .filter(|record| active_skill_ids.contains(record.skill_id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
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
                SkillRootSourceKind::UserGlobal => SkillScope::User,
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
        SkillScope::User => 1,
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

fn scope_label(scope: SkillScope) -> &'static str {
    match scope {
        SkillScope::User => "user",
        SkillScope::Agent => "agent",
        SkillScope::Workspace => "workspace",
    }
}

const SKILL_ROOT_SUFFIX_AGENT: &str = "skills";

fn agent_skills_root(agent_home: &Path) -> PathBuf {
    // Prefer an existing skill root if one already exists (e.g. .agents/skills, .codex/skills).
    // This avoids creating a new `skills/` root that would shadow legacy roots.
    if let Some(existing) = select_skill_root(Some(agent_home), &SKILL_ROOT_SUFFIXES) {
        existing
    } else {
        agent_home.join(SKILL_ROOT_SUFFIX_AGENT)
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
pub struct SkillManagerUnavailable {
    pub manager: String,
}

impl std::fmt::Display for SkillManagerUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "remote skill install requires {}", self.manager)
    }
}

impl std::error::Error for SkillManagerUnavailable {}

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
    pub timeout: Duration,
}

impl std::fmt::Display for RemoteSkillInstallTimedOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "remote skill install for '{}' timed out after {}s",
            self.package,
            self.timeout.as_secs()
        )
    }
}

impl std::error::Error for RemoteSkillInstallTimedOut {}

trait RemoteSkillInstaller {
    fn add_global(&self, package: &str, skill: Option<&str>) -> Result<()>;
}

struct NpxRemoteSkillInstaller;

impl RemoteSkillInstaller for NpxRemoteSkillInstaller {
    fn add_global(&self, package: &str, skill: Option<&str>) -> Result<()> {
        const REMOTE_SKILL_INSTALL_TIMEOUT: Duration = Duration::from_secs(120);

        match command_output_with_timeout(
            Command::new("npx").arg("--version"),
            Duration::from_secs(10),
        ) {
            Ok(Some(_)) => {}
            Ok(None) => {
                return Err(RemoteSkillInstallTimedOut {
                    package: package.into(),
                    timeout: Duration::from_secs(10),
                }
                .into());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(SkillManagerUnavailable {
                    manager: "npx".into(),
                }
                .into());
            }
            Err(error) => return Err(error).context("failed to check npx availability"),
        }

        let mut command = Command::new("npx");
        command.args(["--yes", "skills", "add", package, "--global", "--yes"]);
        if let Some(skill) = skill {
            command.args(["--skill", skill]);
        }
        let output = command_output_with_timeout(&mut command, REMOTE_SKILL_INSTALL_TIMEOUT)
            .with_context(|| format!("failed to run npx skills add for {package}"))?;
        let Some(output) = output else {
            return Err(RemoteSkillInstallTimedOut {
                package: package.into(),
                timeout: REMOTE_SKILL_INSTALL_TIMEOUT,
            }
            .into());
        };
        if !output.status.success() {
            return Err(RemoteSkillInstallFailed {
                package: package.into(),
                status: output.status.code(),
                stdout: bounded_output_excerpt(&output.stdout),
                stderr: bounded_output_excerpt(&output.stderr),
            }
            .into());
        }
        Ok(())
    }
}

fn command_output_with_timeout(
    command: &mut Command,
    timeout: Duration,
) -> std::io::Result<Option<Output>> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() {
            return child.wait_with_output().map(Some);
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
#[derive(Clone)]
struct RecordedRemoteSkillInstall {
    package: String,
    skill: Option<String>,
}

#[cfg(test)]
struct RecordingRemoteSkillInstaller {
    calls: std::sync::Arc<std::sync::Mutex<Vec<RecordedRemoteSkillInstall>>>,
}

#[cfg(test)]
impl RemoteSkillInstaller for RecordingRemoteSkillInstaller {
    fn add_global(&self, package: &str, skill: Option<&str>) -> Result<()> {
        self.calls.lock().unwrap().push(RecordedRemoteSkillInstall {
            package: package.into(),
            skill: skill.map(str::to_string),
        });
        Ok(())
    }
}

#[cfg(test)]
struct UnavailableRemoteSkillInstaller;

#[cfg(test)]
impl RemoteSkillInstaller for UnavailableRemoteSkillInstaller {
    fn add_global(&self, _package: &str, _skill: Option<&str>) -> Result<()> {
        Err(SkillManagerUnavailable {
            manager: "npx".into(),
        }
        .into())
    }
}

fn bounded_output_excerpt(bytes: &[u8]) -> String {
    const MAX_OUTPUT_BYTES: usize = 2048;
    let excerpt = if bytes.len() > MAX_OUTPUT_BYTES {
        &bytes[..MAX_OUTPUT_BYTES]
    } else {
        bytes
    };
    String::from_utf8_lossy(excerpt).to_string()
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
        &NpxRemoteSkillInstaller,
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
            remote_installer.add_global(package, skill.as_deref())?;
            let skill_name = match skill {
                Some(skill) => skill.as_str(),
                None => remote_package_default_skill_name(package)?,
            };
            let path = resolve_user_skill_by_name(user_home, skill_name)?;
            materialize_local_skill(&skills_root, &path, mode)?
        }
    };
    Ok(name)
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
        for skill in load_catalog_for_scope(SkillScope::User, &root)? {
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
                    skill_id: format!("agent:{skill_name}"),
                    name: parsed.name.unwrap_or_else(|| skill_name.clone()),
                    description: parsed
                        .description
                        .unwrap_or_else(|| first_body_paragraph(&content)),
                    path: skill_path,
                    scope: SkillScope::Agent,
                }
            } else {
                SkillCatalogEntry {
                    skill_id: format!("agent:{skill_name}"),
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
            .skill_id
            .cmp(&right.catalog.skill_id)
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
    use tempfile::tempdir;

    use super::*;
    use crate::types::{SkillActivationSource, SkillActivationState};

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
        let names = view
            .discoverable_skills
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<Vec<_>>();
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
            .any(|entry| entry.scope == SkillScope::User));
        assert!(non_default_view
            .discoverable_skills
            .iter()
            .all(|entry| entry.scope != SkillScope::User));
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

        let view = load_skills_runtime_view(
            SkillVisibility::DefaultAgent,
            Some(&user_home),
            &agent_home,
            Some(&workspace),
            &[
                ActiveSkillRecord {
                    skill_id: "workspace:ghx".into(),
                    name: "ghx".into(),
                    path: workspace_skill.join(SKILL_ENTRYPOINT),
                    scope: SkillScope::Workspace,
                    agent_id: "default".into(),
                    activation_source: SkillActivationSource::ImplicitFromCatalog,
                    activation_state: SkillActivationState::SessionActive,
                    activated_at_turn: 1,
                },
                ActiveSkillRecord {
                    skill_id: "agent:ghx".into(),
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
        assert_eq!(view.discoverable_skills[0].skill_id, "agent:ghx");
        assert_eq!(view.discoverable_skills[0].description, "agent skill");
        assert_eq!(view.active_skills.len(), 1);
        assert_eq!(view.active_skills[0].skill_id, "agent:ghx");
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
                name: "shared".into(),
                description: "later id".into(),
                path: later_id_path,
                scope: SkillScope::Agent,
            },
            SkillCatalogEntry {
                skill_id: "agent:a-skill".into(),
                name: "shared".into(),
                description: "earlier id".into(),
                path: earlier_id_path,
                scope: SkillScope::Agent,
            },
            SkillCatalogEntry {
                skill_id: "workspace:skill".into(),
                name: "same-id".into(),
                description: "later path".into(),
                path: same_id_later_path,
                scope: SkillScope::Workspace,
            },
            SkillCatalogEntry {
                skill_id: "workspace:skill".into(),
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

        let view = load_skills_runtime_view(
            SkillVisibility::NonDefaultAgent,
            None,
            dir.path(),
            None,
            &[ActiveSkillRecord {
                skill_id: "agent:demo".into(),
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
                skill_id: "agent:demo".into(),
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
                    skill_id: "agent:demo".into(),
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
        assert_eq!(view.active_skills[0].skill_id, "agent:demo");
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
    fn remote_install_requires_skill_manager() {
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

        let unavailable = error
            .downcast_ref::<SkillManagerUnavailable>()
            .expect("missing remote manager should be structured");
        assert_eq!(unavailable.manager, "npx");
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
    fn command_output_with_timeout_captures_failed_process_output() {
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "read ignored || true; printf captured-stdout; printf captured-stderr >&2; exit 7",
        ]);

        let output = command_output_with_timeout(&mut command, Duration::from_secs(5))
            .unwrap()
            .expect("process should exit before timeout");

        assert_eq!(output.status.code(), Some(7));
        assert_eq!(bounded_output_excerpt(&output.stdout), "captured-stdout");
        assert_eq!(bounded_output_excerpt(&output.stderr), "captured-stderr");
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
        assert_eq!(calls[0].package, "vercel-labs/agent-skills");
        assert_eq!(calls[0].skill.as_deref(), Some("demo"));
        assert_eq!(
            fs::read_link(agent_home.join("skills/demo")).unwrap(),
            user_home.join(".agents/skills/demo")
        );
    }
}
