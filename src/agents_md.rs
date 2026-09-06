use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::types::{
    AgentsMdKind, AgentsMdLoadStatus, AgentsMdScope, AgentsMdSource, LoadedAgentsMd,
};

const AGENTS_MD_FILENAME: &str = "AGENTS.md";
const CLAUDE_MD_FILENAME: &str = "CLAUDE.md";

pub fn load_agents_md(
    user_home: Option<&Path>,
    agent_home: &Path,
    workspace_anchor: Option<&Path>,
) -> Result<LoadedAgentsMd> {
    let (user_global_source, user_global_status) = load_user_global_agents_md(user_home)?;
    let (agent_source, agent_status) = load_agent_agents_md(agent_home)?;
    let (workspace_source, workspace_status) = load_workspace_agents_md(workspace_anchor)?;
    Ok(LoadedAgentsMd {
        user_global_source,
        user_global_status,
        agent_source,
        agent_status,
        workspace_source,
        workspace_status,
    })
}

fn load_user_global_agents_md(
    user_home: Option<&Path>,
) -> Result<(Option<AgentsMdSource>, AgentsMdLoadStatus)> {
    let Some(user_home) = user_home else {
        return Ok((None, AgentsMdLoadStatus::RootUnavailable));
    };
    let path = user_home.join(".agents").join(AGENTS_MD_FILENAME);
    load_source(AgentsMdScope::UserGlobal, AgentsMdKind::AgentsMd, &path)
}

fn load_agent_agents_md(agent_home: &Path) -> Result<(Option<AgentsMdSource>, AgentsMdLoadStatus)> {
    let path = agent_home.join(AGENTS_MD_FILENAME);
    load_source(AgentsMdScope::Agent, AgentsMdKind::AgentsMd, &path)
}

fn load_workspace_agents_md(
    workspace_anchor: Option<&Path>,
) -> Result<(Option<AgentsMdSource>, AgentsMdLoadStatus)> {
    let Some(workspace_anchor) = workspace_anchor else {
        return Ok((None, AgentsMdLoadStatus::RootUnavailable));
    };

    let agents_md = workspace_anchor.join(AGENTS_MD_FILENAME);
    let (source, status) =
        load_source(AgentsMdScope::Workspace, AgentsMdKind::AgentsMd, &agents_md)?;
    if source.is_some() {
        return Ok((source, status));
    }

    let claude_md = workspace_anchor.join(CLAUDE_MD_FILENAME);
    load_source(
        AgentsMdScope::Workspace,
        AgentsMdKind::ClaudeMdFallback,
        &claude_md,
    )
}

fn load_source(
    scope: AgentsMdScope,
    kind: AgentsMdKind,
    path: &Path,
) -> Result<(Option<AgentsMdSource>, AgentsMdLoadStatus)> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok((None, AgentsMdLoadStatus::NotFound));
        }
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "failed to read {} instruction file at {}",
                    agents_md_scope_name(&scope),
                    path.display()
                )
            });
        }
    };
    Ok((
        Some(AgentsMdSource {
            scope,
            kind,
            path: PathBuf::from(path),
            content,
        }),
        AgentsMdLoadStatus::Loaded,
    ))
}

