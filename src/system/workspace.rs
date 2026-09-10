use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Result};
use thiserror::Error;

use super::types::{ExecutionRootRef, WorkspaceAccessMode, WorkspaceProjectionKind};

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

/// Reverse-map a resolved filesystem path to a canonical `workspace://` URI
/// reference so results can embed durable workspace links instead of raw
/// local paths.
///
/// Priority: active workspace canonical anchor, active execution root (for
/// worktree projections this emits `?root=<execution_root_id>`), attached
/// workspace anchors, then registered execution roots. Returns `None` when
/// the path is outside every workspace (for example a system temp
/// directory); callers should then keep the local path without a workspace
/// reference.
pub fn workspace_uri_for_path(
    path: &Path,
    workspace: &WorkspaceView,
    attached_workspaces: &[(String, PathBuf)],
    execution_roots: &[ExecutionRootRef],
) -> Option<String> {
    if let Some(workspace_id) = workspace.workspace_id() {
        if let Some(relative) = relative_under_root(path, workspace.workspace_anchor()) {
            return Some(format_workspace_uri(workspace_id, &relative, None));
        }
        if let Some(root_id) = workspace.execution_root_id() {
            if let Some(relative) = relative_under_root(path, workspace.execution_root()) {
                return Some(format_workspace_uri(workspace_id, &relative, Some(root_id)));
            }
        }
    }
    for (workspace_id, anchor) in attached_workspaces {
        if let Some(relative) = relative_under_root(path, anchor) {
            return Some(format_workspace_uri(workspace_id, &relative, None));
        }
    }
    for root in execution_roots {
        if let Some(relative) = relative_under_root(path, &root.filesystem_path) {
            return Some(format_workspace_uri(
                &root.workspace_id,
                &relative,
                Some(&root.execution_root_id),
            ));
        }
    }
    None
}

/// Compute the path relative to `root` when `path` lives inside `root`.
/// Prefers symlink-resolved comparison when both exist on disk, falling back
/// to the normalized lexical comparison.
fn relative_under_root(path: &Path, root: &Path) -> Option<PathBuf> {
    let normalized_path = normalize_path(path).ok()?;
    let normalized_root = normalize_path(root).ok()?;
    if let (Ok(canonical_path), Ok(canonical_root)) = (
        fs::canonicalize(&normalized_path),
        fs::canonicalize(&normalized_root),
    ) {
        return canonical_path
            .strip_prefix(&canonical_root)
            .ok()
            .map(Path::to_path_buf);
    }
    normalized_path
        .strip_prefix(&normalized_root)
        .ok()
        .map(Path::to_path_buf)
}

