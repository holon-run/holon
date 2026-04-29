use anyhow::{Context, Result};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use super::helpers::{normalize_path, resolve_workspace_path};
use crate::{
    tool::ToolError,
    types::{
        ApplyPatchAction, ApplyPatchChangedFile, ApplyPatchDiagnostic, ApplyPatchIgnoredMetadata,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApplyPatchOutcome {
    pub(crate) changed_files: Vec<ApplyPatchChangedFile>,
    pub(crate) changed_paths: Vec<String>,
    pub(crate) ignored_metadata: Vec<ApplyPatchIgnoredMetadata>,
    pub(crate) diagnostics: Vec<ApplyPatchDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FilePatch {
    old_path: PatchPath,
    new_path: PatchPath,
    rename_from: Option<String>,
    rename_to: Option<String>,
    hunks: Vec<PatchHunk>,
    ignored_metadata: Vec<ApplyPatchIgnoredMetadata>,
    diagnostics: Vec<ApplyPatchDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PatchPath {
    Workspace(String),
    DevNull,
}

impl PatchPath {
    fn as_workspace_path(&self) -> Option<&str> {
        match self {
            Self::Workspace(path) => Some(path.as_str()),
            Self::DevNull => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PatchHunk {
    old_start: usize,
    old_count: usize,
    new_start: usize,
    new_count: usize,
    lines: Vec<HunkLine>,
    no_newline_at_end: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HunkLine {
    kind: HunkLineKind,
    text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HunkLineKind {
    Context,
    Add,
    Remove,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileState {
    lines: Vec<String>,
    trailing_newline: bool,
}

impl FileState {
    fn from_content(content: &str) -> Self {
        if content.is_empty() {
            return Self {
                lines: Vec::new(),
                trailing_newline: false,
            };
        }

        let trailing_newline = content.ends_with('\n');
        let mut lines = content
            .split('\n')
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        if trailing_newline {
            lines.pop();
        }
        Self {
            lines,
            trailing_newline,
        }
    }

    fn into_content(self) -> String {
        let mut content = self.lines.join("\n");
        if self.trailing_newline && !content.is_empty() {
            content.push('\n');
        }
        content
    }
}

pub(crate) async fn apply_patch(workspace_root: &Path, input: &str) -> Result<ApplyPatchOutcome> {
    let patches = parse_patch(input)?;
    let (changed_files, touched, ignored_metadata, diagnostics) =
        apply_file_patches(workspace_root, &patches).await?;
    Ok(ApplyPatchOutcome {
        changed_files,
        changed_paths: touched,
        ignored_metadata,
        diagnostics,
    })
}

fn parse_patch(input: &str) -> Result<Vec<FilePatch>> {
    if input.lines().next().map(str::trim) == Some("*** Begin Patch") {
        return Err(syntax_error(
            "legacy_patch_format",
            "ApplyPatch now expects unified diff text, not *** Begin Patch DSL",
            None,
            "submit unified diff with --- old_path, +++ new_path, and @@ hunks",
        ));
    }

    let lines = input.lines().map(ToString::to_string).collect::<Vec<_>>();
    let mut patches = Vec::new();
    let mut index = 0usize;

    while index < lines.len() {
        while index < lines.len() && lines[index].trim().is_empty() {
            index += 1;
        }
        if index >= lines.len() {
            break;
        }

        let mut ignored_metadata = Vec::new();
        let mut rename_from = None;
        let mut rename_to = None;
        let git_paths = if lines[index].starts_with("diff --git ") {
            let parsed = parse_git_header(&lines[index])?;
            index += 1;
            Some(parsed)
        } else {
            None
        };

        while index < lines.len() {
            let line = lines[index].as_str();
            if line.starts_with("--- ") || line.starts_with("diff --git ") {
                break;
            }
            if let Some(path) = line.strip_prefix("rename from ") {
                rename_from = Some(strip_diff_prefix(path));
                index += 1;
                continue;
            }
            if let Some(path) = line.strip_prefix("rename to ") {
                rename_to = Some(strip_diff_prefix(path));
                index += 1;
                continue;
            }
            if let Some(metadata) = parse_accepted_metadata(line, None) {
                ignored_metadata.push(metadata);
                index += 1;
                continue;
            }
            if is_unsupported_git_feature(line) {
                return Err(unsupported_git_patch_feature(line, None));
            }
            if line.trim().is_empty() {
                index += 1;
                continue;
            }
            return Err(syntax_error(
                "unexpected_git_metadata",
                format!("unexpected unified diff metadata line: {line}"),
                None,
                "remove unsupported metadata or use ---/+++ file headers before hunks",
            ));
        }

        if index >= lines.len() || lines[index].starts_with("diff --git ") {
            let (Some(rename_from), Some(rename_to)) = (rename_from, rename_to) else {
                return Err(syntax_error(
                    "missing_file_header",
                    "expected --- old_path followed by +++ new_path",
                    None,
                    "add --- and +++ headers before unified diff hunks",
                ));
            };
            let Some((old_path, new_path)) = git_paths else {
                return Err(rename_requires_git_header(&rename_from, &rename_to));
            };
            validate_rename_paths(&rename_from, &rename_to, &old_path, &new_path)?;
            fill_metadata_paths(&mut ignored_metadata, &rename_to);
            patches.push(FilePatch {
                old_path: PatchPath::Workspace(rename_from.clone()),
                new_path: PatchPath::Workspace(rename_to.clone()),
                rename_from: Some(rename_from),
                rename_to: Some(rename_to),
                hunks: Vec::new(),
                ignored_metadata,
                diagnostics: Vec::new(),
            });
            continue;
        }

        let old_path = parse_file_header(&lines[index], "---")?;
        index += 1;
        if index >= lines.len() {
            return Err(syntax_error(
                "missing_file_header",
                "expected +++ new_path after --- old_path",
                old_path.as_workspace_path(),
                "add the matching +++ file header before unified diff hunks",
            ));
        }
        let new_path = parse_file_header(&lines[index], "+++")?;
        index += 1;

        if let (Some(rename_from), Some(rename_to)) = (rename_from.as_ref(), rename_to.as_ref()) {
            if git_paths.is_none() {
                return Err(rename_requires_git_header(rename_from, rename_to));
            }
            validate_rename_file_headers(rename_from, rename_to, &old_path, &new_path)?;
        }
        if let Some((git_old, git_new)) = git_paths.as_ref() {
            validate_git_file_headers(git_old, git_new, &old_path, &new_path)?;
        }
        if let Some(path) = new_path
            .as_workspace_path()
            .or_else(|| old_path.as_workspace_path())
        {
            fill_metadata_paths(&mut ignored_metadata, path);
        }

        let mut hunks = Vec::new();
        let mut diagnostics = Vec::new();
        while index < lines.len() {
            if lines[index].starts_with("diff --git ") {
                break;
            }
            if lines[index].starts_with("--- ")
                && index + 1 < lines.len()
                && lines[index + 1].starts_with("+++ ")
            {
                break;
            }
            if lines[index].trim().is_empty() {
                index += 1;
                continue;
            }
            if !lines[index].starts_with("@@ ") {
                return Err(syntax_error(
                    "invalid_hunk_header",
                    format!("expected unified diff hunk header, got: {}", lines[index]),
                    old_path.as_workspace_path(),
                    "use @@ -old_start,old_count +new_start,new_count @@ before hunk lines",
                ));
            }
            let hunk_path = new_path
                .as_workspace_path()
                .or_else(|| old_path.as_workspace_path());
            let (hunk, consumed, hunk_diagnostics) = parse_hunk(&lines[index..], hunk_path)?;
            hunks.push(hunk);
            diagnostics.extend(hunk_diagnostics);
            index += consumed;
        }

        if hunks.is_empty() {
            return Err(syntax_error(
                "missing_hunk",
                "file patch must include at least one hunk unless it is rename-only",
                new_path
                    .as_workspace_path()
                    .or_else(|| old_path.as_workspace_path()),
                "add an @@ hunk or use diff --git with rename from/rename to for rename-only",
            ));
        }

        patches.push(FilePatch {
            old_path,
            new_path,
            rename_from,
            rename_to,
            hunks,
            ignored_metadata,
            diagnostics,
        });
    }

    if patches.is_empty() {
        return Err(syntax_error(
            "missing_file_header",
            "expected at least one unified diff file patch",
            None,
            "submit unified diff text with --- old_path and +++ new_path",
        ));
    }

    Ok(patches)
}

fn parse_git_header(line: &str) -> Result<(String, String)> {
    let rest = line
        .strip_prefix("diff --git ")
        .ok_or_else(|| syntax_error("invalid_git_header", "invalid diff --git header", None, ""))?;
    let parts = rest.split_whitespace().collect::<Vec<_>>();
    if parts.len() != 2 {
        return Err(syntax_error(
            "invalid_git_header",
            "expected diff --git a/path b/path",
            None,
            "use diff --git a/path b/path or omit the git header",
        ));
    }
    Ok((strip_diff_prefix(parts[0]), strip_diff_prefix(parts[1])))
}

fn fill_metadata_paths(metadata: &mut [ApplyPatchIgnoredMetadata], path: &str) {
    for entry in metadata {
        if entry.path.is_empty() {
            entry.path = path.to_string();
        }
    }
}

fn parse_file_header(line: &str, prefix: &str) -> Result<PatchPath> {
    let expected = format!("{prefix} ");
    let path = line.strip_prefix(&expected).ok_or_else(|| {
        syntax_error(
            "missing_file_header",
            format!("expected {prefix} file header"),
            None,
            "use --- old_path followed by +++ new_path",
        )
    })?;
    if path == "/dev/null" {
        return Ok(PatchPath::DevNull);
    }
    Ok(PatchPath::Workspace(strip_diff_prefix(path)))
}

fn parse_hunk(
    lines: &[String],
    path: Option<&str>,
) -> Result<(PatchHunk, usize, Vec<ApplyPatchDiagnostic>)> {
    let (old_start, old_count, new_start, new_count) = parse_hunk_header(&lines[0], path)?;
    let mut consumed = 1usize;
    let mut hunk_lines: Vec<HunkLine> = Vec::new();
    let mut no_newline_at_end = false;

    while consumed < lines.len() {
        let line = lines[consumed].as_str();
        if line.starts_with("@@ ")
            || line.starts_with("diff --git ")
            || (line.starts_with("--- ")
                && consumed + 1 < lines.len()
                && lines[consumed + 1].starts_with("+++ "))
        {
            break;
        }
        if line == r"\ No newline at end of file" {
            no_newline_at_end = true;
            consumed += 1;
            continue;
        }
        let Some(first) = line.chars().next() else {
            // Provide context for debugging: show hunk line number and surrounding lines
            let hunk_line_num = hunk_lines.len() + 1;
            let context_start = hunk_lines.len().saturating_sub(2);
            let context_end = hunk_lines.len();

            let mut context_lines = String::new();
            for i in context_start..context_end {
                if i < hunk_lines.len() {
                    if !context_lines.is_empty() {
                        context_lines.push('\n');
                    }
                    // Reconstruct the original hunk line with prefix for context display
                    let prefix = match hunk_lines[i].kind {
                        HunkLineKind::Context => " ",
                        HunkLineKind::Add => "+",
                        HunkLineKind::Remove => "-",
                    };
                    context_lines.push_str(prefix);
                    context_lines.push_str(&hunk_lines[i].text);
                }
            }

            return Err(syntax_error(
                "invalid_hunk_empty_line",
                format!(
                    "hunk line {hunk_line_num} is empty; all hunk lines must have a prefix (space for context, + for added, - for removed). Context:\n{context_lines}\n---\nFix: add a space character to this blank line.",
                ),
                path,
                "ensure all blank lines within hunk sections have a space prefix",
            ));
        };
        let kind = match first {
            ' ' => HunkLineKind::Context,
            '+' => HunkLineKind::Add,
            '-' => HunkLineKind::Remove,
            _ => {
                return Err(syntax_error(
                    "invalid_hunk_line",
                    format!("hunk line must start with space, +, or -, got: {line}"),
                    path,
                    "use only context, added, and removed lines inside hunks",
                ))
            }
        };
        hunk_lines.push(HunkLine {
            kind,
            text: line[1..].to_string(),
        });
        consumed += 1;
    }

    if hunk_lines.is_empty() {
        return Err(syntax_error(
            "invalid_hunk_header",
            "hunk header must be followed by at least one hunk line",
            path,
            "include context, added, or removed lines after the @@ header",
        ));
    }

    let old_actual = hunk_lines
        .iter()
        .filter(|line| line.kind != HunkLineKind::Add)
        .count();
    let new_actual = hunk_lines
        .iter()
        .filter(|line| line.kind != HunkLineKind::Remove)
        .count();
    let mut diagnostics = Vec::new();
    if old_actual != old_count || new_actual != new_count {
        diagnostics.push(ApplyPatchDiagnostic {
            path: path.unwrap_or("").to_string(),
            kind: "hunk_count_mismatch".to_string(),
            message: format!(
                "hunk header declared -{},{} +{},{} but body counted -{},{} +{},{}",
                old_start,
                old_count,
                new_start,
                new_count,
                old_start,
                old_actual,
                new_start,
                new_actual
            ),
        });
    }

    Ok((
        PatchHunk {
            old_start,
            old_count,
            new_start,
            new_count,
            lines: hunk_lines,
            no_newline_at_end,
        },
        consumed,
        diagnostics,
    ))
}

fn parse_hunk_header(line: &str, path: Option<&str>) -> Result<(usize, usize, usize, usize)> {
    let Some(rest) = line.strip_prefix("@@ -") else {
        return Err(syntax_error(
            "invalid_hunk_header",
            "expected @@ -old_start,old_count +new_start,new_count @@",
            path,
            "use a standard unified diff hunk header",
        ));
    };
    let Some((old_range, rest)) = rest.split_once(" +") else {
        return Err(syntax_error(
            "invalid_hunk_header",
            "missing +new range in hunk header",
            path,
            "use @@ -old_start,old_count +new_start,new_count @@",
        ));
    };
    let Some((new_range, _suffix)) = rest.split_once(" @@") else {
        return Err(syntax_error(
            "invalid_hunk_header",
            "hunk header must end the range section with @@",
            path,
            "use @@ -old_start,old_count +new_start,new_count @@",
        ));
    };
    let (old_start, old_count) = parse_range(old_range, path)?;
    let (new_start, new_count) = parse_range(new_range, path)?;
    Ok((old_start, old_count, new_start, new_count))
}

fn parse_range(range: &str, path: Option<&str>) -> Result<(usize, usize)> {
    if let Some((start, count)) = range.split_once(',') {
        return Ok((
            start.parse().map_err(|_| {
                syntax_error(
                    "invalid_hunk_header",
                    format!("invalid hunk start: {start}"),
                    path,
                    "use numeric hunk ranges",
                )
            })?,
            count.parse().map_err(|_| {
                syntax_error(
                    "invalid_hunk_header",
                    format!("invalid hunk count: {count}"),
                    path,
                    "use numeric hunk ranges",
                )
            })?,
        ));
    }
    Ok((
        range.parse().map_err(|_| {
            syntax_error(
                "invalid_hunk_header",
                format!("invalid hunk start: {range}"),
                path,
                "use numeric hunk ranges",
            )
        })?,
        1,
    ))
}

async fn apply_file_patches(
    workspace_root: &Path,
    patches: &[FilePatch],
) -> Result<(
    Vec<ApplyPatchChangedFile>,
    Vec<String>,
    Vec<ApplyPatchIgnoredMetadata>,
    Vec<ApplyPatchDiagnostic>,
)> {
    let mut touched_paths = BTreeSet::new();
    for patch in patches {
        let mut patch_paths = BTreeSet::<(String, String)>::new();
        for path in [
            patch.old_path.as_workspace_path(),
            patch.new_path.as_workspace_path(),
            patch.rename_from.as_deref(),
            patch.rename_to.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            let resolved =
                map_workspace_path_error(path, resolve_workspace_path(workspace_root, path))?;
            let normalized = normalize_path(&resolved)?;
            patch_paths.insert((normalized.display().to_string(), path.to_string()));
        }
        for (normalized, _original) in patch_paths {
            if !touched_paths.insert(normalized.clone()) {
                return Err(duplicate_file_patch(&normalized));
            }
        }
    }

    let mut state = HashMap::<PathBuf, Option<FileState>>::new();
    let mut originals = HashMap::<PathBuf, Option<FileState>>::new();
    let mut changed_files = Vec::new();
    let mut ignored_metadata = Vec::new();
    let mut diagnostics = Vec::new();

    for patch in patches {
        ignored_metadata.extend(patch.ignored_metadata.clone());
        diagnostics.extend(patch.diagnostics.clone());
        match patch_operation_kind(patch)? {
            PatchOperationKind::Add { path } => {
                let target =
                    map_workspace_path_error(path, resolve_workspace_path(workspace_root, path))?;
                let existing = load_state(&target, &mut state, &mut originals).await?;
                if existing.is_some() {
                    return Err(existing_file(path));
                }
                let updated = apply_hunks(
                    path,
                    FileState {
                        lines: Vec::new(),
                        trailing_newline: false,
                    },
                    &patch.hunks,
                )?;
                state.insert(target, Some(updated));
                changed_files.push(ApplyPatchChangedFile {
                    action: ApplyPatchAction::Add,
                    path: path.to_string(),
                    from_path: None,
                });
            }
            PatchOperationKind::Delete { path } => {
                let target =
                    map_workspace_path_error(path, resolve_workspace_path(workspace_root, path))?;
                let existing = load_state(&target, &mut state, &mut originals)
                    .await?
                    .ok_or_else(|| missing_file("delete", path))?;
                if !patch.hunks.is_empty() {
                    let _ = apply_hunks(path, existing, &patch.hunks)?;
                }
                state.insert(target, None);
                changed_files.push(ApplyPatchChangedFile {
                    action: ApplyPatchAction::Delete,
                    path: path.to_string(),
                    from_path: None,
                });
            }
            PatchOperationKind::Modify { path } => {
                let target =
                    map_workspace_path_error(path, resolve_workspace_path(workspace_root, path))?;
                let existing = load_state(&target, &mut state, &mut originals)
                    .await?
                    .ok_or_else(|| missing_file("update", path))?;
                let updated = apply_hunks(path, existing, &patch.hunks)?;
                state.insert(target, Some(updated));
                changed_files.push(ApplyPatchChangedFile {
                    action: ApplyPatchAction::Modify,
                    path: path.to_string(),
                    from_path: None,
                });
            }
            PatchOperationKind::Rename {
                from,
                to,
                with_edit,
            } => {
                let source =
                    map_workspace_path_error(from, resolve_workspace_path(workspace_root, from))?;
                let target =
                    map_workspace_path_error(to, resolve_workspace_path(workspace_root, to))?;
                let existing = load_state(&source, &mut state, &mut originals)
                    .await?
                    .ok_or_else(|| missing_file("rename", from))?;
                let target_existing = load_state(&target, &mut state, &mut originals).await?;
                if target_existing.is_some() {
                    return Err(existing_file(to));
                }
                let final_state = if with_edit {
                    apply_hunks(from, existing, &patch.hunks)?
                } else {
                    existing
                };
                state.insert(source, None);
                state.insert(target, Some(final_state));
                changed_files.push(ApplyPatchChangedFile {
                    action: ApplyPatchAction::Move,
                    path: to.to_string(),
                    from_path: Some(from.to_string()),
                });
            }
        }
    }

    let mut removals = Vec::new();
    let mut writes = Vec::new();
    let mut changed = BTreeSet::new();

    for (path, final_state) in state {
        let original_state = originals.get(&path).cloned().unwrap_or(None);
        match (original_state, final_state) {
            (Some(_), None) => {
                removals.push(path.clone());
                changed.insert(path.display().to_string());
            }
            (None, Some(state)) | (Some(_), Some(state)) => {
                writes.push((path.clone(), state.into_content()));
                changed.insert(path.display().to_string());
            }
            (None, None) => {}
        }
    }

    for path in &removals {
        if tokio::fs::try_exists(path).await.unwrap_or(false) {
            tokio::fs::remove_file(path)
                .await
                .with_context(|| format!("failed to remove {}", path.display()))?;
        }
    }

    for (path, content) in writes {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        tokio::fs::write(&path, content.as_bytes())
            .await
            .with_context(|| format!("failed to write {}", path.display()))?;
    }

    Ok((
        changed_files,
        changed.into_iter().collect(),
        ignored_metadata,
        diagnostics,
    ))
}

enum PatchOperationKind<'a> {
    Add {
        path: &'a str,
    },
    Delete {
        path: &'a str,
    },
    Modify {
        path: &'a str,
    },
    Rename {
        from: &'a str,
        to: &'a str,
        with_edit: bool,
    },
}

fn patch_operation_kind(patch: &FilePatch) -> Result<PatchOperationKind<'_>> {
    if let (Some(from), Some(to)) = (patch.rename_from.as_deref(), patch.rename_to.as_deref()) {
        return Ok(PatchOperationKind::Rename {
            from,
            to,
            with_edit: !patch.hunks.is_empty(),
        });
    }
    match (
        patch.old_path.as_workspace_path(),
        patch.new_path.as_workspace_path(),
    ) {
        (None, Some(path)) => Ok(PatchOperationKind::Add { path }),
        (Some(path), None) => Ok(PatchOperationKind::Delete { path }),
        (Some(old), Some(new)) if old == new => Ok(PatchOperationKind::Modify { path: old }),
        (Some(_), Some(_)) => Err(syntax_error(
            "missing_rename_header",
            "path-changing file patches must include rename from and rename to headers",
            patch
                .new_path
                .as_workspace_path()
                .or_else(|| patch.old_path.as_workspace_path()),
            "add diff --git plus rename from/rename to headers for file renames",
        )),
        (None, None) => Err(syntax_error(
            "missing_file_header",
            "both unified diff paths cannot be /dev/null",
            None,
            "use one workspace path for add or delete operations",
        )),
    }
}

async fn load_state(
    path: &Path,
    state: &mut HashMap<PathBuf, Option<FileState>>,
    originals: &mut HashMap<PathBuf, Option<FileState>>,
) -> Result<Option<FileState>> {
    if let Some(existing) = state.get(path) {
        return Ok(existing.clone());
    }

    let loaded = match tokio::fs::read_to_string(path).await {
        Ok(content) => Some(FileState::from_content(&content)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()))
        }
    };
    state.insert(path.to_path_buf(), loaded.clone());
    originals.insert(path.to_path_buf(), loaded.clone());
    Ok(loaded)
}

fn apply_hunks(path: &str, mut state: FileState, hunks: &[PatchHunk]) -> Result<FileState> {
    for hunk in hunks {
        let old_block = hunk
            .lines
            .iter()
            .filter(|line| line.kind != HunkLineKind::Add)
            .map(|line| line.text.clone())
            .collect::<Vec<_>>();
        let new_block = hunk
            .lines
            .iter()
            .filter(|line| line.kind != HunkLineKind::Remove)
            .map(|line| line.text.clone())
            .collect::<Vec<_>>();

        let index = if old_block.is_empty() {
            hunk.old_start.saturating_sub(1).min(state.lines.len())
        } else {
            find_unique_match(path, &state.lines, &old_block, hunk.old_start)?
        };
        state
            .lines
            .splice(index..index + old_block.len(), new_block.into_iter());
        if hunk.no_newline_at_end {
            state.trailing_newline = false;
        } else if !state.lines.is_empty() {
            state.trailing_newline = true;
        }
    }
    Ok(state)
}

fn find_unique_match(
    path: &str,
    lines: &[String],
    needle: &[String],
    hint: usize,
) -> Result<usize> {
    if needle.len() > lines.len() {
        return Err(context_not_found(path, needle));
    }

    let mut matches = Vec::new();
    for start in 0..=lines.len() - needle.len() {
        if lines[start..start + needle.len()] == *needle {
            matches.push(start);
        }
    }
    match matches.len() {
        0 => Err(context_not_found(path, needle)),
        1 => Ok(matches[0]),
        _ => Err(ambiguous_context(path, needle, lines, &matches, hint)),
    }
}

fn parse_accepted_metadata(line: &str, path: Option<&str>) -> Option<ApplyPatchIgnoredMetadata> {
    for kind in [
        "index",
        "similarity index",
        "new file mode",
        "deleted file mode",
        "old mode",
        "new mode",
    ] {
        let prefix = format!("{kind} ");
        if let Some(value) = line.strip_prefix(&prefix) {
            return Some(ApplyPatchIgnoredMetadata {
                path: path.unwrap_or("").to_string(),
                kind: kind.to_string(),
                value: value.to_string(),
            });
        }
    }
    None
}

fn is_unsupported_git_feature(line: &str) -> bool {
    line.starts_with("Binary files ")
        || line == "GIT binary patch"
        || line.starts_with("copy from ")
        || line.starts_with("copy to ")
        || line.starts_with("Subproject commit ")
}

fn strip_diff_prefix(path: &str) -> String {
    path.strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path)
        .to_string()
}

fn validate_rename_paths(
    rename_from: &str,
    rename_to: &str,
    git_old: &str,
    git_new: &str,
) -> Result<()> {
    if rename_from != git_old || rename_to != git_new {
        return Err(rename_path_mismatch(rename_from, rename_to));
    }
    Ok(())
}

fn validate_rename_file_headers(
    rename_from: &str,
    rename_to: &str,
    old_path: &PatchPath,
    new_path: &PatchPath,
) -> Result<()> {
    if old_path.as_workspace_path() != Some(rename_from)
        || new_path.as_workspace_path() != Some(rename_to)
    {
        return Err(rename_path_mismatch(rename_from, rename_to));
    }
    Ok(())
}

fn validate_git_file_headers(
    git_old: &str,
    git_new: &str,
    old_path: &PatchPath,
    new_path: &PatchPath,
) -> Result<()> {
    let old_matches = old_path
        .as_workspace_path()
        .map(|path| path == git_old)
        .unwrap_or(true);
    let new_matches = new_path
        .as_workspace_path()
        .map(|path| path == git_new)
        .unwrap_or(true);
    if !old_matches || !new_matches {
        return Err(syntax_error(
            "path_header_mismatch",
            "diff --git paths disagree with ---/+++ paths",
            new_path
                .as_workspace_path()
                .or_else(|| old_path.as_workspace_path()),
            "make diff --git, ---, and +++ paths refer to the same file patch",
        ));
    }
    Ok(())
}

fn syntax_error(
    kind: &'static str,
    message: impl Into<String>,
    path: Option<&str>,
    recovery_hint: impl Into<String>,
) -> anyhow::Error {
    let mut details = serde_json::json!({ "rule": kind });
    if let Some(path) = path {
        details["path"] = serde_json::Value::String(path.to_string());
    }
    anyhow::Error::from(
        ToolError::new("invalid_patch_syntax", message)
            .with_details(details)
            .with_recovery_hint(recovery_hint.into()),
    )
}

fn unsupported_git_patch_feature(line: &str, path: Option<&str>) -> anyhow::Error {
    let mut details = serde_json::json!({
        "line": line,
    });
    if let Some(path) = path {
        details["path"] = serde_json::Value::String(path.to_string());
    }
    anyhow::Error::from(
        ToolError::new(
            "unsupported_git_patch_feature",
            format!("unsupported git patch feature: {line}"),
        )
        .with_details(details)
        .with_recovery_hint(
            "submit a text-only unified diff without binary, copy, or submodule patch features",
        ),
    )
}

fn duplicate_file_patch(path: &str) -> anyhow::Error {
    anyhow::Error::from(
        ToolError::new(
            "duplicate_file_patch",
            format!("duplicate file patch for normalized path: {path}"),
        )
        .with_details(serde_json::json!({
            "path": path,
        }))
        .with_recovery_hint(
            "merge multiple hunks for the same file into one unified diff file patch",
        ),
    )
}

fn context_not_found(path: &str, needle: &[String]) -> anyhow::Error {
    anyhow::Error::from(
        ToolError::new(
            "context_not_found",
            format!("hunk context does not match current file: {path}"),
        )
        .with_details(serde_json::json!({
            "path": path,
            "expected_lines": needle.iter().take(3).cloned().collect::<Vec<_>>().join("\\n"),
        }))
        .with_recovery_hint(format!(
            "read the exact target region in {path} and submit a hunk with matching context"
        )),
    )
}

fn ambiguous_context(
    path: &str,
    needle: &[String],
    lines: &[String],
    matches: &[usize],
    hint: usize,
) -> anyhow::Error {
    let hint_index = hint.saturating_sub(1);

    // Report at most 10 candidates to avoid excessively long error messages
    let total_matches = matches.len();
    let candidates: Vec<serde_json::Value> = matches
        .iter()
        .take(10)
        .map(|&index| {
            // Include a bounded preview: 2 lines before and 2 lines after the match
            let preview_start = index.saturating_sub(2);
            let preview_end = (index + needle.len() + 2).min(lines.len());
            let preview_lines = lines[preview_start..preview_end]
                .iter()
                .map(|line| line.trim().to_string())
                .collect::<Vec<_>>();

            serde_json::json!({
                "line_number": index + 1,
                "distance_from_hint": index.abs_diff(hint_index),
                "preview": preview_lines,
                "preview_range": {
                    "start": preview_start + 1,
                    "end": preview_end,
                }
            })
        })
        .collect();

    let truncated = total_matches > 10;
    let candidate_lines = candidates
        .iter()
        .map(|c| {
            let line_num = &c["line_number"];
            let preview = c["preview"].as_array().unwrap();
            let preview_text = preview
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect::<Vec<_>>()
                .join(" / ");
            format!("  - Line {}: {}", line_num, preview_text)
        })
        .collect::<Vec<_>>()
        .join("\n");

    let candidates_text = if truncated {
        format!(
            "Showing first 10 of {} matching locations:\n{}",
            total_matches, candidate_lines
        )
    } else {
        format!(
            "The following locations all match the current context:\n{}",
            candidate_lines
        )
    };

    anyhow::Error::from(
        ToolError::new(
            "ambiguous_context",
            format!("hunk context matches {} locations in {path}", total_matches),
        )
        .with_details(serde_json::json!({
            "path": path,
            "expected_lines": needle.iter().take(3).cloned().collect::<Vec<_>>().join("\\n"),
            "candidate_count": total_matches,
            "reported_count": candidates.len(),
            "truncated": truncated,
            "candidates": candidates,
        }))
        .with_recovery_hint(format!(
            "include more surrounding context (at least 3 lines, or 5–10 if the file has repeated structures) \
             in the hunk for {path}. {}",
            candidates_text
        )),
    )
}

fn rename_path_mismatch(rename_from: &str, rename_to: &str) -> anyhow::Error {
    anyhow::Error::from(
        ToolError::new(
            "rename_path_mismatch",
            "rename headers disagree with diff path headers",
        )
        .with_details(serde_json::json!({
            "rename_from": rename_from,
            "rename_to": rename_to,
        }))
        .with_recovery_hint("make rename from/to agree with diff --git and ---/+++ paths"),
    )
}

fn rename_requires_git_header(rename_from: &str, rename_to: &str) -> anyhow::Error {
    anyhow::Error::from(
        ToolError::new(
            "missing_git_header",
            "rename patches must include a diff --git header",
        )
        .with_details(serde_json::json!({
            "rename_from": rename_from,
            "rename_to": rename_to,
        }))
        .with_recovery_hint("start rename patches with diff --git a/old_path b/new_path"),
    )
}

fn path_escape(path: &str) -> anyhow::Error {
    anyhow::Error::from(
        ToolError::new(
            "path_escape",
            format!("patch path escapes workspace root: {path}"),
        )
        .with_details(serde_json::json!({
            "path": path,
        }))
        .with_recovery_hint("use only workspace-relative paths inside the current execution root"),
    )
}

fn missing_file(action: &str, path: &str) -> anyhow::Error {
    anyhow::Error::from(
        ToolError::new(
            "missing_file",
            format!("cannot {action} missing file {path}"),
        )
        .with_details(serde_json::json!({
            "action": action,
            "path": path,
        }))
        .with_recovery_hint(format!(
            "read {path} first or adjust the patch so it targets an existing file"
        )),
    )
}

fn existing_file(path: &str) -> anyhow::Error {
    anyhow::Error::from(
        ToolError::new(
            "existing_file",
            format!("target file already exists: {path}"),
        )
        .with_details(serde_json::json!({
            "path": path,
        }))
        .with_recovery_hint(format!(
            "use a modify hunk for {path}, or choose a new add/rename target"
        )),
    )
}

fn map_workspace_path_error<T>(path: &str, result: Result<T>) -> Result<T> {
    result.map_err(|error| {
        if error.to_string().contains("path escapes workspace root") {
            path_escape(path)
        } else {
            error
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn apply_patch_updates_multiple_files_with_unified_diff() {
        let dir = tempdir().unwrap();
        let alpha = dir.path().join("alpha.txt");
        let beta = dir.path().join("beta.txt");
        tokio::fs::write(&alpha, "before\nshared\n").await.unwrap();
        tokio::fs::write(&beta, "keep\nold\n").await.unwrap();

        let patch = r#"--- a/alpha.txt
+++ b/alpha.txt
@@ -1,2 +1,2 @@
-before
+after
 shared
--- a/beta.txt
+++ b/beta.txt
@@ -1,2 +1,2 @@
 keep
-old
+new
"#;

        let outcome = apply_patch(dir.path(), patch).await.unwrap();
        assert_eq!(
            tokio::fs::read_to_string(&alpha).await.unwrap(),
            "after\nshared\n"
        );
        assert_eq!(
            tokio::fs::read_to_string(&beta).await.unwrap(),
            "keep\nnew\n"
        );
        assert_eq!(outcome.changed_files.len(), 2);
    }

    #[tokio::test]
    async fn apply_patch_adds_and_deletes_files_with_dev_null() {
        let dir = tempdir().unwrap();
        let doomed = dir.path().join("doomed.txt");
        tokio::fs::write(&doomed, "bye\n").await.unwrap();

        let patch = r#"--- /dev/null
+++ b/created.txt
@@ -0,0 +1,1 @@
+hello
--- a/doomed.txt
+++ /dev/null
@@ -1,1 +0,0 @@
-bye
"#;

        apply_patch(dir.path(), patch).await.unwrap();
        assert_eq!(
            tokio::fs::read_to_string(dir.path().join("created.txt"))
                .await
                .unwrap(),
            "hello\n"
        );
        assert!(!tokio::fs::try_exists(&doomed).await.unwrap());
    }

    #[tokio::test]
    async fn apply_patch_supports_rename_only() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("old.txt");
        tokio::fs::write(&source, "hello\n").await.unwrap();

        let patch = r#"diff --git a/old.txt b/new.txt
similarity index 100%
rename from old.txt
rename to new.txt
"#;

        let outcome = apply_patch(dir.path(), patch).await.unwrap();
        assert!(!tokio::fs::try_exists(&source).await.unwrap());
        assert_eq!(
            tokio::fs::read_to_string(dir.path().join("new.txt"))
                .await
                .unwrap(),
            "hello\n"
        );
        assert_eq!(outcome.changed_files[0].action, ApplyPatchAction::Move);
        assert_eq!(outcome.ignored_metadata[0].kind, "similarity index");
        assert_eq!(outcome.ignored_metadata[0].path, "new.txt");
    }

    #[tokio::test]
    async fn apply_patch_supports_rename_with_edit() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("old.txt");
        tokio::fs::write(&source, "hello\n").await.unwrap();

        let patch = r#"diff --git a/old.txt b/new.txt
rename from old.txt
rename to new.txt
--- a/old.txt
+++ b/new.txt
@@ -1,1 +1,1 @@
-hello
+world
"#;

        apply_patch(dir.path(), patch).await.unwrap();
        assert!(!tokio::fs::try_exists(&source).await.unwrap());
        assert_eq!(
            tokio::fs::read_to_string(dir.path().join("new.txt"))
                .await
                .unwrap(),
            "world\n"
        );
    }

    #[tokio::test]
    async fn apply_patch_line_number_drift_still_matches_unique_context() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("sample.txt");
        tokio::fs::write(&file, "one\ntwo\nthree\n").await.unwrap();

        let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@ -42,1 +42,1 @@
-two
+TWO
"#;

        apply_patch(dir.path(), patch).await.unwrap();
        assert_eq!(
            tokio::fs::read_to_string(&file).await.unwrap(),
            "one\nTWO\nthree\n"
        );
    }

    #[tokio::test]
    async fn apply_patch_hunk_count_mismatch_is_diagnostic_not_failure() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("sample.txt");
        tokio::fs::write(&file, "one\ntwo\n").await.unwrap();

        let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@ -1,99 +1,99 @@
-one
+ONE
 two
"#;

        let outcome = apply_patch(dir.path(), patch).await.unwrap();
        assert_eq!(outcome.diagnostics[0].kind, "hunk_count_mismatch");
        assert_eq!(
            tokio::fs::read_to_string(&file).await.unwrap(),
            "ONE\ntwo\n"
        );
    }

    #[tokio::test]
    async fn apply_patch_rejects_context_not_found_without_partial_writes() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("sample.txt");
        tokio::fs::write(&file, "hello\nworld\n").await.unwrap();

        let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@ -1,1 +1,1 @@
-missing
+present
"#;

        let error = apply_patch(dir.path(), patch).await.unwrap_err();
        let tool_error = ToolError::from_anyhow(&error);
        assert_eq!(tool_error.kind, "context_not_found");
        assert_eq!(
            tokio::fs::read_to_string(&file).await.unwrap(),
            "hello\nworld\n"
        );
    }

    #[tokio::test]
    async fn apply_patch_rejects_ambiguous_context() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("sample.txt");
        tokio::fs::write(&file, "same\nx\nsame\n").await.unwrap();

        let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@ -1,1 +1,1 @@
-same
+changed
"#;

        let error = apply_patch(dir.path(), patch).await.unwrap_err();
        let tool_error = ToolError::from_anyhow(&error);
        assert_eq!(tool_error.kind, "ambiguous_context");
    }

    #[tokio::test]
    async fn apply_patch_rejects_path_escape() {
        let dir = tempdir().unwrap();
        let patch = r#"--- /dev/null
+++ b/../escape.txt
@@ -0,0 +1,1 @@
+bad
"#;

        let error = apply_patch(dir.path(), patch).await.unwrap_err();
        let tool_error = ToolError::from_anyhow(&error);
        assert_eq!(tool_error.kind, "path_escape");
    }

    #[tokio::test]
    async fn apply_patch_rejects_duplicate_normalized_file_patch() {
        let dir = tempdir().unwrap();
        tokio::fs::write(dir.path().join("sample.txt"), "one\n")
            .await
            .unwrap();

        let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@ -1,1 +1,1 @@
-one
+two
--- a/./sample.txt
+++ b/./sample.txt
@@ -1,1 +1,1 @@
-two
+three
"#;

        let error = apply_patch(dir.path(), patch).await.unwrap_err();
        let tool_error = ToolError::from_anyhow(&error);
        assert_eq!(tool_error.kind, "duplicate_file_patch");
    }

    #[test]
    fn parse_patch_rejects_legacy_dsl() {
        let error = parse_patch("*** Begin Patch\n*** End Patch\n").unwrap_err();
        let tool_error = ToolError::from_anyhow(&error);
        assert_eq!(tool_error.kind, "invalid_patch_syntax");
        assert_eq!(
            tool_error.details.as_ref().unwrap()["rule"],
            "legacy_patch_format"
        );
    }

    #[test]
    fn parse_patch_rejects_unsupported_binary_patch() {
        let error = parse_patch(
            "diff --git a/image.png b/image.png\nBinary files a/image.png and b/image.png differ\n",
        )
        .unwrap_err();
        let tool_error = ToolError::from_anyhow(&error);
        assert_eq!(tool_error.kind, "unsupported_git_patch_feature");
    }

    #[test]
    fn parse_patch_rejects_unsupported_copy_and_submodule_patch() {
        for patch in [
            "diff --git a/old.txt b/new.txt\ncopy from old.txt\ncopy to new.txt\n",
            "diff --git a/sub b/sub\nSubproject commit abc123\n",
        ] {
            let error = parse_patch(patch).unwrap_err();
            let tool_error = ToolError::from_anyhow(&error);
            assert_eq!(tool_error.kind, "unsupported_git_patch_feature");
        }
    }

    #[test]
    fn parse_patch_rejects_rename_path_mismatch() {
        let error = parse_patch(
            "diff --git a/old.txt b/new.txt\nrename from other.txt\nrename to new.txt\n",
        )
        .unwrap_err();
        let tool_error = ToolError::from_anyhow(&error);
        assert_eq!(tool_error.kind, "rename_path_mismatch");
    }

    #[test]
    fn parse_patch_rejects_rename_without_git_header() {
        let error = parse_patch(
            "rename from old.txt\nrename to new.txt\n--- a/old.txt\n+++ b/new.txt\n@@ -1,1 +1,1 @@\n-old\n+new\n",
        )
        .unwrap_err();
        let tool_error = ToolError::from_anyhow(&error);
        assert_eq!(tool_error.kind, "missing_git_header");
    }

    mod proptests {
        use super::*;
        use proptest::prelude::*;
        use std::sync::OnceLock;

        fn runtime() -> &'static tokio::runtime::Runtime {
            static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
            RUNTIME.get_or_init(|| tokio::runtime::Runtime::new().unwrap())
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(32))]

            #[test]
            fn prop_add_file_creates_normalized_workspace_path(
                segments in prop::collection::vec("[a-zA-Z0-9_-]{1,16}", 1..4),
                content in "[a-zA-Z0-9 _-]{1,80}"
            ) {
                let dir = tempdir().unwrap();
                let path = format!("{}.txt", segments.join("/"));
                let expected_content = format!("{content}\n");

                let patch = format!(
                    r#"--- /dev/null
+++ b/{path}
@@ -0,0 +1,1 @@
+{content}
"#);

                runtime()
                    .block_on(apply_patch(dir.path(), &patch))
                    .unwrap();

                prop_assert_eq!(
                    std::fs::read_to_string(dir.path().join(path)).unwrap(),
                    expected_content
                );
            }

            #[test]
            fn prop_path_escape_rejects_generated_escape_paths(
                file_name in "[a-zA-Z0-9_-]{1,20}"
            ) {
                let dir = tempdir().unwrap();
                let patch = format!(
                    r#"--- /dev/null
+++ b/../{file_name}.txt
@@ -0,0 +1,1 @@
+bad
"#
                );

                let error = runtime()
                    .block_on(apply_patch(dir.path(), &patch))
                    .unwrap_err();
                let tool_error = ToolError::from_anyhow(&error);
                prop_assert_eq!(tool_error.kind, "path_escape");
            }

            #[test]
            fn prop_single_line_modify_replaces_only_target_content(
                original in "[a-zA-Z0-9 _-]{1,80}",
                replacement in "[a-zA-Z0-9 _-]{1,80}"
            ) {
                prop_assume!(original != replacement);

                let dir = tempdir().unwrap();
                let file = dir.path().join("test.txt");
                let original_content = format!("{original}\n");
                let replacement_content = format!("{replacement}\n");
                std::fs::write(&file, &original_content).unwrap();

                let patch = format!(
                    r#"--- a/test.txt
+++ b/test.txt
@@ -1,1 +1,1 @@
-{original}
+{replacement}
"#
                );

                runtime()
                    .block_on(apply_patch(dir.path(), &patch))
                    .unwrap();

                prop_assert_eq!(std::fs::read_to_string(&file).unwrap(), replacement_content);
            }

            #[test]
            fn prop_patch_roundtrip_returns_to_original(
                initial_lines in prop::collection::vec("[a-zA-Z0-9_]{1,30}", 1..10),
                modify_line_index in 0usize..10,
                old_line in "[a-zA-Z0-9_]{1,30}",
                new_line in "[a-zA-Z0-9_]{1,30}",
            ) {
                prop_assume!(modify_line_index < initial_lines.len());
                prop_assume!(old_line != new_line);

                let dir = tempdir().unwrap();
                let file_path = dir.path().join("test.txt");

                let content_vec = initial_lines.clone();

                // Ensure old_line is not already in the content (to avoid ambiguous matches)
                let old_line_count = content_vec.iter().filter(|&line| line == &old_line).count();
                prop_assume!(old_line_count == 0);

                // Ensure new_line is not already in the content (to avoid ambiguous reverse matches)
                let new_line_count = content_vec.iter().filter(|&line| line == &new_line).count();
                prop_assume!(new_line_count == 0);

                let mut content_lines = content_vec;
                content_lines[modify_line_index] = old_line.clone();
                let original_content = content_lines.join("\n") + "\n";
                std::fs::write(&file_path, &original_content).unwrap();

                let old_start = modify_line_index + 1;
                let forward_patch = format!(
                    r#"--- a/test.txt
+++ b/test.txt
@@ -{old_start},1 +{old_start},1 @@
-{old_line}
+{new_line}
"#
                );

                runtime()
                    .block_on(apply_patch(dir.path(), &forward_patch))
                    .unwrap();

                let reverse_patch = format!(
                    r#"--- a/test.txt
+++ b/test.txt
@@ -{old_start},1 +{old_start},1 @@
-{new_line}
+{old_line}
"#
                );

                runtime()
                    .block_on(apply_patch(dir.path(), &reverse_patch))
                    .unwrap();

                let final_content = std::fs::read_to_string(&file_path).unwrap();
                prop_assert_eq!(final_content, original_content);
            }

            #[test]
            fn prop_context_matching_robustness(
                base_lines in prop::collection::vec("[a-zA-Z0-9_]{20,40}", 15..30),
                target_line in 5usize..15,
                replacement in "[a-zA-Z0-9_]{5,20}",
            ) {
                prop_assume!(base_lines[target_line] != replacement);

                let dir = tempdir().unwrap();
                let file_path = dir.path().join("test.txt");

                let original_content = base_lines.join("\n") + "\n";
                std::fs::write(&file_path, &original_content).unwrap();

                let context_start = target_line.saturating_sub(2);
                let context_end = (target_line + 3).min(base_lines.len());
                let old_start = context_start + 1;
                let old_count = context_end - context_start;

                // Ensure the old block occurs exactly once to avoid ambiguous_context errors
                let old_block = &base_lines[context_start..context_end];
                let old_block_occurrences = base_lines
                    .windows(old_block.len())
                    .filter(|window| *window == old_block)
                    .count();
                prop_assume!(old_block_occurrences == 1);

                let mut hunk_lines = Vec::new();
                for i in context_start..target_line {
                    hunk_lines.push(format!(" {}", &base_lines[i]));
                }
                hunk_lines.push(format!("-{}", &base_lines[target_line]));
                hunk_lines.push(format!("+{}", &replacement));
                for i in (target_line + 1)..context_end {
                    if i < base_lines.len() {
                        hunk_lines.push(format!(" {}", &base_lines[i]));
                    }
                }

                let patch = format!(
                    r#"--- a/test.txt
+++ b/test.txt
@@ -{old_start},{old_count} +{old_start},{old_count} @@
{}"#,
                    hunk_lines.join("\n")
                );

                let result = runtime()
                    .block_on(apply_patch(dir.path(), &patch));

                prop_assert!(result.is_ok());
                let final_content = std::fs::read_to_string(&file_path).unwrap();
                let final_lines: Vec<&str> = final_content.lines().collect();
                prop_assert_eq!(final_lines[target_line], replacement);
            }
        }
    }

    #[tokio::test]
    async fn apply_patch_rejects_ambiguous_context_with_repeated_blocks() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("sample.txt");
        tokio::fs::write(&file, "same\nx\nsame\ny\nsame\nz\nsame\n")
            .await
            .unwrap();

        let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@ -1,1 +1,1 @@
-same
+changed
"#;

        let error = apply_patch(dir.path(), patch).await.unwrap_err();
        let tool_error = ToolError::from_anyhow(&error);
        assert_eq!(tool_error.kind, "ambiguous_context");

        // candidates must exist and have correct count ("same" occurs 4 times)
        let details = tool_error.details.as_ref().unwrap();
        let candidates = details["candidates"].as_array().unwrap();
        assert_eq!(candidates.len(), 4);
        // each candidate must have line_number and distance_from_hint fields
        for c in candidates {
            assert!(c["line_number"].is_number());
            assert!(c["distance_from_hint"].is_number());
        }
    }

    #[tokio::test]
    async fn apply_patch_reports_candidates_near_hint() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("sample.txt");
        tokio::fs::write(&file, "repeat\nline\nrepeat\nline\nrepeat\n")
            .await
            .unwrap();

        let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@ -2,1 +2,1 @@