fn agents_md_scope_name(scope: &AgentsMdScope) -> &'static str {
    match scope {
        AgentsMdScope::UserGlobal => "user-global",
        AgentsMdScope::Agent => "agent",
        AgentsMdScope::Workspace => "workspace",
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn loads_agent_and_workspace_agents_md() {
        let dir = tempdir().unwrap();
        let agent_home = dir.path().join("agent");
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&agent_home).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(agent_home.join(AGENTS_MD_FILENAME), "agent rules\n").unwrap();
        std::fs::write(workspace.join(AGENTS_MD_FILENAME), "workspace rules\n").unwrap();

        let loaded = load_agents_md(None, &agent_home, Some(&workspace)).unwrap();

        assert_eq!(
            loaded
                .agent_source
                .as_ref()
                .map(|source| source.kind.clone()),
            Some(AgentsMdKind::AgentsMd)
        );
        assert_eq!(
            loaded
                .workspace_source
                .as_ref()
                .map(|source| source.kind.clone()),
            Some(AgentsMdKind::AgentsMd)
        );
    }

    #[test]
    fn uses_workspace_claude_md_only_as_fallback() {
        let dir = tempdir().unwrap();
        let agent_home = dir.path().join("agent");
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&agent_home).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join(CLAUDE_MD_FILENAME), "legacy rules\n").unwrap();

        let loaded = load_agents_md(None, &agent_home, Some(&workspace)).unwrap();

        assert_eq!(
            loaded
                .workspace_source
                .as_ref()
                .map(|source| source.kind.clone()),
            Some(AgentsMdKind::ClaudeMdFallback)
        );

        std::fs::write(workspace.join(AGENTS_MD_FILENAME), "new rules\n").unwrap();
        let loaded = load_agents_md(None, &agent_home, Some(&workspace)).unwrap();
        assert_eq!(
            loaded
                .workspace_source
                .as_ref()
                .map(|source| source.kind.clone()),
            Some(AgentsMdKind::AgentsMd)
        );
    }

    #[test]
    fn loaded_agents_md_does_not_serialize_content() {
        let loaded = LoadedAgentsMd {
            user_global_source: None,
            agent_source: Some(AgentsMdSource {
                scope: AgentsMdScope::Agent,
                kind: AgentsMdKind::AgentsMd,
                path: PathBuf::from("/tmp/agent/AGENTS.md"),
                content: "secret agent content".into(),
            }),
            agent_status: AgentsMdLoadStatus::Loaded,
            workspace_source: None,
            ..LoadedAgentsMd::default()
        };

        let json = serde_json::to_value(&loaded).unwrap();
        assert_eq!(json["agent_source"]["path"], "/tmp/agent/AGENTS.md");
        assert!(json["agent_source"]["content"].is_null());
    }

    #[test]
    fn loads_user_global_agent_and_workspace_guidance_layers() {
        let dir = tempdir().unwrap();
        let user_home = dir.path().join("user");
        let agent_home = dir.path().join("agent");
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(user_home.join(".agents")).unwrap();
        std::fs::create_dir_all(&agent_home).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(
            user_home.join(".agents").join(AGENTS_MD_FILENAME),
            "global\n",
        )
        .unwrap();
        std::fs::write(agent_home.join(AGENTS_MD_FILENAME), "agent\n").unwrap();
        std::fs::write(workspace.join(AGENTS_MD_FILENAME), "workspace\n").unwrap();

        let loaded = load_agents_md(Some(&user_home), &agent_home, Some(&workspace)).unwrap();

        assert_eq!(
            loaded
                .user_global_source
                .as_ref()
                .map(|source| source.scope.clone()),
            Some(AgentsMdScope::UserGlobal)
        );
        assert!(loaded.agent_source.is_some());
        assert!(loaded.workspace_source.is_some());
        assert_eq!(loaded.user_global_status, AgentsMdLoadStatus::Loaded);
        assert_eq!(loaded.agent_status, AgentsMdLoadStatus::Loaded);
        assert_eq!(loaded.workspace_status, AgentsMdLoadStatus::Loaded);
    }

    #[test]
    fn reports_missing_and_unavailable_instruction_roots() {
        let dir = tempdir().unwrap();
        let agent_home = dir.path().join("agent");
        std::fs::create_dir_all(&agent_home).unwrap();

        let loaded = load_agents_md(None, &agent_home, None).unwrap();

        assert_eq!(
            loaded.user_global_status,
            AgentsMdLoadStatus::RootUnavailable
        );
        assert_eq!(loaded.agent_status, AgentsMdLoadStatus::NotFound);
        assert_eq!(loaded.workspace_status, AgentsMdLoadStatus::RootUnavailable);
    }

    #[test]
    fn unreadable_instruction_error_includes_scope_and_path() {
        let dir = tempdir().unwrap();
        let user_home = dir.path().join("user");
        let agents_md = user_home.join(".agents").join(AGENTS_MD_FILENAME);
        std::fs::create_dir_all(&agents_md).unwrap();

        let error = load_agents_md(Some(&user_home), dir.path(), None).unwrap_err();
        let message = format!("{error:#}");

        assert!(message.contains("failed to read user-global instruction file"));
        assert!(message.contains(&agents_md.display().to_string()));
    }

    #[test]
    fn user_global_and_agent_guidance_survive_workspace_switches() {
        let dir = tempdir().unwrap();
        let user_home = dir.path().join("user");
        let agent_home = dir.path().join("agent");
        let workspace_a = dir.path().join("workspace-a");
        let workspace_b = dir.path().join("workspace-b");
        std::fs::create_dir_all(user_home.join(".agents")).unwrap();
        std::fs::create_dir_all(&agent_home).unwrap();
        std::fs::create_dir_all(&workspace_a).unwrap();
        std::fs::create_dir_all(&workspace_b).unwrap();
        let user_global_path = user_home.join(".agents").join(AGENTS_MD_FILENAME);
        let agent_path = agent_home.join(AGENTS_MD_FILENAME);
        let workspace_a_path = workspace_a.join(AGENTS_MD_FILENAME);
        let workspace_b_path = workspace_b.join(AGENTS_MD_FILENAME);
        std::fs::write(&user_global_path, "global\n").unwrap();
        std::fs::write(&agent_path, "agent\n").unwrap();
        std::fs::write(&workspace_a_path, "workspace a\n").unwrap();
        std::fs::write(&workspace_b_path, "workspace b\n").unwrap();

        let loaded_a = load_agents_md(Some(&user_home), &agent_home, Some(&workspace_a)).unwrap();
        let loaded_b = load_agents_md(Some(&user_home), &agent_home, Some(&workspace_b)).unwrap();

        assert_eq!(
            loaded_a
                .user_global_source
                .as_ref()
                .map(|source| &source.path),
            Some(&user_global_path)
        );
        assert_eq!(
            loaded_b
                .user_global_source
                .as_ref()
                .map(|source| &source.path),
            Some(&user_global_path)
        );
        assert_eq!(
            loaded_a.agent_source.as_ref().map(|source| &source.path),
            Some(&agent_path)
        );
        assert_eq!(
            loaded_b.agent_source.as_ref().map(|source| &source.path),
            Some(&agent_path)
        );
        assert_eq!(
            loaded_a
                .workspace_source
                .as_ref()
                .map(|source| &source.path),
            Some(&workspace_a_path)
        );
        assert_eq!(
            loaded_b
                .workspace_source
                .as_ref()
                .map(|source| &source.path),
            Some(&workspace_b_path)
        );
    }
}
