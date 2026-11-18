use anyhow::{Context, Result};
use holon_system::resource_material;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use super::helpers::normalize_path;
use crate::{
    tool::ToolError,
    types::{
        ApplyPatchAction, ApplyPatchChangedFile, ApplyPatchDiagnostic, ApplyPatchHunkSummary,
        ApplyPatchIgnoredMetadata,
    },
};

const DIFF_PREVIEW_MAX_LINES: usize = 80;
const CODEX_END_PATCH_SENTINEL: &str = "*** End Patch";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyPatchOutcome {
    pub changed_files: Vec<ApplyPatchChangedFile>,
    pub changed_paths: Vec<String>,
    pub ignored_metadata: Vec<ApplyPatchIgnoredMetadata>,
    pub diagnostics: Vec<ApplyPatchDiagnostic>,
}

pub use holon_domain::ApplyPatchSurface;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchFormat {
    CodexDsl,
    UnifiedDiff,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PatchCompatibility {
    CrossFormat {
        detected_format: PatchFormat,
        strict_failure_kind: String,
    },
    TrailingSentinel {
        strict_failure_kind: String,
    },
}

impl PatchFormat {
    fn label(self) -> &'static str {
        match self {
            Self::CodexDsl => "codex_dsl",
            Self::UnifiedDiff => "unified_diff",
            Self::Unknown => "unknown",
        }
    }
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
    fn from_content(content: &str) -> Result<Self> {
        if content.is_empty() {
            return Ok(Self {
                lines: Vec::new(),
                trailing_newline: false,
            });
        }

        charge_lines(
            content.len(),
            content.bytes().filter(|byte| *byte == b'\n').count() + 1,
        )?;
        let trailing_newline = content.ends_with('\n');
        let mut lines = content
            .split('\n')
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        if trailing_newline {
            lines.pop();
        }
        Ok(Self {
            lines,
            trailing_newline,
        })
    }

    fn try_clone(&self) -> Result<Self> {
        charge_lines(self.lines.iter().map(String::len).sum(), self.lines.len())?;
        Ok(self.clone())
    }

    fn into_content(self) -> Result<String> {
        resource_material::charge_owned_payload(
            self.lines.iter().map(String::len).sum::<usize>() as u64 + self.lines.len() as u64 + 1,
        )?;
        let mut content = self.lines.join("\n");
        if self.trailing_newline && !content.is_empty() {
            content.push('\n');
        }
        Ok(content)
    }
}

fn charge_lines(bytes: usize, lines: usize) -> Result<()> {
    resource_material::charge_owned_payload(bytes.try_into()?)?;
    resource_material::charge_owned_metadata(
        (lines as u64)
            .checked_mul(64)
            .ok_or_else(|| anyhow::anyhow!("patch line material overflow"))?,
        lines,
    )?;
    Ok(())
}

/// Runtime policy for reading files and preserving diagnostic error labels.
pub struct PatchHost {
    pub read_text: fn(&Path) -> Result<std::io::Result<String>>,
    pub error_kind: fn(&anyhow::Error) -> String,
}

pub fn apply_patch<W>(
    workspace_root: &Path,
    input: &str,
    surface: ApplyPatchSurface,
    host: &PatchHost,
    begin_write: impl FnOnce(Vec<PathBuf>) -> Result<W>,
) -> Result<ApplyPatchOutcome> {
    // Covers the original parser's simultaneous line/path/hunk representations.
    // File bodies and execution copies are admitted separately from actual data.
    charge_lines(
        input
            .len()
            .checked_mul(8)
            .ok_or_else(|| anyhow::anyhow!("patch parser material overflow"))?,
        input
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count()
            .saturating_add(1)
            .checked_mul(8)
            .ok_or_else(|| anyhow::anyhow!("patch parser records overflow"))?,
    )?;
    let expected_format = match surface {
        ApplyPatchSurface::CodexDslFreeform => PatchFormat::CodexDsl,
        ApplyPatchSurface::UnifiedDiffJson => PatchFormat::UnifiedDiff,
    };
    let patch_bytes = input.len();
    let parse = parse_patch_for_format(input, expected_format);
    let (patches, compatibility) = match parse {
        Ok(patches) => (patches, None),
        Err(strict_error) => {
            let detected = detect_patch_format(input);
            if detected != PatchFormat::Unknown && detected != expected_format {
                match parse_patch_for_format(input, detected) {
                    Ok(patches) => {
                        let strict_failure_kind = (host.error_kind)(&strict_error);
                        (
                            patches,
                            Some(PatchCompatibility::CrossFormat {
                                detected_format: detected,
                                strict_failure_kind,
                            }),
                        )
                    }
                    Err(_) => return Err(strict_error),
                }
            } else if surface == ApplyPatchSurface::UnifiedDiffJson
                && detected == PatchFormat::UnifiedDiff
            {
                match parse_unified_diff_with_trailing_sentinel(input)? {
                    Some(patches) => {
                        let strict_failure_kind = (host.error_kind)(&strict_error);
                        (
                            patches,
                            Some(PatchCompatibility::TrailingSentinel {
                                strict_failure_kind,
                            }),
                        )
                    }
                    None => return Err(strict_error),
                }
            } else {
                return Err(strict_error);
            }
        }
    };
    let file_count = patches.len();
    let hunk_count = patches.iter().map(|patch| patch.hunks.len()).sum::<usize>();
    let (changed_files, touched, ignored_metadata, diagnostics) =
        apply_file_patches(workspace_root, &patches, host.read_text, begin_write)?;
    let mut diagnostics = diagnostics;
    if let Some(compatibility) = compatibility {
        let diagnostic = match compatibility {
            PatchCompatibility::CrossFormat {
                detected_format,
                strict_failure_kind,
            } => ApplyPatchDiagnostic {
                path: String::new(),
                kind: "apply_patch_compatibility_fallback".to_string(),
                message: format!(
                    "ApplyPatch succeeded using {}, but this turn expects {}. Continue using {} for future ApplyPatch calls. surface={}, expected_format={}, detected_format={}, parser_mode=compatibility, compatibility_fallback_used=true, patch_bytes={}, file_count={}, hunk_count={}, strict_parse_failure_kind={strict_failure_kind}",
                    detected_format.label(),
                    expected_format.label(),
                    expected_format.label(),
                    surface.label(),
                    expected_format.label(),
                    detected_format.label(),
                    patch_bytes,
                    file_count,
                    hunk_count,
                ),
            },
            PatchCompatibility::TrailingSentinel {
                strict_failure_kind,
            } => ApplyPatchDiagnostic {
                path: String::new(),
                kind: "apply_patch_trailing_sentinel_ignored".to_string(),
                message: format!(
                    "ApplyPatch ignored one trailing patch sentinel. surface={}, expected_format={}, detected_format={}, parser_mode=trailing_sentinel_compatibility, trailing_sentinel_ignored=true, patch_bytes={}, file_count={}, hunk_count={}, strict_parse_failure_kind={strict_failure_kind}",
                    surface.label(),
                    expected_format.label(),
                    PatchFormat::UnifiedDiff.label(),
                    patch_bytes,
                    file_count,
                    hunk_count,
                ),
            },
        };
        diagnostics.push(diagnostic);
    }
    Ok(ApplyPatchOutcome {
        changed_files,
        changed_paths: touched,
        ignored_metadata,
        diagnostics,
    })
}