-repeat
+changed
"#;

        let error = apply_patch(dir.path(), patch).await.unwrap_err();
        let tool_error = ToolError::from_anyhow(&error);
        assert_eq!(tool_error.kind, "ambiguous_context");

        // "repeat" occurs 3 times in the file
        let details = tool_error.details.as_ref().unwrap();
        let candidates = details["candidates"].as_array().unwrap();
        assert_eq!(candidates.len(), 3);

        // hint is line 2 (which is "line"), but "repeat" is at lines 1, 3, 5
        // So the closest candidate to line 2 is line 1 (index 0), which should have distance 1
        // Actually, the distance calculation is: |candidate_index - hint_index|
        // hint_index = 2 - 1 = 1, candidates are at indices 0, 2, 4
        // distances are |0-1|=1, |2-1|=1, |4-1|=3
        // We should find a candidate with distance 1, not 0
        assert!(candidates.iter().any(|c| c["distance_from_hint"] == 1));
    }

    #[tokio::test]
    async fn apply_patch_handles_stale_line_hint() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("sample.txt");
        tokio::fs::write(&file, "pattern\na\nb\nc\npattern\nx\ny\nz\n")
            .await
            .unwrap();

        // hint points to line 42 (far beyond file range), but should still return all candidates
        let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@ -42,1 +42,1 @@
