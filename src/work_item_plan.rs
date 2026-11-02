use std::{
    fs,
    fs::File,
    io::ErrorKind,
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) enum PlanArtifactQuery {
    NotRecorded,
    Missing,
    Available(WorkItemPlanArtifact),
    Unreadable(String),
}

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
    let path = admitted_plan_path(agent_home, &record.id)?;
    if !path.exists() {
        // Admit the entire first-file kernel before creating even its directory.
        let body = initial_plan.unwrap_or_default().as_bytes();
        crate::diagnostics::resource_io::admit_owned_metadata(
            plan_owned_metadata_bytes(&path, &record.agent_id, &record.id, body.len() as u64)
                .saturating_add((body.len() as u64).saturating_mul(2))
                .saturating_add(4096),
            32,
        )?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        fs::write(&path, body).with_context(|| format!("failed to write {}", path.display()))?;
    }
    describe_plan_artifact(&path, &record.agent_id, &record.id)
}

pub(crate) fn refresh_plan_artifact_metadata(
    agent_home: &Path,
    record: &mut WorkItemRecord,
) -> Result<bool> {
    let path = admitted_plan_path(agent_home, &record.id)?;
    let previous = record.plan_artifact.clone();
    if !path.exists() && record.plan_artifact.is_some() {
        anyhow::bail!(
            "missing plan artifact {} for work item {}",
            path.display(),
            record.id
        );
    }
    let artifact = ensure_plan_artifact(agent_home, record, None)?;
    record.plan_artifact = Some(artifact);
    Ok(record.plan_artifact != previous)
}

/// Loaded observation never creates files or converts a failed read into absence.
pub(crate) fn observe_plan_artifact(
    agent_home: &Path,
    owner_agent_id: &str,
    work_item_id: &str,
    recorded: bool,
) -> Result<PlanArtifactQuery> {
    let path = admitted_plan_path(agent_home, work_item_id)?;
    let file = match crate::diagnostics::io_probe::open_file(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok(if recorded {
                PlanArtifactQuery::Missing
            } else {
                PlanArtifactQuery::NotRecorded
            });
        }
        Err(error) => {
            return Err(error).with_context(|| format!("unreadable plan {}", path.display()))
        }
    };
    Ok(PlanArtifactQuery::Available(describe_open_plan_artifact(
        file,
        &path,
        owner_agent_id,
        work_item_id,
    )?))
}

pub(crate) fn query_plan_artifact(agent_home: &Path, record: &WorkItemRecord) -> PlanArtifactQuery {
    let path = match admitted_plan_path(agent_home, &record.id) {
        Ok(path) => path,
        Err(error) => return PlanArtifactQuery::Unreadable(error.to_string()),
    };
    let file = match crate::diagnostics::io_probe::open_file(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return if record.plan_artifact.is_some() {
                PlanArtifactQuery::Missing
            } else {
                PlanArtifactQuery::NotRecorded
            };
        }
        Err(error) => {
            return PlanArtifactQuery::Unreadable(format!(
                "plan artifact unreadable at {}: {}",
                path.display(),
                error
            ));
        }
    };

    match describe_open_plan_artifact(file, &path, &record.agent_id, &record.id) {
        Ok(artifact) => PlanArtifactQuery::Available(artifact),
        Err(error) => PlanArtifactQuery::Unreadable(format!(
            "plan artifact unreadable at {}: {}",
            path.display(),
            error
        )),
    }
}

fn admitted_plan_path(agent_home: &Path, work_item_id: &str) -> Result<PathBuf> {
    crate::diagnostics::resource_io::check_active()?;
    crate::diagnostics::resource_io::charge_owned_metadata(
        (agent_home.as_os_str().len() as u64)
            .saturating_add(work_item_id.len() as u64)
            .saturating_add(512),
        1,
    )?;
    Ok(plan_path(agent_home, work_item_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{WorkItemRecord, WorkItemState};
    use tempfile::tempdir;

    #[test]
    fn query_plan_artifact_reports_not_recorded_when_metadata_and_file_are_absent() {
        let agent_home = tempdir().unwrap();
        let record = WorkItemRecord::new("default", "without a plan", WorkItemState::Open);

        assert_eq!(
            query_plan_artifact(agent_home.path(), &record),
            PlanArtifactQuery::NotRecorded
        );
    }

    #[test]
    fn loaded_observation_never_creates_an_unrecorded_or_missing_plan() {
        let home = tempdir().unwrap();
        for recorded in [false, true] {
            assert_eq!(
                observe_plan_artifact(home.path(), "default", "work-a", recorded).unwrap(),
                if recorded {
                    PlanArtifactQuery::Missing
                } else {
                    PlanArtifactQuery::NotRecorded
                },
            );
            assert!(!home.path().join("work-items").exists());
        }
    }
}

pub(crate) fn describe_plan_artifact(
    path: &Path,
    owner_agent_id: &str,
    work_item_id: &str,
) -> Result<WorkItemPlanArtifact> {
    let file = crate::diagnostics::io_probe::open_file(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    describe_open_plan_artifact(file, path, owner_agent_id, work_item_id)
}

fn describe_open_plan_artifact(
    mut file: File,
    path: &Path,
    owner_agent_id: &str,
    work_item_id: &str,
) -> Result<WorkItemPlanArtifact> {
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to stat {}", path.display()))?;
    let owned_bytes = plan_owned_metadata_bytes(path, owner_agent_id, work_item_id, metadata.len());
    crate::diagnostics::resource_io::charge_owned_metadata(owned_bytes, 8)?;
    crate::diagnostics::resource_io::admit_file_bytes(metadata.len())?;
    #[cfg(test)]
    crate::diagnostics::resource_io::run_after_file_metadata();
    let mut content = Vec::new();
    crate::diagnostics::io_probe::read_to_end(&mut file, &mut content)
        .with_context(|| format!("failed to read {}", path.display()))?;
    // A concurrently growing file must also admit its larger lossy text/preview.
    let actual_owned_bytes =
        plan_owned_metadata_bytes(path, owner_agent_id, work_item_id, content.len() as u64);
    crate::diagnostics::resource_io::charge_owned_metadata(
        actual_owned_bytes.saturating_sub(owned_bytes),
        0,
    )?;
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
        bytes: content.len() as u64,
        updated_at,
        preview,
        preview_complete,
    })
}

fn plan_owned_metadata_bytes(
    path: &Path,
    owner_agent_id: &str,
    work_item_id: &str,
    body_bytes: u64,
) -> u64 {
    // Descriptor, both owner copies, paths, hash, alias, worst-case UTF-8
    // replacement text and the bounded Unicode preview.
    (std::mem::size_of::<WorkItemPlanArtifact>() as u64)
        .saturating_add(512)
        .saturating_add((owner_agent_id.len() as u64).saturating_mul(2))
        .saturating_add(work_item_id.len() as u64)
        .saturating_add(path.as_os_str().len() as u64)
        .saturating_add(body_bytes.saturating_mul(3))
        .saturating_add(body_bytes.min(PLAN_PREVIEW_CHARS as u64).saturating_mul(4))
}
