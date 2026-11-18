use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Result};
use thiserror::Error;

use super::types::{WorkspaceAccessMode, WorkspaceProjectionKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspacePathErrorKind {
    ExecutionRootViolation,
}

#[derive(Debug, Error)]
#[error("path escapes execution root")]
pub struct WorkspacePathError {
    kind: WorkspacePathErrorKind,
}

impl WorkspacePathError {
    pub fn execution_root_violation() -> Self {
        Self {
            kind: WorkspacePathErrorKind::ExecutionRootViolation,
        }
    }

    pub fn kind(&self) -> WorkspacePathErrorKind {
        self.kind
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceView {
    workspace_id: Option<String>,
    workspace_anchor: PathBuf,
    execution_root: PathBuf,
    cwd: PathBuf,
    execution_root_id: Option<String>,
    access_mode: Option<WorkspaceAccessMode>,
    worktree_root: Option<PathBuf>,
    projection_kind: WorkspaceProjectionKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingGitWorktree {
    pub worktree_root: PathBuf,
    pub parent_workspace_anchor: PathBuf,
    pub gitdir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspacePathDiscovery {
    pub workspace_anchor: PathBuf,
    pub execution_root: PathBuf,
    pub cwd: PathBuf,
    pub projection_kind: WorkspaceProjectionKind,
    pub gitdir: Option<PathBuf>,
}

pub fn discover_workspace_path(path: &Path) -> Result<WorkspacePathDiscovery> {
    let normalized_path = normalize_path(path)?;
    if let Some(worktree) = detect_existing_git_worktree(&normalized_path)? {
        return Ok(WorkspacePathDiscovery {
            workspace_anchor: worktree.parent_workspace_anchor,
            execution_root: worktree.worktree_root,
            cwd: normalized_path,
            projection_kind: WorkspaceProjectionKind::GitWorktreeRoot,
            gitdir: Some(worktree.gitdir),
        });
    }

    let mut candidate = normalized_path.as_path();
    loop {
        let git_dir = candidate.join(".git");
        if git_dir.is_dir() {
            return Ok(WorkspacePathDiscovery {
                workspace_anchor: candidate.to_path_buf(),
                execution_root: candidate.to_path_buf(),
                cwd: normalized_path,
                projection_kind: WorkspaceProjectionKind::CanonicalRoot,
                gitdir: Some(git_dir),
            });
        }
        let Some(parent) = candidate.parent() else {
            break;
        };
        candidate = parent;
    }

    Ok(WorkspacePathDiscovery {
        workspace_anchor: normalized_path.clone(),
        execution_root: normalized_path.clone(),
        cwd: normalized_path,
        projection_kind: WorkspaceProjectionKind::CanonicalRoot,
        gitdir: None,
    })
}

pub fn detect_existing_git_worktree(path: &Path) -> Result<Option<ExistingGitWorktree>> {
    let normalized_path = normalize_path(path)?;
    let mut candidate = normalized_path.as_path();
    loop {
        let git_file = candidate.join(".git");
        if git_file.is_file() {
            let content = fs::read_to_string(&git_file)?;
            let Some(gitdir_value) = content.trim().strip_prefix("gitdir:") else {
                return Ok(None);
            };
            let gitdir = normalize_path(&resolve_gitdir(candidate, gitdir_value.trim()))?;
            let Some(parent_workspace_anchor) =
                parent_workspace_anchor_from_worktree_gitdir(&gitdir)
            else {
                return Ok(None);
            };
            return Ok(Some(ExistingGitWorktree {
                worktree_root: candidate.to_path_buf(),
                parent_workspace_anchor,
                gitdir,
            }));
        }
        if git_file.is_dir() {
            return Ok(None);
        }
        let Some(parent) = candidate.parent() else {
            return Ok(None);
        };
        candidate = parent;
    }
}

fn resolve_gitdir(worktree_root: &Path, gitdir: &str) -> PathBuf {
    let gitdir = PathBuf::from(gitdir);
    if gitdir.is_absolute() {
        gitdir
    } else {
        worktree_root.join(gitdir)
    }
}

fn parent_workspace_anchor_from_worktree_gitdir(gitdir: &Path) -> Option<PathBuf> {
    let mut components = gitdir.components();
    let mut anchor = PathBuf::new();
    while let Some(component) = components.next() {
        if component.as_os_str() == ".git" {
            let Some(worktrees) = components.next() else {
                return None;
            };
            if worktrees.as_os_str() != "worktrees" {
                return None;
            }
            if components.next().is_none() {
                return None;
            }
            if components.next().is_some() {
                return None;
            }
            return Some(anchor);
        }
        anchor.push(component.as_os_str());
    }
    None
}

impl WorkspaceView {
    pub fn new(
        workspace_id: Option<String>,
        workspace_anchor: PathBuf,
        execution_root: PathBuf,
        cwd: PathBuf,
        execution_root_id: Option<String>,
        access_mode: Option<WorkspaceAccessMode>,
        projection_kind: WorkspaceProjectionKind,
        worktree_root: Option<PathBuf>,
    ) -> Result<Self> {
        let normalized_anchor = normalize_path(&workspace_anchor)?;
        let normalized_execution_root = normalize_path(&execution_root)?;
        let normalized_cwd = normalize_path(&cwd)?;
        let normalized_worktree_root = if let Some(worktree_root) = &worktree_root {
            let normalized = normalize_path(worktree_root)?;
            if normalized != normalized_execution_root {
                return Err(anyhow!("worktree root must match execution root"));
            }
            Some(normalized)
        } else {
            None
        };
        if normalized_worktree_root.is_none()
            && !normalized_execution_root.starts_with(&normalized_anchor)
        {
            return Err(anyhow!("execution root escapes workspace anchor"));
        }
        if !normalized_cwd.starts_with(&normalized_execution_root) {
            return Err(anyhow!("cwd escapes execution root"));
        }
        Ok(Self {
            workspace_id,
            workspace_anchor: normalized_anchor,
            execution_root: normalized_execution_root,
            cwd: normalized_cwd,
            execution_root_id,
            access_mode,
            projection_kind,
            worktree_root: normalized_worktree_root,
        })
    }

    pub fn workspace_id(&self) -> Option<&str> {
        self.workspace_id.as_deref()
    }

    pub fn workspace_anchor(&self) -> &Path {
        &self.workspace_anchor
    }

    pub fn execution_root(&self) -> &Path {
        &self.execution_root
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn execution_root_id(&self) -> Option<&str> {
        self.execution_root_id.as_deref()
    }

    pub fn access_mode(&self) -> Option<WorkspaceAccessMode> {
        self.access_mode
    }

    pub fn projection_kind(&self) -> WorkspaceProjectionKind {
        self.projection_kind
    }

    pub fn worktree_root(&self) -> Option<&Path> {
        self.worktree_root.as_deref()
    }

    pub fn resolve_path(&self, relative: &str) -> Result<PathBuf> {
        let candidate = if Path::new(relative).is_absolute() {
            PathBuf::from(relative)
        } else {
            self.cwd.join(relative)
        };
        let normalized_candidate = normalize_path(&candidate)?;
        if !normalized_candidate.starts_with(&self.execution_root) {
            return Err(WorkspacePathError::execution_root_violation().into());
        }
        Ok(candidate)
    }

    pub fn resolve_read_path(&self, relative: &str) -> Result<PathBuf> {
        let candidate = if Path::new(relative).is_absolute() {
            PathBuf::from(relative)
        } else {
            self.cwd.join(relative)
        };
        normalize_path(&candidate)
    }

    pub fn resolve_optional_path(&self, relative: Option<&str>) -> Result<PathBuf> {
        match relative {
            Some(relative) => self.resolve_path(relative),
            None => Ok(self.cwd.clone()),
        }
    }
}

pub fn normalize_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            std::path::Component::RootDir => normalized.push(component.as_os_str()),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                let can_pop = matches!(
                    normalized.components().next_back(),
                    Some(std::path::Component::Normal(_))
                );
                if can_pop {
                    normalized.pop();
                }
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests;