-pattern
+changed
"#;

        let error = apply_patch(dir.path(), patch).await.unwrap_err();
        let tool_error = ToolError::from_anyhow(&error);
        assert_eq!(tool_error.kind, "ambiguous_context");

        // "pattern" occurs 2 times in the file
        let details = tool_error.details.as_ref().unwrap();
        let candidates = details["candidates"].as_array().unwrap();
        assert_eq!(candidates.len(), 2);
    }

    #[tokio::test]
    async fn apply_patch_succeeds_with_unique_context() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("sample.txt");
        tokio::fs::write(&file, "unique_before\nunique_target\nunique_after\n")
            .await
            .unwrap();

        let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@ -1,3 +1,3 @@
 unique_before
-unique_target
+CHANGED
 unique_after
"#;

        apply_patch(dir.path(), patch).await.unwrap();
        let content = tokio::fs::read_to_string(&file).await.unwrap();
        assert_eq!(content, "unique_before\nCHANGED\nunique_after\n");
    }

    #[tokio::test]
    async fn apply_patch_rejects_empty_hunk_line_without_prefix() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("sample.txt");
        tokio::fs::write(&file, "line1\nline2\nline3\n")
            .await
            .unwrap();

        // Patch with an empty line that lacks the required space prefix
        let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@ -1,3 +1,3 @@
 line1

+line2_modified
 line3
"#;

        let error = apply_patch(dir.path(), patch).await.unwrap_err();
        let tool_error = ToolError::from_anyhow(&error);

        // Verify the syntax error category and specific rule
        assert_eq!(tool_error.kind, "invalid_patch_syntax");
        let details = tool_error.details.as_ref().unwrap();
        assert_eq!(details["rule"], "invalid_hunk_empty_line");

        // Verify the error message contains helpful context
        let error_message = tool_error.message.to_lowercase();
        assert!(
            error_message.contains("empty"),
            "error should mention empty line"
        );
        assert!(
            error_message.contains("prefix"),
            "error should mention prefix requirement"
        );
        assert!(
            error_message.contains("space"),
            "error should suggest space character"
        );
    }
}
