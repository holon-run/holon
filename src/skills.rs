use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::Result;
use tracing::warn;

use crate::types::{
    ActiveSkillRecord, SkillCatalogEntry, SkillRootView, SkillScope, SkillsRuntimeView,
};

const SKILL_ENTRYPOINT: &str = "SKILL.md";
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

    if let Some(root) = select_skill_root(Some(agent_home), &SKILL_ROOT_SUFFIXES) {
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
        if !file_type.is_dir() {
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

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::types::{SkillActivationSource, SkillActivationState};

    #[test]
    fn uses_first_existing_root_without_merging_fallbacks() {
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

        assert_eq!(view.discovered_roots.len(), 1);
        assert_eq!(view.discoverable_skills.len(), 1);
        assert_eq!(view.discoverable_skills[0].name, "alpha");
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
}
