use std::{
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

use crate::types::{WorkItemPlanArtifact, WorkItemRecord};

const PLAN_PREVIEW_CHARS: usize = 1600;

pub(crate) fn plan_path(agent_home: &Path, work_item_id: &str) -> PathBuf {
    agent_home
        .join("work-items")
        .join(work_item_id)
        .join("plan.md")
}

pub(crate) fn ensure_plan_artifact(
    agent_home: &Path,
    record: &WorkItemRecord,
    initial_plan: Option<&str>,
) -> Result<WorkItemPlanArtifact> {
    let path = plan_path(agent_home, &record.id);
    if !path.exists() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let body = initial_plan
            .or(record.plan.as_deref())
            .unwrap_or_default()
            .as_bytes();
        fs::write(&path, body).with_context(|| format!("failed to write {}", path.display()))?;
    }
    describe_plan_artifact(&path)
}

pub(crate) fn describe_plan_artifact(path: &Path) -> Result<WorkItemPlanArtifact> {
    let content = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let metadata =
        fs::metadata(path).with_context(|| format!("failed to stat {}", path.display()))?;
    let hash = format!("sha256:{:x}", Sha256::digest(&content));
    let text = String::from_utf8_lossy(&content);
    let preview = text.chars().take(PLAN_PREVIEW_CHARS).collect::<String>();
    let preview_complete = text.chars().count() <= PLAN_PREVIEW_CHARS;
    let updated_at = metadata
        .modified()
        .ok()
        .map(DateTime::<Utc>::from)
        .unwrap_or_else(|| DateTime::<Utc>::from(SystemTime::UNIX_EPOCH));
    Ok(WorkItemPlanArtifact {
        path: path.to_path_buf(),
        hash,
        bytes: metadata.len(),
        updated_at,
        preview,
        preview_complete,
    })
}
