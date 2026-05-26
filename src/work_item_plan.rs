use std::{
    fs,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

use crate::types::{
    agent_home_workspace_id, WorkItemPlanArtifact, WorkItemRecord, AGENT_HOME_WORKSPACE_ID,
};

const PLAN_PREVIEW_CHARS: usize = 1600;

pub(crate) fn plan_path(agent_home: &Path, work_item_id: &str) -> PathBuf {
    agent_home
        .join("work-items")
        .join(work_item_id)
        .join("plan.md")
}

pub(crate) fn plan_relative_path(work_item_id: &str) -> PathBuf {
    PathBuf::from("work-items")
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
            .or(record.legacy_inline_plan.as_deref())
            .unwrap_or_default()
            .as_bytes();
        fs::write(&path, body).with_context(|| format!("failed to write {}", path.display()))?;
    }
    describe_plan_artifact(&path, &record.agent_id, &record.id)
}

pub(crate) fn refresh_plan_artifact_metadata(
    agent_home: &Path,
    record: &mut WorkItemRecord,
) -> Result<bool> {
    let previous = record.plan_artifact.clone();
    let had_inline_plan = record.legacy_inline_plan.is_some();
    let path = plan_path(agent_home, &record.id);
    if !path.exists() && record.legacy_inline_plan.is_none() && record.plan_artifact.is_some() {
        anyhow::bail!(
            "missing plan artifact {} for work item {}",
            path.display(),
            record.id
        );
    }
    let artifact = ensure_plan_artifact(agent_home, record, None)?;
    record.legacy_inline_plan = None;
    record.plan_artifact = Some(artifact);
    Ok(had_inline_plan || record.plan_artifact != previous)
}

pub(crate) fn describe_plan_artifact(
    path: &Path,
    owner_agent_id: &str,
    work_item_id: &str,
) -> Result<WorkItemPlanArtifact> {
    let mut file =
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to stat {}", path.display()))?;
    let mut content = Vec::new();
    file.read_to_end(&mut content)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let hash = format!("sha256:{:x}", Sha256::digest(&content));
    let text = String::from_utf8_lossy(&content);
    let mut chars = text.chars();
    let preview = chars.by_ref().take(PLAN_PREVIEW_CHARS).collect::<String>();
    let preview_complete = chars.next().is_none();
    let updated_at = metadata
        .modified()
        .ok()
        .map(DateTime::<Utc>::from)
        .unwrap_or_else(|| DateTime::<Utc>::from(SystemTime::UNIX_EPOCH));
    Ok(WorkItemPlanArtifact {
        owner_agent_id: owner_agent_id.to_string(),
        workspace_id: agent_home_workspace_id(owner_agent_id),
        workspace_alias: Some(AGENT_HOME_WORKSPACE_ID.into()),
        relative_path: plan_relative_path(work_item_id),
        path: path.to_path_buf(),
        hash,
        bytes: metadata.len(),
        updated_at,
        preview,
        preview_complete,
    })
}
