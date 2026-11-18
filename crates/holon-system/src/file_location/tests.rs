use super::*;

#[test]
fn parses_workspace_uri_with_opaque_root() {
    let parsed = parse_workspace_uri(
        "workspace://ws_abc/media/image%20one.png?root=git_worktree_root%3Aws_abc%3A%2Ftmp%2Fwt",
    )
    .unwrap();
    assert_eq!(parsed.workspace_id, "ws_abc");
    assert_eq!(parsed.path, "media/image one.png");
    assert_eq!(
        parsed.execution_root_id.as_deref(),
        Some("git_worktree_root:ws_abc:/tmp/wt")
    );
}

#[test]
fn workspace_uri_query_and_fragment_contract_is_explicit() {
    let parsed =
        parse_workspace_uri("workspace://ws_abc/media/image.png?root=root%3Aone#preview").unwrap();
    assert_eq!(parsed.path, "media/image.png");
    assert_eq!(parsed.execution_root_id.as_deref(), Some("root:one"));
    assert_eq!(
        parse_workspace_uri("workspace://ws_abc/media/image.png?root=one&extra=two"),
        Err(FileLocationError::InvalidWorkspaceUri)
    );
    assert_eq!(
        parse_workspace_uri("workspace://ws_abc/media/image.png?root=one&root=two"),
        Err(FileLocationError::InvalidWorkspaceUri)
    );
}

#[test]
fn chooses_the_most_specific_registered_root() {
    let temp = tempfile::tempdir().unwrap();
    let nested = temp.path().join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    let file = nested.join("file.txt");
    std::fs::write(&file, "nested").unwrap();
    let roots = vec![
        FileRoot {
            workspace_id: "outer".into(),
            execution_root_id: "canonical_root:outer".into(),
            filesystem_path: temp.path().to_path_buf(),
            kind: WorkspaceProjectionKind::CanonicalRoot,
            removed: false,
        },
        FileRoot {
            workspace_id: "nested".into(),
            execution_root_id: "canonical_root:nested".into(),
            filesystem_path: nested,
            kind: WorkspaceProjectionKind::CanonicalRoot,
            removed: false,
        },
    ];
    let location = locate_absolute_path(&roots, &file).unwrap();
    assert_eq!(location.workspace_id, "nested");
    assert_eq!(location.path, "file.txt");
}

#[test]
fn rejects_equally_specific_registered_roots() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("file.txt");
    std::fs::write(&file, "ambiguous").unwrap();
    let roots = ["first", "second"]
        .into_iter()
        .map(|id| FileRoot {
            workspace_id: id.into(),
            execution_root_id: id.into(),
            filesystem_path: temp.path().to_path_buf(),
            kind: WorkspaceProjectionKind::CanonicalRoot,
            removed: false,
        })
        .collect::<Vec<_>>();

    assert_eq!(
        locate_absolute_path(&roots, &file),
        Err(FileLocationError::AmbiguousRoot)
    );
}

#[test]
fn rejects_relative_escape() {
    let temp = tempfile::tempdir().unwrap();
    assert_eq!(
        resolve_path_within_root(temp.path(), "../outside"),
        Err(FileLocationError::PathEscapesRoot)
    );
}

#[test]
fn structured_relative_paths_are_not_percent_decoded() {
    let temp = tempfile::tempdir().unwrap();
    let literal = temp.path().join("100%25.txt");
    std::fs::write(&literal, "literal").unwrap();
    let resolved = resolve_path_within_root(temp.path(), "100%25.txt").unwrap();
    assert_eq!(resolved, literal);
}

#[test]
fn removed_nested_root_is_not_hidden_by_parent_root() {
    let temp = tempfile::tempdir().unwrap();
    let nested = temp.path().join("removed");
    std::fs::create_dir_all(&nested).unwrap();
    let file = nested.join("file.txt");
    std::fs::write(&file, "removed").unwrap();
    let roots = vec![
        FileRoot {
            workspace_id: "outer".into(),
            execution_root_id: "canonical_root:outer".into(),
            filesystem_path: temp.path().to_path_buf(),
            kind: WorkspaceProjectionKind::CanonicalRoot,
            removed: false,
        },
        FileRoot {
            workspace_id: "nested".into(),
            execution_root_id: "removed-root".into(),
            filesystem_path: nested,
            kind: WorkspaceProjectionKind::GitWorktreeRoot,
            removed: true,
        },
    ];
    assert_eq!(
        locate_absolute_path(&roots, &file),
        Err(FileLocationError::RootRemoved)
    );
}

#[cfg(unix)]
#[test]
fn rejects_symlink_escape() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_file = outside.path().join("secret.txt");
    std::fs::write(&outside_file, "secret").unwrap();
    symlink(&outside_file, root.path().join("escape.txt")).unwrap();

    assert_eq!(
        resolve_path_within_root(root.path(), "escape.txt"),
        Err(FileLocationError::PathEscapesRoot)
    );
}
