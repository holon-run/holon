use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::types::{AgentsMdKind, AgentsMdScope, AgentsMdSource, LoadedAgentsMd};

const AGENTS_MD_FILENAME: &str = "AGENTS.md";
const CLAUDE_MD_FILENAME: &str = "CLAUDE.md";

pub fn load_agents_md(
    user_home: Option<&Path>,
    agent_home: &Path,
    workspace_anchor: Option<&Path>,
) -> Result<LoadedAgentsMd> {
    Ok(LoadedAgentsMd {
        global_source: load_global_agents_md(user_home)?,
        agent_source: load_agent_agents_md(agent_home)?,
        workspace_source: load_workspace_agents_md(workspace_anchor)?,
    })
}

fn load_global_agents_md(user_home: Option<&Path>) -> Result<Option<AgentsMdSource>> {
    let Some(user_home) = user_home else {
        return Ok(None);
    };
    let path = user_home.join(".agents").join(AGENTS_MD_FILENAME);
    load_source(AgentsMdScope::Global, AgentsMdKind::AgentsMd, &path)
}

fn load_agent_agents_md(agent_home: &Path) -> Result<Option<AgentsMdSource>> {
    let path = agent_home.join(AGENTS_MD_FILENAME);
    load_source(AgentsMdScope::Agent, AgentsMdKind::AgentsMd, &path)
}

fn load_workspace_agents_md(workspace_anchor: Option<&Path>) -> Result<Option<AgentsMdSource>> {
    let Some(workspace_anchor) = workspace_anchor else {
        return Ok(None);
    };

    let agents_md = workspace_anchor.join(AGENTS_MD_FILENAME);
    if let Some(source) = load_source(AgentsMdScope::Workspace, AgentsMdKind::AgentsMd, &agents_md)?
    {
        return Ok(Some(source));
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
) -> Result<Option<AgentsMdSource>> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    Ok(Some(AgentsMdSource {
        scope,
        kind,
        path: PathBuf::from(path),
        content,
    }))
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
            global_source: None,
            agent_source: Some(AgentsMdSource {
                scope: AgentsMdScope::Agent,
                kind: AgentsMdKind::AgentsMd,
                path: PathBuf::from("/tmp/agent/AGENTS.md"),
                content: "secret agent content".into(),
            }),
            workspace_source: None,
        };

        let json = serde_json::to_value(&loaded).unwrap();
        assert_eq!(json["agent_source"]["path"], "/tmp/agent/AGENTS.md");
        assert!(json["agent_source"]["content"].is_null());
    }

    #[test]
    fn loads_global_agent_and_workspace_guidance_layers() {
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
                .global_source
                .as_ref()
                .map(|source| source.scope.clone()),
            Some(AgentsMdScope::Global)
        );
        assert!(loaded.agent_source.is_some());
        assert!(loaded.workspace_source.is_some());
    }

    #[test]
    fn global_and_agent_guidance_survive_workspace_switches() {
        let dir = tempdir().unwrap();
        let user_home = dir.path().join("user");
        let agent_home = dir.path().join("agent");
        let workspace_a = dir.path().join("workspace-a");
        let workspace_b = dir.path().join("workspace-b");
        std::fs::create_dir_all(user_home.join(".agents")).unwrap();
        std::fs::create_dir_all(&agent_home).unwrap();
        std::fs::create_dir_all(&workspace_a).unwrap();
        std::fs::create_dir_all(&workspace_b).unwrap();
        let global_path = user_home.join(".agents").join(AGENTS_MD_FILENAME);
        let agent_path = agent_home.join(AGENTS_MD_FILENAME);
        let workspace_a_path = workspace_a.join(AGENTS_MD_FILENAME);
        let workspace_b_path = workspace_b.join(AGENTS_MD_FILENAME);
        std::fs::write(&global_path, "global\n").unwrap();
        std::fs::write(&agent_path, "agent\n").unwrap();
        std::fs::write(&workspace_a_path, "workspace a\n").unwrap();
        std::fs::write(&workspace_b_path, "workspace b\n").unwrap();

        let loaded_a = load_agents_md(Some(&user_home), &agent_home, Some(&workspace_a)).unwrap();
        let loaded_b = load_agents_md(Some(&user_home), &agent_home, Some(&workspace_b)).unwrap();

        assert_eq!(
            loaded_a.global_source.as_ref().map(|source| &source.path),
            Some(&global_path)
        );
        assert_eq!(
            loaded_b.global_source.as_ref().map(|source| &source.path),
            Some(&global_path)
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