fn format_workspace_uri(
    workspace_id: &str,
    relative: &Path,
    execution_root_id: Option<&str>,
) -> String {
    use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
    // Keep RFC 3986 pchar unreserved/sub-delims readable (`.` `-` `_` `~` etc.)
    // and escape only characters that would break URI parsing.
    const URI_SEGMENT: &AsciiSet = &CONTROLS
        .add(b' ')
        .add(b'"')
        .add(b'#')
        .add(b'%')
        .add(b'?')
        .add(b'\\')
        .add(b'{')
        .add(b'}');
    let encoded_path = relative
        .components()
        .map(|component| {
            utf8_percent_encode(&component.as_os_str().to_string_lossy(), URI_SEGMENT).to_string()
        })
        .collect::<Vec<_>>()
        .join("/");
    match execution_root_id {
        Some(root_id) => format!(
            "workspace://{workspace_id}/{encoded_path}?root={}",
            utf8_percent_encode(root_id, URI_SEGMENT)
        ),
        None => format!("workspace://{workspace_id}/{encoded_path}"),
    }
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
            WorkspaceProjectionKind::GitWorktreeRoot,
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
            WorkspaceProjectionKind::CanonicalRoot,
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
            WorkspaceProjectionKind::CanonicalRoot,
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

    #[test]
    fn detects_existing_git_worktree_from_gitdir_file() {
        let dir = tempdir().unwrap();
        let parent = dir.path().join("repo");
        let worktree = dir.path().join("repo-worktree");
        std::fs::create_dir_all(parent.join(".git").join("worktrees").join("repo-worktree"))
            .unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!(
                "gitdir: {}\n",
                parent
                    .join(".git")
                    .join("worktrees")
                    .join("repo-worktree")
                    .display()
            ),
        )
        .unwrap();

        let detected = detect_existing_git_worktree(&worktree).unwrap().unwrap();
        assert_eq!(detected.worktree_root, worktree);
        assert_eq!(detected.parent_workspace_anchor, parent);
    }

    #[test]
    fn does_not_treat_submodule_gitdir_file_as_worktree() {
        let dir = tempdir().unwrap();
        let parent = dir.path().join("repo");
        let submodule = parent.join("vendor").join("lib");
        std::fs::create_dir_all(parent.join(".git").join("modules").join("vendor/lib")).unwrap();
        std::fs::create_dir_all(&submodule).unwrap();
        std::fs::write(
            submodule.join(".git"),
            "gitdir: ../../.git/modules/vendor/lib\n",
        )
        .unwrap();

        assert!(detect_existing_git_worktree(&submodule).unwrap().is_none());
    }

    #[test]
    fn discovers_repository_root_from_subdirectory() {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");
        let subdir = repo.join("src/nested");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(&subdir).unwrap();

        let discovered = discover_workspace_path(&subdir).unwrap();

        assert_eq!(discovered.workspace_anchor, repo);
        assert_eq!(discovered.execution_root, repo);
        assert_eq!(discovered.cwd, subdir);
        assert_eq!(
            discovered.projection_kind,
            WorkspaceProjectionKind::CanonicalRoot
        );
    }

    #[test]
    fn workspace_uri_maps_canonical_workspace_path() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir_all(root.join("docs")).unwrap();
        let view = WorkspaceView::new(
            Some("ws-1".into()),
            root.clone(),
            root.clone(),
            root.clone(),
            Some("canonical_root:ws-1".into()),
            None,
            WorkspaceProjectionKind::CanonicalRoot,
            None,
        )
        .unwrap();

        let uri = workspace_uri_for_path(&root.join("docs/chart.png"), &view, &[], &[]);

        assert_eq!(uri.as_deref(), Some("workspace://ws-1/docs/chart.png"));
    }

    #[test]
    fn workspace_uri_maps_worktree_path_with_root_param() {
        let dir = tempdir().unwrap();
        let anchor = dir.path().join("repo");
        let worktree = dir.path().join("wt");
        std::fs::create_dir_all(worktree.join("docs")).unwrap();
        let view = WorkspaceView::new(
            Some("ws-1".into()),
            anchor,
            worktree.clone(),
            worktree.clone(),
            Some("git_worktree_root:ws-1:/wt".into()),
            None,
            WorkspaceProjectionKind::GitWorktreeRoot,
            Some(worktree.clone()),
        )
        .unwrap();

        let uri = workspace_uri_for_path(&worktree.join("docs/chart.png"), &view, &[], &[]);

        assert_eq!(
            uri.as_deref(),
            // root tokens stay readable; parsers percent-decode leniently.
            Some("workspace://ws-1/docs/chart.png?root=git_worktree_root:ws-1:/wt")
        );
    }

    #[test]
    fn workspace_uri_maps_attached_workspace_and_registered_root() {
        let dir = tempdir().unwrap();
        let active = dir.path().join("active");
        let other = dir.path().join("other");
        let other_wt = dir.path().join("other-wt");
        std::fs::create_dir_all(active.join("media")).unwrap();
        std::fs::create_dir_all(other_wt.join("shots")).unwrap();
        let view = WorkspaceView::new(
            Some("ws-active".into()),
            active.clone(),
            active.clone(),
            active.clone(),
            Some("canonical_root:ws-active".into()),
            None,
            WorkspaceProjectionKind::CanonicalRoot,
            None,
        )
        .unwrap();

        let attached_uri = workspace_uri_for_path(
            &other.join("media/logo.png"),
            &view,
            &[("ws-other".to_string(), other.clone())],
            &[],
        );
        assert_eq!(
            attached_uri.as_deref(),
            Some("workspace://ws-other/media/logo.png")
        );

        let root_uri = workspace_uri_for_path(
            &other_wt.join("shots/logo.png"),
            &view,
            &[("ws-other".to_string(), other.clone())],
            &[ExecutionRootRef {
                execution_root_id: "git_worktree_root:ws-other:/other-wt".to_string(),
                workspace_id: "ws-other".to_string(),
                filesystem_path: other_wt.clone(),
            }],
        );
        assert_eq!(
            root_uri.as_deref(),
            Some("workspace://ws-other/shots/logo.png?root=git_worktree_root:ws-other:/other-wt")
        );
    }

    #[test]
    fn workspace_uri_returns_none_for_unmapped_path() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let outside = dir.path().join("tmp").join("scratch.png");
        std::fs::create_dir_all(dir.path().join("tmp")).unwrap();
        let view = WorkspaceView::new(
            Some("ws-1".into()),
            root.clone(),
            root.clone(),
            root.clone(),
            Some("canonical_root:ws-1".into()),
            None,
            WorkspaceProjectionKind::CanonicalRoot,
            None,
        )
        .unwrap();

        assert_eq!(workspace_uri_for_path(&outside, &view, &[], &[]), None);
    }
}
