use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::types::{
    ActiveSkillRecord, SkillCatalogEntry, SkillInstallMode, SkillRootView, SkillScope,
    SkillsRuntimeView,
};

const SKILL_ENTRYPOINT: &str = "SKILL.md";
const INSTALL_METADATA_FILENAME: &str = ".holon-skill-install.json";
const SKILL_ROOT_SUFFIXES: [&str; 4] = [
    "skills",
    ".agents/skills",
    ".codex/skills",
    ".claude/skills",
];
const COMPAT_SKILL_ROOT_SUFFIXES: [&str; 3] = [".agents/skills", ".codex/skills", ".claude/skills"];

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

fn existing_skill_roots(base: Option<&Path>, suffixes: &[&str]) -> Vec<PathBuf> {
    let Some(base) = base else {
        return Vec::new();
    };
    suffixes
        .iter()
        .map(|suffix| base.join(suffix))
        .filter(|candidate| candidate.is_dir())
        .collect()
}

fn load_catalog_for_scope(scope: SkillScope, root: &Path) -> Result<Vec<SkillCatalogEntry>> {
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
    };
    Ok(name)
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
    let mut matches = Vec::new();
    for root in existing_skill_roots(user_home, &COMPAT_SKILL_ROOT_SUFFIXES) {
        for skill in load_catalog_for_scope(SkillScope::User, &root)? {
            let dir_name = skill
                .path
                .parent()
                .and_then(|path| path.file_name())
                .and_then(|name| name.to_str())
                .unwrap_or("");
            if skill.name == name || dir_name == name {
                if let Some(skill_dir) = skill.path.parent() {
                    matches.push(skill_dir.to_path_buf());
                }
            }
        }
    }
    matches.sort();
    matches.dedup();
    match matches.as_slice() {
        [path] => Ok(path.clone()),
        [] => bail!(
            "skill '{}' is not a builtin skill and was not found in the user-global skill catalog; use an explicit path",
            name
        ),
        paths => bail!(
            "skill '{}' matched multiple user-global skill directories: {}; use an explicit path",
            name,
            paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
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
    fn duplicate_user_global_skill_name_requires_explicit_path() {
        let dir = tempdir().unwrap();
        let user_home = dir.path().join("user");
        for root in [".agents/skills", ".codex/skills"] {
            let source = user_home.join(root).join("demo");
            fs::create_dir_all(&source).unwrap();
            fs::write(
                source.join(SKILL_ENTRYPOINT),
                "---\nname: demo\ndescription: duplicate\n---\nbody",
            )
            .unwrap();
        }

        let error = install_skill_with_user_home(
            &dir.path().join("agent"),
            Some(&user_home),
            &crate::types::SkillInstallKind::Named {
                name: "demo".into(),
                mode: SkillInstallMode::Linked,
            },
        )
        .unwrap_err();

        assert!(error.to_string().contains("matched multiple"));
    }
}
