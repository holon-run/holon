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
    std::fs::create_dir_all(parent.join(".git").join("worktrees").join("repo-worktree")).unwrap();
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
