use std::path::{Path, PathBuf};

use percent_encoding::percent_decode_str;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::WorkspaceProjectionKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRoot {
    pub workspace_id: String,
    pub execution_root_id: String,
    pub filesystem_path: PathBuf,
    pub kind: WorkspaceProjectionKind,
    pub removed: bool,
}

pub fn decode_percent_encoded_path(value: &str) -> Result<String, FileLocationError> {
    decode_component(value)
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileLocationKind {
    File,
    Directory,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct FileLocation {
    pub workspace_id: String,
    pub execution_root_id: String,
    pub path: String,
    pub absolute_path: String,
    pub kind: FileLocationKind,
    pub root_kind: WorkspaceProjectionKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceUriReference {
    pub workspace_id: String,
    pub path: String,
    pub execution_root_id: Option<String>,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum FileLocationError {
    #[error("file reference is not a valid workspace URI")]
    InvalidWorkspaceUri,
    #[error("file path is not valid UTF-8 after percent decoding")]
    InvalidEncoding,
    #[error("path escapes the selected execution root")]
    PathEscapesRoot,
    #[error("path does not exist")]
    PathNotFound,
    #[error("no registered execution root contains the path")]
    RootNotFound,
    #[error("the matching execution root has been removed")]
    RootRemoved,
    #[error("multiple equally specific execution roots contain the path")]
    AmbiguousRoot,
}

pub fn canonical_execution_root_id(workspace_id: &str) -> String {
    format!("canonical_root:{workspace_id}")
}

pub fn parse_workspace_uri(value: &str) -> Result<WorkspaceUriReference, FileLocationError> {
    let rest = value
        .strip_prefix("workspace://")
        .ok_or(FileLocationError::InvalidWorkspaceUri)?;
    let (workspace_id, relative) = rest
        .split_once('/')
        .ok_or(FileLocationError::InvalidWorkspaceUri)?;
    if workspace_id.is_empty() {
        return Err(FileLocationError::InvalidWorkspaceUri);
    }
    let relative = relative.split_once('#').map_or(relative, |(path, _)| path);
    let (path, query) = relative
        .split_once('?')
        .map_or((relative, None), |(path, query)| (path, Some(query)));
    if path.is_empty() {
        return Err(FileLocationError::InvalidWorkspaceUri);
    }
    let execution_root_id = match query {
        None => None,
        Some(query) => {
            let mut root = None;
            for pair in query.split('&') {
                let (key, value) = pair
                    .split_once('=')
                    .ok_or(FileLocationError::InvalidWorkspaceUri)?;
                if key != "root" || value.is_empty() || root.is_some() {
                    return Err(FileLocationError::InvalidWorkspaceUri);
                }
                root = Some(decode_component(value)?);
            }
            root
        }
    };
    Ok(WorkspaceUriReference {
        workspace_id: workspace_id.to_string(),
        path: decode_component(path)?,
        execution_root_id,
    })
}

pub fn resolve_path_within_root(root: &Path, relative: &str) -> Result<PathBuf, FileLocationError> {
    let relative = Path::new(relative);
    if relative.is_absolute() {
        return Err(FileLocationError::PathEscapesRoot);
    }
    let candidate = root.join(relative);
    let normalized = super::workspace::normalize_path(&candidate)
        .map_err(|_| FileLocationError::PathEscapesRoot)?;
    let normalized_root =
        super::workspace::normalize_path(root).map_err(|_| FileLocationError::PathEscapesRoot)?;
    if !normalized.starts_with(&normalized_root) {
        return Err(FileLocationError::PathEscapesRoot);
    }
    if let (Ok(canonical), Ok(canonical_root)) = (
        std::fs::canonicalize(&normalized),
        std::fs::canonicalize(&normalized_root),
    ) {
        if !canonical.starts_with(canonical_root) {
            return Err(FileLocationError::PathEscapesRoot);
        }
    }
    Ok(normalized)
}

pub fn path_is_within_root(path: &Path, root: &Path) -> bool {
    let Ok(normalized) = super::workspace::normalize_path(path) else {
        return false;
    };
    let Ok(normalized_root) = super::workspace::normalize_path(root) else {
        return false;
    };
    if !normalized.starts_with(&normalized_root) {
        return false;
    }
    if let (Ok(canonical), Ok(canonical_root)) = (
        std::fs::canonicalize(&normalized),
        std::fs::canonicalize(&normalized_root),
    ) {
        return canonical.starts_with(canonical_root);
    }
    true
}

pub fn location_for_relative_path(
    root: &FileRoot,
    relative: &str,
) -> Result<FileLocation, FileLocationError> {
    if root.removed {
        return Err(FileLocationError::RootRemoved);
    }
    let absolute = resolve_path_within_root(&root.filesystem_path, relative)?;
    let metadata = std::fs::metadata(&absolute).map_err(|_| FileLocationError::PathNotFound)?;
    let relative = absolute
        .strip_prefix(
            super::workspace::normalize_path(&root.filesystem_path)
                .map_err(|_| FileLocationError::PathEscapesRoot)?,
        )
        .map_err(|_| FileLocationError::PathEscapesRoot)?;
    Ok(FileLocation {
        workspace_id: root.workspace_id.clone(),
        execution_root_id: root.execution_root_id.clone(),
        path: path_to_slash_string(relative),
        absolute_path: absolute.to_string_lossy().into_owned(),
        kind: if metadata.is_dir() {
            FileLocationKind::Directory
        } else {
            FileLocationKind::File
        },
        root_kind: root.kind,
    })
}

pub fn locate_absolute_path(
    roots: &[FileRoot],
    absolute_path: &Path,
) -> Result<FileLocation, FileLocationError> {
    if !absolute_path.is_absolute() {
        return Err(FileLocationError::RootNotFound);
    }
    let normalized = super::workspace::normalize_path(absolute_path)
        .map_err(|_| FileLocationError::RootNotFound)?;
    let mut matches = roots
        .iter()
        .filter_map(|root| {
            let normalized_root = super::workspace::normalize_path(&root.filesystem_path).ok()?;
            normalized
                .starts_with(&normalized_root)
                .then_some((root, normalized_root.components().count()))
        })
        .collect::<Vec<_>>();
    let Some(max_depth) = matches.iter().map(|(_, depth)| *depth).max() else {
        return Err(FileLocationError::RootNotFound);
    };
    matches.retain(|(_, depth)| *depth == max_depth);
    if matches.len() != 1 {
        return Err(FileLocationError::AmbiguousRoot);
    }
    let root = matches[0].0;
    if root.removed {
        return Err(FileLocationError::RootRemoved);
    }
    let relative = normalized
        .strip_prefix(
            super::workspace::normalize_path(&root.filesystem_path)
                .map_err(|_| FileLocationError::RootNotFound)?,
        )
        .map_err(|_| FileLocationError::RootNotFound)?;
    location_for_relative_path(root, &path_to_slash_string(relative))
}

fn decode_component(value: &str) -> Result<String, FileLocationError> {
    percent_decode_str(value)
        .decode_utf8()
        .map(|value| value.into_owned())
        .map_err(|_| FileLocationError::InvalidEncoding)
}

fn path_to_slash_string(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
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
            parse_workspace_uri("workspace://ws_abc/media/image.png?root=root%3Aone#preview")
                .unwrap();
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
}
