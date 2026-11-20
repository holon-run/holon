//! Bounded metadata-only report for offline diagnosis. Never an admission input.
use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::Path,
};

use anyhow::{ensure, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use holon_domain::ids::validate_agent_id_format;

use crate::DaemonConfig;

const FILE_NAME: &str = "quarantine-summary.json";
const MAX_WRITE_BYTES: usize = 32 * 1024;
const MAX_READ_BYTES: u64 = 64 * 1024;
const MAX_ENTRY_BYTES: usize = 512;
const MAX_ENTRIES: usize = 32;
const MAX_BOOTS: usize = 3;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QuarantineStage {
    StorageConstructor,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QuarantineReason {
    ProjectionChanged,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QuarantineEntry {
    pub agent_id: String,
    pub incarnation: u64,
    pub generation: u64,
    pub stage: QuarantineStage,
    pub reason: QuarantineReason,
    pub failed_at: DateTime<Utc>,
}

impl QuarantineEntry {
    fn valid(&self) -> bool {
        validate_agent_id_format(&self.agent_id).is_ok()
            && serde_json::to_vec(self).is_ok_and(|json| json.len() <= MAX_ENTRY_BYTES)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuarantineBoot {
    startup_id: String,
    started_at: DateTime<Utc>,
    entries: Vec<QuarantineEntry>,
    omitted_entries: usize,
}

impl QuarantineBoot {
    pub fn new<'a>(
        startup_id: &str,
        started_at: DateTime<Utc>,
        entries: impl IntoIterator<Item = &'a QuarantineEntry>,
    ) -> Self {
        let mut boot = Self {
            startup_id: startup_id.to_owned(),
            started_at,
            entries: Vec::new(),
            omitted_entries: 0,
        };
        for entry in entries {
            if boot.entries.len() < MAX_ENTRIES && entry.valid() {
                boot.entries.push(entry.clone());
            } else {
                boot.omitted_entries += 1;
            }
        }
        boot
    }

    fn valid(&self) -> bool {
        self.startup_id.len() <= 80
            && self.startup_id.starts_with("startup_")
            && validate_agent_id_format(&self.startup_id).is_ok()
            && self.entries.len() <= MAX_ENTRIES
            && self.entries.iter().all(QuarantineEntry::valid)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuarantineSummary {
    version: u8,
    current: QuarantineBoot,
    history: Vec<QuarantineBoot>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QuarantineReportUnavailable {
    Missing,
    ReadFailed,
    NotPrivateRegularFile,
    Oversized,
    Invalid,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum QuarantineReport {
    Available { summary: QuarantineSummary },
    Unavailable { reason: QuarantineReportUnavailable },
}

pub fn read_quarantine_report(config: &dyn DaemonConfig) -> QuarantineReport {
    match read_summary(&config.run_dir().join(FILE_NAME)) {
        Ok(summary) => QuarantineReport::Available { summary },
        Err(reason) => QuarantineReport::Unavailable { reason },
    }
}

fn read_summary(
    path: &Path,
) -> std::result::Result<QuarantineSummary, QuarantineReportUnavailable> {
    use QuarantineReportUnavailable::*;

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => Missing,
        _ => ReadFailed,
    })?;
    let metadata = file.metadata().map_err(|_| ReadFailed)?;
    if !metadata.is_file() {
        return Err(NotPrivateRegularFile);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(NotPrivateRegularFile);
        }
    }
    if metadata.len() > MAX_READ_BYTES {
        return Err(Oversized);
    }
    let mut data = Vec::new();
    file.take(MAX_READ_BYTES + 1)
        .read_to_end(&mut data)
        .map_err(|_| ReadFailed)?;
    if data.len() as u64 > MAX_READ_BYTES {
        return Err(Oversized);
    }
    let summary: QuarantineSummary = serde_json::from_slice(&data).map_err(|_| Invalid)?;
    if summary.version != 1
        || !summary.current.valid()
        || summary.history.len() >= MAX_BOOTS
        || summary.history.iter().any(|boot| !boot.valid())
    {
        return Err(Invalid);
    }
    Ok(summary)
}

/// The host serializes writes and only calls this while owning the daemon.
/// A diagnostic failure must not change admission or discard in-memory state.
pub fn persist_quarantine_boot(run_dir: &Path, current: QuarantineBoot) -> Result<()> {
    ensure!(current.valid(), "invalid quarantine boot summary");
    let path = run_dir.join(FILE_NAME);
    let mut history = match read_summary(&path) {
        Ok(old) if old.current.startup_id == current.startup_id => old.history,
        Ok(old) => std::iter::once(old.current)
            .chain(old.history)
            .take(MAX_BOOTS - 1)
            .collect(),
        Err(_) => Vec::new(),
    };
    let mut current = current;
    let data = loop {
        let summary = QuarantineSummary {
            version: 1,
            current,
            history,
        };
        let data = serde_json::to_vec(&summary)?;
        if data.len() <= MAX_WRITE_BYTES {
            break data;
        }
        QuarantineSummary {
            current,
            history,
            ..
        } = summary;
        let oldest = history.last_mut().unwrap_or(&mut current);
        if oldest.entries.pop().is_some() {
            oldest.omitted_entries += 1;
        } else if history.pop().is_none() {
            anyhow::bail!("quarantine report metadata exceeds its size limit");
        }
    };
    fs::create_dir_all(run_dir)?;
    let mut temp = tempfile::NamedTempFile::new_in(run_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temp.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    temp.write_all(&data)?;
    temp.as_file().sync_all()?;
    temp.persist(path)?;
    #[cfg(unix)]
    fs::File::open(run_dir)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests;