fn parse_patch_for_format(input: &str, format: PatchFormat) -> Result<Vec<FilePatch>> {
    match format {
        PatchFormat::CodexDsl => parse_codex_dsl_patch(input),
        PatchFormat::UnifiedDiff => parse_unified_diff_patch(input),
        PatchFormat::Unknown => Err(syntax_error(
            "unknown_patch_format",
            "unknown ApplyPatch format",
            None,
            "submit the patch format advertised for this turn",
        )),
    }
}

fn detect_patch_format(input: &str) -> PatchFormat {
    let trimmed = input.trim_start();
    if trimmed.starts_with("*** Begin Patch")
        || codex_dsl_lines(input)
            .first()
            .is_some_and(|line| line == "*** Begin Patch")
    {
        return PatchFormat::CodexDsl;
    }
    let mut has_old = false;
    let mut has_new = false;
    let mut has_hunk = false;
    for line in trimmed.lines().take(40) {
        has_old |= line.starts_with("--- ");
        has_new |= line.starts_with("+++ ");
        has_hunk |= is_hunk_header_line(line);
    }
    if has_old && has_new && has_hunk {
        PatchFormat::UnifiedDiff
    } else {
        PatchFormat::Unknown
    }
}

fn parse_unified_diff_with_trailing_sentinel(input: &str) -> Result<Option<Vec<FilePatch>>> {
    let lines = input.lines().collect::<Vec<_>>();
    let candidate_lines = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| line.starts_with(CODEX_END_PATCH_SENTINEL).then_some(index))
        .collect::<Vec<_>>();
    if candidate_lines.is_empty() {
        return Ok(None);
    }

    let sentinel_line = candidate_lines[0];
    let is_unique_exact_trailing_sentinel = candidate_lines.len() == 1
        && lines[sentinel_line] == CODEX_END_PATCH_SENTINEL
        && sentinel_line + 1 == lines.len();
    if !is_unique_exact_trailing_sentinel {
        return Err(unexpected_patch_sentinel(sentinel_line + 1));
    }

    let without_final_newline = input
        .strip_suffix("\r\n")
        .or_else(|| input.strip_suffix('\n'))
        .unwrap_or(input);
    let Some(stripped) = without_final_newline.strip_suffix(CODEX_END_PATCH_SENTINEL) else {
        return Err(unexpected_patch_sentinel(sentinel_line + 1));
    };
    parse_unified_diff_patch(stripped).map(Some)
}

