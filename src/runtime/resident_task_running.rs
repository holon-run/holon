//! Fixed Running associations are versioned metadata, never Input body grants.
use super::{CanonicalTaskFact, CapturedTasks, QualifiedTaskFact, TaskFamily};
use crate::{
    runtime::resident_input_evidence::QualifiedInputEvidence,
    types::{
        AdmissionContext, MessageDeliverySurface, MessageEnvelope, MessageKind, MessageOrigin,
        TaskKind, TaskRecord, TaskRecoverySpec, TaskStatus,
    },
};
use anyhow::{ensure, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io::Write};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RunningSourceGeneration {
    pub source_id: String,
    pub source_digest: String,
    pub task_id: String,
    pub created_at: DateTime<Utc>,
    pub generation_digest: String,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TaskRunningSourceCoverage {
    pub sources: BTreeMap<String, RunningSourceGeneration>,
}

/// Hash borrowed original generation fields without copying arbitrary recovery/detail.
pub(crate) fn task_running_generation_digest(task: &TaskRecord) -> Result<String> {
    struct HashWriter(Sha256);
    impl Write for HashWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let detail = task.detail.as_ref();
    let has_markers = ["rejoin_obligation_id", "rejoin_generation"]
        .into_iter()
        .any(|key| detail.and_then(|value| value.get(key)).is_some());
    let rejoin = has_markers.then(|| {
        [
            "rejoin_obligation_id",
            "rejoin_generation",
            "parent_turn_id",
        ]
        .map(|key| {
            let field = detail.and_then(|value| value.get(key));
            (field.is_some(), field)
        })
    });
    let mut writer = HashWriter(Sha256::new());
    serde_json::to_writer(
        &mut writer,
        &(
            &task.id,
            &task.agent_id,
            &task.kind,
            task.created_at,
            &task.work_item_id,
            &task.recovery,
            rejoin,
        ),
    )?;
    Ok(format!("{:x}", writer.0.finalize()))
}

fn valid_running_rejoin(task: &TaskRecord) -> bool {
    let field = |key: &str| task.detail.as_ref().and_then(|value| value.get(key));
    if field("rejoin_obligation_id").is_none() && field("rejoin_generation").is_none() {
        return true;
    }
    field("rejoin_obligation_id").and_then(serde_json::Value::as_str) == Some(task.id.as_str())
        && field("rejoin_generation")
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|value| value > 0)
        && match field("parent_turn_id") {
            None => true,
            Some(serde_json::Value::String(value)) => !value.is_empty(),
            Some(_) => false,
        }
}

/// Only an original bounded cutover collector uses this predicate. Its caller
/// separately proves fixed Task + Input membership before publishing metadata.
pub(crate) fn fixed_running_matches_task(
    task: &TaskRecord,
    source: &MessageEnvelope,
) -> Result<bool> {
    if !valid_running_rejoin(task)
        || source.kind != MessageKind::TaskStatus
        || !matches!(
            task.status,
            TaskStatus::Queued | TaskStatus::Running | TaskStatus::Cancelling
        )
        || !(task.kind.is_child_agent()
            || matches!(
                task.kind,
                TaskKind::ActorInvocation | TaskKind::AgentMessageWait
            ))
    {
        return Ok(false);
    }
    let Some(metadata) = source
        .metadata
        .as_ref()
        .and_then(serde_json::Value::as_object)
    else {
        return Ok(false);
    };
    // New sources must use their explicit expected header, never legacy capture.
    if metadata.contains_key("task_created_at") {
        return Ok(false);
    }
    let kind: Option<TaskKind> = match metadata
        .get("task_kind")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
    {
        Ok(value) => value,
        Err(_) => return Ok(false),
    };
    let recovery: Option<TaskRecoverySpec> = match metadata
        .get("task_recovery")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
    {
        Ok(value) => value,
        Err(_) => return Ok(false),
    };
    let original = metadata.get("task_detail");
    let has_markers = ["rejoin_obligation_id", "rejoin_generation"]
        .into_iter()
        .any(|key| {
            original.and_then(|value| value.get(key)).is_some()
                || task
                    .detail
                    .as_ref()
                    .and_then(|value| value.get(key))
                    .is_some()
        });
    let exact_rejoin = !has_markers
        || [
            "rejoin_obligation_id",
            "rejoin_generation",
            "parent_turn_id",
        ]
        .into_iter()
        .all(|key| {
            original.and_then(|value| value.get(key))
                == task.detail.as_ref().and_then(|value| value.get(key))
        });
    Ok(source.agent_id == task.agent_id
        && source.task_id.as_deref() == Some(task.id.as_str())
        && matches!(&source.origin, MessageOrigin::Task { task_id } if task_id == &task.id)
        && source.delivery_surface == Some(MessageDeliverySurface::TaskRejoin)
        && source.admission_context == Some(AdmissionContext::RuntimeOwned)
        && metadata.get("task_id").and_then(serde_json::Value::as_str) == Some(task.id.as_str())
        && kind.as_ref() == Some(&task.kind)
        && metadata
            .get("task_status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("running")
            == "running"
        && source.work_item_id == task.work_item_id
        && recovery == task.recovery
        && exact_rejoin
        && task.created_at <= source.created_at)
}

impl CapturedTasks {
    /// Association is useful only alongside the actual independently qualified
    /// Source and current Task. It grants no new Task/Message/Turn membership.
    pub(crate) fn fixed_running_generation_matches(
        &self,
        task: &QualifiedTaskFact,
        source: &QualifiedInputEvidence,
    ) -> Result<bool> {
        task.validate(&self.scope)?;
        ensure!(
            task.locator.key.family == TaskFamily::Task
                && source.locator.scope == self.scope
                && source.locator.key.family
                    == crate::runtime::resident_input_evidence::InputEvidenceFamily::Message,
            "Running association crossed scope/family"
        );
        let CanonicalTaskFact::Task(record) = &task.fact else {
            return Ok(false);
        };
        let Some(observation) = self.coverage.running.sources.get(&source.locator.key.id) else {
            return Ok(false);
        };
        Ok(valid_running_rejoin(record)
            && observation.source_id == source.locator.key.id
            && observation.source_digest == source.locator.content_digest
            && observation.task_id == record.id
            && observation.created_at == record.created_at
            && observation.generation_digest == task_running_generation_digest(record)?)
    }
}
