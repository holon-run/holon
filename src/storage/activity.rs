//! File and poll activity markers for storage freshness checks.

use std::{fs, path::Path, time::UNIX_EPOCH};

use anyhow::{Context, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileActivityMarker {
    pub exists: bool,
    pub len: u64,
    pub modified_unix_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollActivityMarker {
    pub briefs: FileActivityMarker,
    pub tasks: FileActivityMarker,
    pub tools: FileActivityMarker,
    pub events: FileActivityMarker,
    pub transcript: FileActivityMarker,
}

pub(crate) fn file_activity_marker(path: &Path) -> Result<FileActivityMarker> {
    if !path.exists() {
        return Ok(FileActivityMarker {
            exists: false,
            len: 0,
            modified_unix_ms: 0,
        });
    }

    let metadata =
        fs::metadata(path).with_context(|| format!("failed to stat {}", path.display()))?;
    let modified_unix_ms = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis())
        .unwrap_or(0);

    Ok(FileActivityMarker {
        exists: true,
        len: metadata.len(),
        modified_unix_ms,
    })
}