fn parse_unified_diff_patch(input: &str) -> Result<Vec<FilePatch>> {
    if detect_patch_format(input) == PatchFormat::CodexDsl {
        return Err(syntax_error(
            "wrong_patch_format",
            "this turn expects a unified diff in the JSON patch field",
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
            if !is_hunk_header_line(&lines[index]) {
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
            let (hunk, consumed, hunk_diagnostics) = parse_hunk(&lines[index..], index, hunk_path)?;
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

fn parse_codex_dsl_patch(input: &str) -> Result<Vec<FilePatch>> {
    let lines = codex_dsl_lines(input);
    if lines.first().map(String::as_str) != Some("*** Begin Patch") {
        return Err(syntax_error(
            "wrong_patch_format",
            "this turn expects Codex *** Begin Patch DSL",
            None,
            "submit raw *** Begin Patch / *** End Patch text without JSON wrapping",
        ));
    }
    if lines.last().map(String::as_str) != Some("*** End Patch") {
        return Err(syntax_error(
            "missing_end_patch",
            "Codex DSL patch must end with *** End Patch",
            None,
            "end the patch with *** End Patch",
        ));
    }

    let mut patches = Vec::new();
    let mut index = 1usize;
    while index + 1 < lines.len() {
        let line = &lines[index];
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            index += 1;
            let mut hunk_lines = Vec::new();
            while index + 1 < lines.len() && !lines[index].starts_with("*** ") {
                let Some(text) = lines[index].strip_prefix('+') else {
                    return Err(syntax_error(
                        "invalid_codex_add_line",
                        "Codex Add File lines must start with +",
                        Some(path),
                        "prefix every added line with +",
                    ));
                };
                hunk_lines.push(HunkLine {
                    kind: HunkLineKind::Add,
                    text: text.to_string(),
                });
                index += 1;
            }
            if hunk_lines.is_empty() {
                return Err(syntax_error(
                    "missing_hunk",
                    "Codex Add File must include at least one added line",
                    Some(path),
                    "add one or more + lines",
                ));
            }
            patches.push(FilePatch {
                old_path: PatchPath::DevNull,
                new_path: PatchPath::Workspace(path.to_string()),
                rename_from: None,
                rename_to: None,
                hunks: vec![PatchHunk {
                    old_start: 0,
                    old_count: 0,
                    new_start: 1,
                    new_count: hunk_lines.len(),
                    lines: hunk_lines,
                    no_newline_at_end: false,
                }],
                ignored_metadata: Vec::new(),
                diagnostics: Vec::new(),
            });
            continue;
        }

        if let Some(path) = line.strip_prefix("*** Delete File: ") {
            patches.push(FilePatch {
                old_path: PatchPath::Workspace(path.to_string()),
                new_path: PatchPath::DevNull,
                rename_from: None,
                rename_to: None,
                hunks: Vec::new(),
                ignored_metadata: Vec::new(),
                diagnostics: Vec::new(),
            });
            index += 1;
            continue;
        }

        if let Some(path) = line.strip_prefix("*** Update File: ") {
            index += 1;
            let mut rename_to = None;
            if index + 1 < lines.len() {
                if let Some(move_to) = lines[index].strip_prefix("*** Move to: ") {
                    rename_to = Some(move_to.to_string());
                    index += 1;
                }
            }
            let mut hunks = Vec::new();
            let mut current = Vec::new();
            let mut no_newline_at_end = false;
            while index + 1 < lines.len() {
                let line = &lines[index];
                if line == "*** End of File" {
                    no_newline_at_end = true;
                    index += 1;
                    continue;
                }
                if line.starts_with("*** ") {
                    break;
                }
                if line == "@@" || line.starts_with("@@ ") {
                    if !current.is_empty() {
                        hunks.push(codex_dsl_hunk(
                            std::mem::take(&mut current),
                            no_newline_at_end,
                        ));
                        no_newline_at_end = false;
                    }
                    index += 1;
                    continue;
                }
                let Some(first) = line.chars().next() else {
                    return Err(syntax_error(
                        "invalid_codex_change_line",
                        "Codex change lines must start with space, +, or -",
                        Some(path),
                        "prefix blank context lines with a space",
                    ));
                };
                let kind = match first {
                    ' ' => HunkLineKind::Context,
                    '+' => HunkLineKind::Add,
                    '-' => HunkLineKind::Remove,
                    _ => {
                        return Err(syntax_error(
                            "invalid_codex_change_line",
                            format!(
                                "Codex change line must start with space, +, or -, got: {line}"
                            ),
                            Some(path),
                            "use only context, added, and removed lines inside update chunks",
                        ))
                    }
                };
                current.push(HunkLine {
                    kind,
                    text: line[1..].to_string(),
                });
                index += 1;
            }
            if !current.is_empty() {
                hunks.push(codex_dsl_hunk(current, no_newline_at_end));
            }
            if hunks.is_empty() && rename_to.is_none() {
                return Err(syntax_error(
                    "missing_hunk",
                    "Codex Update File must include a change or Move to",
                    Some(path),
                    "add change lines or a *** Move to header",
                ));
            }
            let target = rename_to.clone().unwrap_or_else(|| path.to_string());
            patches.push(FilePatch {
                old_path: PatchPath::Workspace(path.to_string()),
                new_path: PatchPath::Workspace(target),
                rename_from: rename_to.as_ref().map(|_| path.to_string()),
                rename_to,
                hunks,
                ignored_metadata: Vec::new(),
                diagnostics: Vec::new(),
            });
            continue;
        }

        return Err(syntax_error(
            "invalid_codex_hunk",
            format!("unexpected Codex DSL line: {line}"),
            None,
            "use *** Add File, *** Delete File, or *** Update File hunks",
        ));
    }

    if patches.is_empty() {
        return Err(syntax_error(
            "missing_hunk",
            "Codex DSL patch must include at least one file hunk",
            None,
            "add a file hunk between Begin Patch and End Patch",
        ));
    }
    Ok(patches)
}

fn codex_dsl_lines(input: &str) -> Vec<String> {
    let mut lines = input.lines().map(ToString::to_string).collect::<Vec<_>>();
    while lines
        .first()
        .map(|line| line.trim().is_empty())
        .unwrap_or(false)
    {
        lines.remove(0);
    }
    while lines
        .last()
        .map(|line| line.trim().is_empty())
        .unwrap_or(false)
    {
        lines.pop();
    }
    if lines
        .first()
        .map(|line| line.trim_start())
        .is_some_and(|line| line == "<<EOF" || line == "<<'EOF'" || line == "<<\"EOF\"")
        && lines.last().map(String::as_str) == Some("EOF")
    {
        lines.remove(0);
        lines.pop();
    }
    lines
}

fn codex_dsl_hunk(lines: Vec<HunkLine>, no_newline_at_end: bool) -> PatchHunk {
    let old_count = lines
        .iter()
        .filter(|line| line.kind != HunkLineKind::Add)
        .count();
    let new_count = lines
        .iter()
        .filter(|line| line.kind != HunkLineKind::Remove)
        .count();
    PatchHunk {
        old_start: 1,
        old_count,
        new_start: 1,
        new_count,
        lines,
        no_newline_at_end,
    }
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
    start_index: usize,
    path: Option<&str>,
) -> Result<(PatchHunk, usize, Vec<ApplyPatchDiagnostic>)> {
    let header = parse_hunk_header(&lines[0], path)?;
    let mut consumed = 1usize;
    let mut hunk_lines: Vec<HunkLine> = Vec::new();
    let mut no_newline_at_end = false;

    while consumed < lines.len() {
        let line = lines[consumed].as_str();
        if is_hunk_header_line(line)
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

            return Err(syntax_error_at(
                "invalid_hunk_empty_line",
                format!(
                    "hunk line {hunk_line_num} is empty; all hunk lines must have a prefix (space for context, + for added, - for removed). Context:\n{context_lines}\n---\nFix: add a space character to this blank line.",
                ),
                path,
                Some(start_index + consumed + 1),
                "ensure all blank lines within hunk sections have a space prefix",
            ));
        };
        let kind = match first {
            ' ' => HunkLineKind::Context,
            '+' => HunkLineKind::Add,
            '-' => HunkLineKind::Remove,
            _ => {
                return Err(syntax_error_at(
                    "invalid_hunk_line",
                    format!("hunk line must start with space, +, or -, got: {line}"),
                    path,
                    Some(start_index + consumed + 1),
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
    let (old_start, old_count, new_start, new_count) = match header {
        HunkHeader::Full {
            old_start,
            old_count,
            new_start,
            new_count,
        } => {
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
            (old_start, old_count, new_start, new_count)
        }
        // Bare `@@` / `@@ <description>`: counts derive from the hunk body
        // and the matcher runs without a line hint, like Codex DSL hunks.
        HunkHeader::Bare => {
            if hunk_lines.iter().all(|line| line.kind == HunkLineKind::Add) {
                return Err(syntax_error(
                    "bare_hunk_insertion_position_required",
                    "cannot determine insertion position for a context-less add-only hunk with a bare @@ header",
                    path,
                    "provide @@ -start,0 +start,count @@ or add context lines around the insertion",
                ));
            }
            (0, old_actual, 0, new_actual)
        }
    };

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

/// Parsed unified diff hunk header. `Full` carries declared ranges; `Bare`
/// covers `@@` and `@@ <description>` headers whose counts derive from the
/// hunk body and whose matcher runs without a line hint.
enum HunkHeader {
    Full {
        old_start: usize,
        old_count: usize,
        new_start: usize,
        new_count: usize,
    },
    Bare,
}

/// `@@` or `@@ <description>`; also the hunk-body boundary predicate.
fn is_hunk_header_line(line: &str) -> bool {
    line == "@@" || line.starts_with("@@ ")
}

fn parse_hunk_header(line: &str, path: Option<&str>) -> Result<HunkHeader> {
    // A header that attempts a range section (leading `@@ -` or a closing
    // ` @@` marker) must parse as a full header; malformed range attempts
    // keep failing closed instead of degrading into descriptions.
    if !line.starts_with("@@ -") && !line.contains(" @@") {
        if !is_hunk_header_line(line) {
            return Err(syntax_error(
                "invalid_hunk_header",
                "expected @@ -old_start,old_count +new_start,new_count @@",
                path,
                "use a standard unified diff hunk header",
            ));
        }
        return Ok(HunkHeader::Bare);
    }
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
    Ok(HunkHeader::Full {
        old_start,
        old_count,
        new_start,
        new_count,
    })
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

fn apply_file_patches<W>(
    workspace_root: &Path,
    patches: &[FilePatch],
    read_text: fn(&Path) -> Result<std::io::Result<String>>,
    begin_write: impl FnOnce(Vec<PathBuf>) -> Result<W>,
) -> Result<(
    Vec<ApplyPatchChangedFile>,
    Vec<String>,
    Vec<ApplyPatchIgnoredMetadata>,
    Vec<ApplyPatchDiagnostic>,
)> {
    let mut write_paths = Vec::new();
    for patch in patches {
        let path_bytes = patch.old_path.as_workspace_path().map_or(0, str::len)
            + patch.new_path.as_workspace_path().map_or(0, str::len)
            + patch.rename_from.as_ref().map_or(0, String::len)
            + patch.rename_to.as_ref().map_or(0, String::len)
            + workspace_root.as_os_str().len();
        let line_bytes = patch
            .hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .map(|line| line.text.len())
            .sum::<usize>();
        let line_count = patch
            .hunks
            .iter()
            .map(|hunk| hunk.lines.len())
            .sum::<usize>();
        // Original receipt/diagnostic/diff construction happens before writes.
        charge_lines(
            path_bytes
                .saturating_mul(16)
                .saturating_add(line_bytes.saturating_mul(4))
                .saturating_add(8192),
            line_count.saturating_mul(4).saturating_add(128),
        )?;
        for path in [
            patch.old_path.as_workspace_path(),
            patch.new_path.as_workspace_path(),
            patch.rename_from.as_deref(),
            patch.rename_to.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            let resolved = resolve_patch_path(workspace_root, path)?;
            write_paths.push(normalize_path(&resolved)?);
        }
    }
    let _writer = begin_write(write_paths)?;

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
                let target = resolve_patch_path(workspace_root, path)?;
                let existing = load_state(&target, &mut state, &mut originals, read_text)?;
                if existing
                    .as_ref()
                    .is_some_and(|state| !state.lines.is_empty())
                {
                    return Err(existing_file(path));
                }
                let updated = apply_hunks(
                    path,
                    FileState {
                        lines: Vec::new(),
                        trailing_newline: false,
                    },
                    &patch.hunks,
                    &mut diagnostics,
                )?;
                state.insert(target, Some(updated));
                changed_files.push(apply_patch_changed_file(
                    ApplyPatchAction::Add,
                    path,
                    None,
                    patch,
                ));
            }
            PatchOperationKind::Delete { path } => {
                let target = resolve_patch_path(workspace_root, path)?;
                let existing = load_state(&target, &mut state, &mut originals, read_text)?
                    .ok_or_else(|| missing_file("delete", path, &target, workspace_root))?;
                if !patch.hunks.is_empty() {
                    let _ = apply_hunks(path, existing, &patch.hunks, &mut diagnostics)?;
                }
                state.insert(target, None);
                changed_files.push(apply_patch_changed_file(
                    ApplyPatchAction::Delete,
                    path,
                    None,
                    patch,
                ));
            }
            PatchOperationKind::Modify { path } => {
                let target = resolve_patch_path(workspace_root, path)?;
                let loaded = load_state(&target, &mut state, &mut originals, read_text)?;
                // A missing modify target whose hunks are all pure-add
                // (`@@ -0,0 +1,N @@`, no context/remove lines) is an add by
                // another spelling; tolerate it instead of hard-failing.
                let tolerated_as_add = loaded.is_none() && patch.hunks.iter().all(is_pure_add_hunk);
                let existing = match loaded {
                    Some(existing) => existing,
                    None if tolerated_as_add => FileState {
                        lines: Vec::new(),
                        trailing_newline: false,
                    },
                    None => {
                        return Err(missing_file("update", path, &target, workspace_root));
                    }
                };
                if tolerated_as_add {
                    diagnostics.push(ApplyPatchDiagnostic {
                        path: path.to_string(),
                        kind: "modify_to_missing_treated_as_add".to_string(),
                        message: format!(
                            "modify target {path} is missing and all hunks are pure-add; applied as file creation"
                        ),
                    });
                }
                let updated = apply_hunks(path, existing, &patch.hunks, &mut diagnostics)?;
                state.insert(target, Some(updated));
                changed_files.push(apply_patch_changed_file(
                    if tolerated_as_add {
                        ApplyPatchAction::Add
                    } else {
                        ApplyPatchAction::Modify
                    },
                    path,
                    None,
                    patch,
                ));
            }
            PatchOperationKind::Rename {
                from,
                to,
                with_edit,
            } => {
                let source = resolve_patch_path(workspace_root, from)?;
                let target = resolve_patch_path(workspace_root, to)?;
                let existing = load_state(&source, &mut state, &mut originals, read_text)?
                    .ok_or_else(|| missing_file("rename", from, &source, workspace_root))?;
                let target_existing = load_state(&target, &mut state, &mut originals, read_text)?;
                if target_existing.is_some() {
                    return Err(existing_file(to));
                }
                let final_state = if with_edit {
                    apply_hunks(from, existing, &patch.hunks, &mut diagnostics)?
                } else {
                    existing
                };
                state.insert(source, None);
                state.insert(target, Some(final_state));
                changed_files.push(apply_patch_changed_file(
                    ApplyPatchAction::Move,
                    to,
                    Some(from),
                    patch,
                ));
            }
        }
    }

    let mut removals = Vec::new();
    let mut writes = Vec::new();
    let mut changed = BTreeSet::new();

    for (path, final_state) in state {
        resource_material::charge_owned_metadata((path.as_os_str().len() as u64 + 256) * 4, 8)?;
        let original_state = originals.remove(&path).unwrap_or(None);
        match (original_state, final_state) {
            (Some(_), None) => {
                removals.push(path.clone());
                changed.insert(path.display().to_string());
            }
            (None, Some(state)) | (Some(_), Some(state)) => {
                writes.push((path.clone(), state.into_content()?));
                changed.insert(path.display().to_string());
            }
            (None, None) => {}
        }
    }

    for path in &removals {
        if path.try_exists().unwrap_or(false) {
            std::fs::remove_file(path)
                .with_context(|| format!("failed to remove {}", path.display()))?;
        }
    }

    for (path, content) in writes {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        std::fs::write(&path, content.as_bytes())
            .with_context(|| format!("failed to write {}", path.display()))?;
    }

    Ok((
        changed_files,
        changed.into_iter().collect(),
        ignored_metadata,
        diagnostics,
    ))
}

fn apply_patch_changed_file(
    action: ApplyPatchAction,
    path: &str,
    from_path: Option<&str>,
    patch: &FilePatch,
) -> ApplyPatchChangedFile {
    let (diff_preview, diff_truncated) = normalized_diff_preview(patch);
    ApplyPatchChangedFile {
        action,
        path: path.to_string(),
        from_path: from_path.map(ToString::to_string),
        hunks: patch.hunks.iter().map(apply_patch_hunk_summary).collect(),
        diff_preview,
        diff_truncated,
    }
}

fn apply_patch_hunk_summary(hunk: &PatchHunk) -> ApplyPatchHunkSummary {
    ApplyPatchHunkSummary {
        old_start: hunk.old_start.try_into().unwrap_or(u32::MAX),
        old_count: hunk.old_count.try_into().unwrap_or(u32::MAX),
        new_start: hunk.new_start.try_into().unwrap_or(u32::MAX),
        new_count: hunk.new_count.try_into().unwrap_or(u32::MAX),
        added_count: hunk
            .lines
            .iter()
            .filter(|line| line.kind == HunkLineKind::Add)
            .count()
            .try_into()
            .unwrap_or(u32::MAX),
        removed_count: hunk
            .lines
            .iter()
            .filter(|line| line.kind == HunkLineKind::Remove)
            .count()
            .try_into()
            .unwrap_or(u32::MAX),
    }
}

fn is_pure_add_hunk(hunk: &PatchHunk) -> bool {
    hunk.lines.iter().all(|line| line.kind == HunkLineKind::Add)
}

fn normalized_diff_preview(patch: &FilePatch) -> (Option<String>, bool) {
    let mut lines = Vec::new();
    lines.push(format!("--- {}", diff_header_path(&patch.old_path, 'a')));
    lines.push(format!("+++ {}", diff_header_path(&patch.new_path, 'b')));

    for hunk in &patch.hunks {
        lines.push(format!(
            "@@ -{},{} +{},{} @@",
            hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count
        ));
        for line in &hunk.lines {
            let prefix = match line.kind {
                HunkLineKind::Context => ' ',
                HunkLineKind::Add => '+',
                HunkLineKind::Remove => '-',
            };
            lines.push(format!("{prefix}{}", line.text));
        }
        if hunk.no_newline_at_end {
            lines.push(r"\ No newline at end of file".to_string());
        }
    }

    let total = lines.len();
    let truncated = total > DIFF_PREVIEW_MAX_LINES;
    if truncated {
        lines.truncate(DIFF_PREVIEW_MAX_LINES);
        lines.push(format!(
            "... diff truncated: showing {} of {} lines",
            DIFF_PREVIEW_MAX_LINES, total
        ));
    }

    if lines.is_empty() {
        (None, false)
    } else {
        (Some(lines.join("\n")), truncated)
    }
}

fn diff_header_path(path: &PatchPath, prefix: char) -> String {
    match path {
        PatchPath::Workspace(path) => format!("{prefix}/{path}"),
        PatchPath::DevNull => "/dev/null".to_string(),
    }
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

fn load_state(
    path: &Path,
    state: &mut HashMap<PathBuf, Option<FileState>>,
    originals: &mut HashMap<PathBuf, Option<FileState>>,
    read_text: fn(&Path) -> Result<std::io::Result<String>>,
) -> Result<Option<FileState>> {
    if let Some(existing) = state.get(path) {
        return existing.as_ref().map(FileState::try_clone).transpose();
    }

    let loaded = match read_text(path)? {
        Ok(content) => Some(FileState::from_content(&content)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()))
        }
    };
    resource_material::charge_owned_metadata((path.as_os_str().len() as u64 + 256) * 2, 2)?;
    state.insert(
        path.to_path_buf(),
        loaded.as_ref().map(FileState::try_clone).transpose()?,
    );
    originals.insert(
        path.to_path_buf(),
        loaded.as_ref().map(FileState::try_clone).transpose()?,
    );
    Ok(loaded)
}

fn old_block_of(lines: &[HunkLine]) -> Vec<String> {
    lines
        .iter()
        .filter(|line| line.kind != HunkLineKind::Add)
        .map(|line| line.text.clone())
        .collect()
}

fn new_block_of(lines: &[HunkLine]) -> Vec<String> {
    lines
        .iter()
        .filter(|line| line.kind != HunkLineKind::Remove)
        .map(|line| line.text.clone())
        .collect()
}

/// #2439: build an alternative hunk-line set with doubled diff-marker
/// prefixes stripped (`-- text` as Remove of `text`, `+- text` / `++ text`
/// as Add of `text`). Returns `None` when nothing changes, so strict
/// interpretation keeps priority.
fn doubled_prefix_alternative(lines: &[HunkLine]) -> Option<Vec<HunkLine>> {
    let mut changed = false;
    let mut alternative = Vec::with_capacity(lines.len());
    for line in lines {
        match line.kind {
            HunkLineKind::Remove if line.text.starts_with("- ") => {
                alternative.push(HunkLine {
                    kind: HunkLineKind::Remove,
                    text: line.text["- ".len()..].to_string(),
                });
                changed = true;
            }
            HunkLineKind::Add if line.text.starts_with("- ") || line.text.starts_with("+ ") => {
                let text = if let Some(rest) = line.text.strip_prefix("+ ") {
                    rest.to_string()
                } else {
                    line.text["- ".len()..].to_string()
                };
                alternative.push(HunkLine {
                    kind: HunkLineKind::Add,
                    text,
                });
                changed = true;
            }
            _ => alternative.push(line.clone()),
        }
    }
    if changed {
        Some(alternative)
    } else {
        None
    }
}

fn apply_hunks(
    path: &str,
    mut state: FileState,
    hunks: &[PatchHunk],
    diagnostics: &mut Vec<ApplyPatchDiagnostic>,
) -> Result<FileState> {
    for hunk in hunks {
        // Original/alternative blocks, match-index vectors, relaxed-line rebuild,
        // and splice capacity are admitted from the actual file and hunk sizes.
        let hunk_bytes = hunk.lines.iter().map(|line| line.text.len()).sum::<usize>();
        charge_lines(
            hunk_bytes
                .checked_mul(16)
                .ok_or_else(|| anyhow::anyhow!("patch hunk material overflow"))?,
            hunk.lines
                .len()
                .checked_mul(16)
                .ok_or_else(|| anyhow::anyhow!("patch hunk records overflow"))?,
        )?;
        charge_lines(state.lines.iter().map(String::len).sum(), state.lines.len())?;
        let mut effective_lines = hunk.lines.clone();
        let mut old_block = old_block_of(&effective_lines);
        let mut new_block = new_block_of(&effective_lines);

        let index = if old_block.is_empty() {
            hunk.old_start.saturating_sub(1).min(state.lines.len())
        } else {
            let m = match find_unique_match(path, &state.lines, &old_block, hunk.old_start) {
                Ok(m) => m,
                // #2439: DSL-style doubled prefixes (`-- x`, `+- x`) parse as
                // Remove/Add of `- x`. When strict matching fails, retry once
                // with the doubled marker stripped. Legitimate edits of
                // marker-initial content (`-- item` removing `- item`) still
                // match strictly first, so they are unaffected.
                Err(strict_error) => {
                    let alternative = doubled_prefix_alternative(&hunk.lines)
                        .filter(|alt| old_block_of(alt) != old_block);
                    match alternative.and_then(|alt| {
                        find_unique_match(path, &state.lines, &old_block_of(&alt), hunk.old_start)
                            .ok()
                            .map(|m| (alt, m))
                    }) {
                        Some((alt, m)) => {
                            diagnostics.push(ApplyPatchDiagnostic {
                                path: path.to_string(),
                                kind: "double_prefix_reinterpreted".to_string(),
                                message: "hunk context matched only after stripping a doubled diff-marker prefix (e.g. `-- text` treated as removing `text`); keep diff markers single-character unless the target line itself starts with the marker".to_string(),
                            });
                            effective_lines = alt;
                            old_block = old_block_of(&effective_lines);
                            new_block = new_block_of(&effective_lines);
                            m
                        }
                        None => return Err(strict_error),
                    }
                }
            };
            if m.relaxed_level.is_some() {
                // Relaxed match: rebuild new_block so context lines use the
                // file's actual content, preserving whitespace exactly.
                // Added lines keep LLM content; removed lines advance offset.
                let mut rebuilt = Vec::with_capacity(new_block.len());
                let mut file_offset = 0usize;
                for line in &effective_lines {
                    match line.kind {
                        HunkLineKind::Context => {
                            rebuilt.push(state.lines[m.index + file_offset].clone());
                            file_offset += 1;
                        }
                        HunkLineKind::Add => {
                            rebuilt.push(line.text.clone());
                        }
                        HunkLineKind::Remove => {
                            file_offset += 1;
                        }
                    }
                }
                new_block = rebuilt;
            }
            m.index
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
) -> Result<UniqueMatch> {
    if needle.len() > lines.len() {
        return Err(context_not_found(path, needle, lines, hint));
    }

    if let Some((window_start, window_end)) = hint_match_window(lines.len(), needle.len(), hint) {
        let window_matches = find_exact_matches(lines, needle, window_start, window_end);
        if window_matches.len() == 1 {
            return Ok(UniqueMatch::exact(window_matches[0]));
        }
    }

    let matches = find_exact_matches(lines, needle, 0, lines.len() - needle.len());
    match matches.len() {
        0 => find_relaxed_unique(path, lines, needle, hint),
        1 => Ok(UniqueMatch::exact(matches[0])),
        _ => find_relaxed_unique(path, lines, needle, hint),
    }
}

fn hint_match_window(line_count: usize, needle_len: usize, hint: usize) -> Option<(usize, usize)> {
    if hint == 0 || needle_len > line_count {
        return None;
    }
    let max_start = line_count - needle_len;
    let hint_index = hint - 1;
    if hint_index > max_start {
        return None;
    }

    // Keep hint matching local, while giving larger hunks proportionally more drift tolerance.
    let radius = needle_len.saturating_mul(2).max(20);
    Some((
        hint_index.saturating_sub(radius),
        hint_index.saturating_add(radius).min(max_start),
    ))
}

fn find_exact_matches(
    lines: &[String],
    needle: &[String],
    start: usize,
    end_inclusive: usize,
) -> Vec<usize> {
    let mut matches = Vec::new();
    for idx in start..=end_inclusive {
        if lines[idx..idx + needle.len()] == *needle {
            matches.push(idx);
        }
    }
    matches
}

/// Result of a unique context-block match. When `relaxed_level` is `Some`,
/// the match was found via whitespace-normalized comparison and the caller
/// must rebuild context lines from the file's actual content.
struct UniqueMatch {
    index: usize,
    relaxed_level: Option<u8>,
}

impl UniqueMatch {
    fn exact(index: usize) -> Self {
        Self {
            index,
            relaxed_level: None,
        }
    }
}

/// After exact matching fails, try progressively more aggressive whitespace
/// normalization. Each level must produce a unique match to be accepted;
/// multiple matches or zero matches advance to the next level.
fn find_relaxed_unique(
    path: &str,
    lines: &[String],
    needle: &[String],
    hint: usize,
) -> Result<UniqueMatch> {
    let end = lines.len() - needle.len();
    for level in 1u8..=3 {
        let matches = find_relaxed_matches(lines, needle, 0, end, level);
        if matches.len() == 1 {
            return Ok(UniqueMatch {
                index: matches[0],
                relaxed_level: Some(level),
            });
        }
    }
    // Re-run level 3 to distinguish 0 matches (context_not_found) from
    // multiple matches (ambiguous_context).
    let matches = find_relaxed_matches(lines, needle, 0, end, 3);
    if matches.is_empty() {
        Err(context_not_found(path, needle, lines, hint))
    } else {
        Err(ambiguous_context(path, needle, lines, &matches, hint))
    }
}

/// Compare lines after normalizing whitespace according to `level`:
/// 1 = `trim_end`, 2 = `trim`, 3 = `split_whitespace().join(" ")`.
fn find_relaxed_matches(
    lines: &[String],
    needle: &[String],
    start: usize,
    end_inclusive: usize,
    level: u8,
) -> Vec<usize> {
    let normalize = |s: &str| -> String {
        match level {
            1 => s.trim_end().to_string(),
            2 => s.trim().to_string(),
            _ => s.split_whitespace().collect::<Vec<_>>().join(" "),
        }
    };
    let norm_needle: Vec<String> = needle.iter().map(|s| normalize(s)).collect();
    let mut matches = Vec::new();
    for idx in start..=end_inclusive {
        let norm_lines: Vec<String> = lines[idx..idx + needle.len()]
            .iter()
            .map(|s| normalize(s))
            .collect();
        if norm_lines == norm_needle {
            matches.push(idx);
        }
    }
    matches
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
    syntax_error_at(kind, message, path, None, recovery_hint)
}

fn syntax_error_at(
    kind: &'static str,
    message: impl Into<String>,
    path: Option<&str>,
    line: Option<usize>,
    recovery_hint: impl Into<String>,
) -> anyhow::Error {
    let mut details = serde_json::json!({ "rule": kind });
    if let Some(path) = path {
        details["path"] = serde_json::Value::String(path.to_string());
    }
    if let Some(line) = line {
        details["line"] = serde_json::Value::from(line);
    }
    anyhow::Error::from(
        ToolError::new("invalid_patch_syntax", message)
            .with_details(details)
            .with_recovery_hint(recovery_hint.into()),
    )
}

fn unexpected_patch_sentinel(line: usize) -> anyhow::Error {
    syntax_error_at(
        "unexpected_patch_sentinel",
        "unified diff contains a standalone patch sentinel that is not allowed in this position",
        None,
        Some(line),
        "submit only the unified diff content and remove standalone wrapper or sentinel lines",
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

struct ContextCandidate {
    start: usize,
    matched: usize,
}

/// #2741: find the position whose following lines share the longest exact
/// prefix with `needle`; ties prefer the position closest to `hint`. Only
/// runs on the failure path, and positions whose first line differs are
/// skipped in O(1).
fn best_context_candidate(
    lines: &[String],
    needle: &[String],
    hint: usize,
) -> Option<ContextCandidate> {
    if lines.is_empty() || needle.is_empty() {
        return None;
    }
    let hint_index = hint.saturating_sub(1);
    let mut best: Option<ContextCandidate> = None;
    for start in 0..lines.len() {
        if lines[start] != needle[0] {
            continue;
        }
        let limit = (lines.len() - start).min(needle.len());
        let mut matched = 0usize;
        while matched < limit && lines[start + matched] == needle[matched] {
            matched += 1;
        }
        let closer_to_hint = |candidate: usize| candidate.abs_diff(hint_index);
        let better = match &best {
            None => true,
            Some(current) => {
                matched > current.matched
                    || (matched == current.matched
                        && closer_to_hint(start) < closer_to_hint(current.start))
            }
        };
        if better {
            best = Some(ContextCandidate { start, matched });
        }
    }
    best
}

fn context_not_found(
    path: &str,
    needle: &[String],
    lines: &[String],
    hint: usize,
) -> anyhow::Error {
    let hint_index = hint.saturating_sub(1).min(lines.len().saturating_sub(1));
    let candidate = best_context_candidate(lines, needle, hint);
    let divergence_index = candidate
        .as_ref()
        .map(|candidate| candidate.start + candidate.matched);
    let (window_start, window_end) = if lines.is_empty() {
        (0, 0)
    } else if hint <= 1 && divergence_index.is_some() {
        // #2741 A2: a hunk without a usable line number (DSL hunks declare
        // old_start=1) used to always show the file head nearby. Center the
        // window on the best candidate's divergence point instead.
        let index = divergence_index.unwrap_or_default();
        (index.saturating_sub(5), (index + 6).min(lines.len()))
    } else {
        let window_start = hint_index.saturating_sub(5);
        let window_end = (hint_index + 6).min(lines.len());
        (window_start, window_end)
    };
    let nearby_lines = lines
        .get(window_start..window_end)
        .unwrap_or(&[])
        .iter()
        .enumerate()
        .map(|(offset, line)| {
            serde_json::json!({
                "line_number": window_start + offset + 1,
                "text": line,
            })
        })
        .collect::<Vec<_>>();
    let nearby_preview = nearby_lines
        .iter()
        .map(|entry| {
            format!(
                "{}: {}",
                entry["line_number"].as_u64().unwrap_or_default(),
                entry["text"].as_str().unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let markdown_list_prefix_suspected =
        suspected_markdown_list_prefix_loss(needle, lines, window_start, window_end);
    let mut details = serde_json::json!({
        "path": path,
        "expected_lines": needle.iter().take(3).cloned().collect::<Vec<_>>().join("\\n"),
        "nearby_range": {
            "start": if lines.is_empty() { 0 } else { window_start + 1 },
            "end": window_end,
        },
        "nearby_lines": nearby_lines,
    });
    let mut divergence_note = String::new();
    if let Some(candidate) = &candidate {
        let divergence_index = candidate.start + candidate.matched;
        let expected: Vec<&String> = needle.iter().skip(candidate.matched).take(3).collect();
        let actual: Vec<&String> = lines
            .get(divergence_index..(divergence_index + 3).min(lines.len()))
            .unwrap_or(&[])
            .iter()
            .collect();
        let expected_preview = needle.get(candidate.matched).cloned().unwrap_or_default();
        let actual_preview = lines
            .get(divergence_index)
            .cloned()
            .unwrap_or_else(|| "<end of file>".to_string());
        details["first_divergence"] = serde_json::json!({
            "candidate_line": candidate.start + 1,
            "matched_prefix_lines": candidate.matched,
            "file_line": divergence_index + 1,
            "expected": expected,
            "actual": actual,
        });
        divergence_note = format!(
            " Closest match: line {} matched the first {} context line(s), then diverged at line {}: expected {:?} but the file has {:?}.",
            candidate.start + 1,
            candidate.matched,
            divergence_index + 1,
            expected_preview,
            actual_preview
        );
    } else if hint <= 1 && !lines.is_empty() {
        // #2741 A2: say explicitly that no similar region exists instead of
        // implying the file head is the target region.
        details["candidate_search"] = serde_json::Value::String("none".to_string());
        divergence_note = format!(
            " No region in {path} matches even the first context line and this hunk carries no usable line number, so the nearby lines show the file head rather than the target region. Re-read the target region before retrying."
        );
    }
    let markdown_list_hint = "If you are editing Markdown list lines, keep the diff prefix separate from the list marker: context ` - item`, deletion `-- item`, addition `+- item`.";
    let recovery_hint = if markdown_list_prefix_suspected {
        details["suspected_issue"] =
            serde_json::Value::String("markdown_list_diff_prefix".to_string());
        format!(
            "read the exact target region in {path} and retry with explicit diff prefixes. {markdown_list_hint}{divergence_note} Nearby current content:\n{nearby_preview}"
        )
    } else {
        format!(
            "read the exact target region in {path} and submit a hunk with matching context.{divergence_note} Nearby current content:\n{nearby_preview}"
        )
    };

    anyhow::Error::from(
        ToolError::new(
            "context_not_found",
            format!("hunk context does not match current file: {path}"),
        )
        .with_details(details)
        .with_recovery_hint(recovery_hint),
    )
}

fn suspected_markdown_list_prefix_loss(
    needle: &[String],
    lines: &[String],
    window_start: usize,
    window_end: usize,
) -> bool {
    let nearby = lines.get(window_start..window_end).unwrap_or(&[]);
    needle.iter().any(|expected| {
        if !expected.starts_with(' ') {
            return false;
        }
        let candidate = format!("-{expected}");
        candidate.starts_with("- ") && nearby.iter().any(|line| line == &candidate)
    })
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

fn path_escape(path: &str, resolved: &Path, workspace_root: &Path) -> anyhow::Error {
    anyhow::Error::from(
        ToolError::new(
            "path_escape",
            format!("patch path escapes workspace root: {path}"),
        )
        .with_details(serde_json::json!({
            "requested_path": path,
            "path": path,
            "resolved_path": resolved.display().to_string(),
            "active_workspace_root": workspace_root.display().to_string(),
            "cwd": std::env::current_dir()
                .map(|cwd| cwd.display().to_string())
                .unwrap_or_else(|_| "<unavailable>".to_string()),
        }))
        .with_recovery_hint(
            "use a normalized relative path inside the current workspace, or use an absolute path only when that exact filesystem target is intended",
        ),
    )
}

fn missing_file(action: &str, path: &str, resolved: &Path, workspace_root: &Path) -> anyhow::Error {
    anyhow::Error::from(
        ToolError::new(
            "missing_file",
            format!("cannot {action} missing file {path}"),
        )
        .with_details(serde_json::json!({
            "action": action,
            "requested_path": path,
            "path": path,
            "resolved_path": resolved.display().to_string(),
            "active_workspace_root": workspace_root.display().to_string(),
            "cwd": std::env::current_dir()
                .map(|cwd| cwd.display().to_string())
                .unwrap_or_else(|_| "<unavailable>".to_string()),
        }))
        .with_recovery_hint(format!(
            "read {path} first or adjust the patch so it targets an existing file. If {path} was meant for another workspace, switch workspace first or provide the intended absolute path.{}",
            if action == "update" {
                format!(
                    " If {path} is meant to be a new file, use --- /dev/null + +++ b/{path} headers, or keep hunks purely additive (@@ -0,0 +1,N @@ with only + lines)."
                )
            } else {
                String::new()
            }
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

fn resolve_patch_path(workspace_root: &Path, path: &str) -> Result<PathBuf> {
    let requested = Path::new(path);
    if requested.is_absolute() {
        return normalize_path(requested);
    }

    let normalized_root = normalize_path(workspace_root)?;
    let candidate = normalized_root.join(requested);
    let normalized_candidate = normalize_path(&candidate)?;
    if !normalized_candidate.starts_with(&normalized_root) {
        return Err(path_escape(path, &normalized_candidate, &normalized_root));
    }

    // Detect the common LLM mistake of prefixing an absolute path with `a/`.
    // strip_diff_prefix turns `a/home/user/foo.rs` into the relative path
    // `home/user/foo.rs`. If that relative file does not exist in the workspace
    // but the absolute form `/{path}` does, return a hint error instead of
    // letting it fail later with a confusing context_not_found.
    if !normalized_candidate.exists() {
        let absolute_guess = PathBuf::from(format!("/{path}"));
        if absolute_guess.is_file() {
            return Err(anyhow::anyhow!(
                "Path `{path}` not found in workspace `{}`. This looks like an absolute path \
                 that was prefixed with `a/` and stripped to a relative path — \
                 write it as `/{path}` (no `a/` prefix) in the diff header.",
                normalized_root.display()
            ));
        }
    }
    Ok(normalized_candidate)
}

#[cfg(test)]
mod tests;
