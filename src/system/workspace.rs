use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use thiserror::Error;

use super::types::WorkspaceAccessMode;

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
}

impl WorkspaceView {
    pub fn new(
        workspace_id: Option<String>,
        workspace_anchor: PathBuf,
        execution_root: PathBuf,
        cwd: PathBuf,
        execution_root_id: Option<String>,
        access_mode: Option<WorkspaceAccessMode>,
        worktree_root: Option<PathBuf>,
    ) -> Result<Self> {
        let normalized_anchor = normalize_path(&workspace_anchor)?;
        let normalized_execution_root = normalize_path(&execution_root)?;
        let normalized_cwd = normalize_path(&cwd)?;
        if let Some(worktree_root) = &worktree_root {
            let normalized_worktree_root = normalize_path(worktree_root)?;
            if normalized_worktree_root != normalized_execution_root {
                return Err(anyhow!("worktree root must match execution root"));
            }
        } else if !normalized_execution_root.starts_with(&normalized_anchor) {
            return Err(anyhow!("execution root escapes workspace anchor"));
        }
        if !normalized_cwd.starts_with(&normalized_execution_root) {
            return Err(anyhow!("cwd escapes execution root"));
        }
        Ok(Self {
            workspace_id,
            workspace_anchor,
            execution_root,
            cwd,
            execution_root_id,
            access_mode,
            worktree_root,
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
        let normalized_execution_root = normalize_path(&self.execution_root)?;
        if !normalized_candidate.starts_with(&normalized_execution_root) {
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
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn resolves_relative_paths_under_active_root() {
        let dir = tempdir().unwrap();
        let workspace_root = dir.path().join("workspace");
        let execution_root = workspace_root.join("nested");
        let cwd = execution_root.join("src");
        std::fs::create_dir_all(&cwd).unwrap();

        let view = WorkspaceView::new(
            Some("ws-1".into()),
            workspace_root,
            execution_root.clone(),
            cwd.clone(),
            Some("git_worktree_root:ws-1:/workspace/nested".into()),
            Some(WorkspaceAccessMode::ExclusiveWrite),
            Some(execution_root.clone()),
        )
        .unwrap();
        let resolved = view.resolve_path("src/app.rs").unwrap();
        assert_eq!(resolved, cwd.join("src/app.rs"));
        assert_eq!(view.worktree_root(), Some(execution_root.as_path()));
    }

    #[test]
    fn rejects_escape_paths() {
        let dir = tempdir().unwrap();
        let workspace_root = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace_root).unwrap();
        let view = WorkspaceView::new(
            Some("ws-1".into()),
            workspace_root.clone(),
            workspace_root.clone(),
            workspace_root,
            Some("canonical_root:ws-1".into()),
            Some(WorkspaceAccessMode::SharedRead),
            None,
        )
        .unwrap();
        let error = view.resolve_path("../outside.txt").unwrap_err();
        let workspace_error = error.downcast_ref::<WorkspacePathError>().unwrap();
        assert_eq!(
            workspace_error.kind(),
            WorkspacePathErrorKind::ExecutionRootViolation
        );
    }

    #[test]
    fn resolve_read_path_allows_absolute_paths_outside_execution_root() {
        let dir = tempdir().unwrap();
        let workspace_root = dir.path().join("workspace");
        let external = dir.path().join("external").join("note.txt");
        std::fs::create_dir_all(&workspace_root).unwrap();

        let view = WorkspaceView::new(
            Some("ws-1".into()),
            workspace_root.clone(),
            workspace_root.clone(),
            workspace_root,
            Some("canonical_root:ws-1".into()),
            Some(WorkspaceAccessMode::SharedRead),
            None,
        )
        .unwrap();

        let resolved = view
            .resolve_read_path(external.to_string_lossy().as_ref())
            .unwrap();
        assert_eq!(resolved, external);
    }

    #[test]
    fn normalize_path_preserves_root_when_parent_dir_appears_at_root() {
        let normalized = normalize_path(Path::new("/../etc")).unwrap();
        assert_eq!(normalized, PathBuf::from("/etc"));
    }
}
